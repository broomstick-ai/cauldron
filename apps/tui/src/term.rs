//! **Terminal ownership.** Everything this program takes from the terminal,
//! and every way it gives it back.
//!
//! `0023` `philosophy` §1 — *the app owns its surface, wholly* — is a CHECK,
//! not an adjective, and this module is where it is checkable: **the DECSET
//! set requested at entry is exactly [`CLAIMS`]**, one array, with each claim
//! paired with the key that releases it. Nothing else in this crate writes a
//! DEC private mode; `grep '\\x1b\[?'` over `apps/tui/src` finds this file and
//! no other.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;

pub type Tui = Terminal<CrosstermBackend<io::Stdout>>;

/// One thing this program takes from the terminal.
pub struct Claim {
    /// The DECSET sequence that takes it.
    pub set: &'static str,
    /// The DECRST sequence that gives it back. A mode RESET, not a toggle —
    /// which is what makes [`restore`] safe to call from a panic hook without
    /// the hook knowing how far entry got.
    pub reset: &'static str,
    pub what: &'static str,
    /// The binding, in the key table, that hands this back while the face is
    /// up. `None` means the only release is exit — which is itself a binding
    /// (`q`), so every claim has a discoverable door.
    pub release_key: Option<&'static str>,
}

/// **The complete set, in the order it is requested.**
///
/// [`restore`] resets them in REVERSE, which is not an aesthetic choice: it
/// puts the mouse release before the alternate-screen exit. Both are DEC
/// private modes and neither is scoped to a screen buffer, so the order is
/// about which failure survives a partial restore. A terminal left on the
/// alternate screen is obvious and one keystroke from fixed; a terminal left
/// in mouse-reporting mode looks FINE until he clicks, and then every click
/// types garbage into his shell. The worse failure goes first.
///
/// **`?1002h` IS CLAIMED AND `?1000h` IS NOT, and that one line is the whole
/// of `IS-21`'s mechanism.**
///
/// `0036` measured every mouse-claiming pane in his live tmux session:
///
/// ```text
///   cmd=tui        any=1 std=1 btn=0 all=0 sgr=1   <-- ours, before this
///   cmd=python3.12 any=1 std=0 btn=0 all=1 sgr=1   <-- $ threads (Textual)
///   cmd=claude     any=1 std=0 btn=0 all=1 sgr=1   <-- Claude Code
/// ```
///
/// **Ours was the only app on the machine holding `?1000h` alone** — press and
/// release, with NO motion events — and `?1000h` alone is a dead zone under
/// tmux: `mouse_any_flag` is set either way, so tmux forwards the drag to the
/// application and does no selection itself, and the application then cannot
/// see the motion it was forwarded. Neither side selected. That is his bug
/// report, mechanically, and it is why he could drag-select in every other
/// pane and not in this one.
///
/// `?1002h` is `?1000h` PLUS motion while a button is held — the same press
/// and release reports, so nothing that worked before changes, and the motion
/// reports without which a drag cannot be seen, let alone painted.
///
/// **Why not `?1003h`.** They are byte-identical for a drag (`0036` probe
/// `p2`, rows 3 and 4). `?1003h` additionally reports every pointer move with
/// NO button held, which is a full repaint per pixel of trackpad travel
/// through a loop that redraws on every event it does not ignore — the storm
/// `0018` measured. `?1002h` reports motion **only while a button is down**,
/// so the repaint law absorbs nothing.
///
/// **And the upgrade is free at the terminal boundary**, measured rather than
/// assumed (`0036` probes `p1`/`p5`, on a captured outer pty): with `mouse on`
/// set, tmux emits `?1006h ?1000h ?1002h` toward iTerm2 whether the pane app
/// claims nothing, `?1000h`, or `?1002h`. The outbound byte stream is
/// identical in all three cases, so no other pane and no terminal setting can
/// tell that this changed.
///
/// **`?2004h` IS CLAIMED SINCE THE COMPOSER LANDED, and it is the difference
/// between a paste and a keystroke storm.** Without it a paste arrives as N
/// ordinary key events with nothing marking where it began or ended — so it
/// cannot be one undo step, it cannot be normalized as a unit, and, worst,
/// the carriage return at the end of a pasted paragraph is indistinguishable
/// from him pressing `enter`, which SENDS. `input::defuse` carries a backstop
/// for terminals that ignore the claim, but the claim is the mechanism.
///
/// `?1004h` (focus reporting) is still absent because nothing consumes it.
///
/// **`?2026` — synchronized output — is deliberately NOT a row here**, and its
/// absence is a statement about what this table means rather than an omission.
/// Every claim in it is a mode HELD for the life of the session and handed back
/// at exit. `?2026` is a begin/end pair written around ONE frame and closed by
/// the same function that opened it; carrying it here would make the table's
/// promise ("these modes are set right now") false between frames. [`restore`]
/// emits its reset anyway — see [`FRAME_END`] — because the one way it can be
/// left open is a panic in the middle of a draw.
pub const CLAIMS: &[Claim] = &[
    Claim {
        set: "\x1b[?1049h",
        reset: "\x1b[?1049l",
        what: "the alternate screen — so nothing this face draws reaches the terminal's scrollback",
        release_key: Some("q"),
    },
    Claim {
        // ratatui's `Terminal::draw` hides the cursor itself on its first
        // frame and shows it again in `Drop`. It is claimed HERE anyway, and
        // that is not redundancy — it is the table refusing to be incomplete.
        // A mode this program causes to be set while not appearing in this
        // list makes the enumeration a claim rather than a fact, and it is
        // also a mode whose RELEASE would then depend on a `Drop` that a panic
        // can skip: `restore` resets it explicitly for exactly that reason.
        set: "\x1b[?25l",
        reset: "\x1b[?25h",
        what: "the cursor, hidden — the face draws its own caret and a terminal caret parked in the corner reads as a hang",
        release_key: Some("q"),
    },
    Claim {
        set: "\x1b[?1002h",
        reset: "\x1b[?1002l",
        what: "mouse button press/release AND motion while a button is held — a wheel notch IS a button event (64/65), and the motion is what makes a drag visible at all",
        release_key: Some("q"),
    },
    Claim {
        set: "\x1b[?1006h",
        reset: "\x1b[?1006l",
        what: "SGR extended coordinates — the legacy encoding cannot express a column past 223, and his window is routinely wider",
        release_key: Some("q"),
    },
    Claim {
        set: "\x1b[?2004h",
        reset: "\x1b[?2004l",
        what: "bracketed paste — so a paste is ONE event that can be normalized and undone whole, rather than a burst of keystrokes whose trailing carriage return would SEND",
        release_key: Some("^C"),
    },
];

/// Written before every frame, and closed by [`FRAME_END`] after it.
///
/// DECSET 2026, synchronized output: the terminal holds its screen still until
/// the end marker, so a frame in which the composer band GREW or SHRANK — every
/// band below it moving — arrives as one picture instead of as a tear. His
/// client already negotiates it (`sync` is in `#{client_termfeatures}`), so it
/// costs one escape pair per frame and nothing else, and a terminal that does
/// not know the mode ignores both halves.
///
/// **It does NOT replace the repaint law.** `0018`'s finding stands exactly as
/// written: a differential renderer has no way back from a screen something
/// else moved, so `needs_full_repaint` still fires on resize, on the link
/// changing hands, on a clipboard write and on `^L`.
pub const FRAME_BEGIN: &str = "\x1b[?2026h";

/// Closes [`FRAME_BEGIN`]. Also emitted by [`restore`], unconditionally, for
/// the one case that can leave it open: a panic between the two.
pub const FRAME_END: &str = "\x1b[?2026l";

/// Enter: raw mode, then every claim in [`CLAIMS`], in order.
///
/// Raw mode is termios, not a DECSET, which is why it is not a row in that
/// table — the table's promise is about the DEC private modes this program
/// asks the terminal for, and padding it with a non-DECSET would blur exactly
/// the thing it is there to make enumerable.
///
/// One consequence of raw mode worth naming where it happens: with `ISIG` off,
/// Ctrl-Z never becomes a `SIGTSTP`, so this program cannot be suspended from
/// its own keyboard. Suspend/resume is therefore NOT one of the repaint
/// set-points in `app.rs`; if a future build re-enables it, it becomes one.
pub fn enter() -> anyhow::Result<Tui> {
    enable_raw_mode().context("enable raw mode")?;
    let mut out = io::stdout();
    for claim in CLAIMS {
        out.write_all(claim.set.as_bytes())
            .with_context(|| format!("claim {}", claim.what))?;
        // The claim set, in the log, with its release. `0023` §1 asks for it
        // to be "exactly enumerable"; this makes it enumerable from a running
        // session and not only from the source.
        tracing::info!(
            sequence = claim.set.escape_debug().to_string(),
            release = claim.release_key.unwrap_or("exit only"),
            "claimed: {}",
            claim.what
        );
    }
    out.flush().context("flush terminal claims")?;
    Terminal::new(CrosstermBackend::new(out)).context("construct terminal")
}

/// Put the terminal back the way it was found. Idempotent, infallible, and
/// callable from a panic hook.
///
/// Sending a reset for a mode that was never set is harmless — it is a mode
/// reset, not a toggle — which is what keeps this callable from the panic hook
/// without the hook having to know what state entry reached.
pub fn restore() {
    let mut out = io::stdout();
    // FIRST, unconditionally: end any synchronized-output frame. A panic
    // between `FRAME_BEGIN` and `FRAME_END` leaves the terminal holding its
    // screen, and everything below — including the backtrace this hook exists
    // to let him read — would be written into a picture nobody is shown.
    let _ = out.write_all(FRAME_END.as_bytes());
    // …and stop asking for enhanced keys, in both dialects, before the modes
    // go. Neither is in `CLAIMS` (see `negotiate`), so neither would be reset
    // by the loop below, and a terminal left in `modifyOtherKeys` mode 2 sends
    // his SHELL re-encoded control keys afterwards.
    let _ = out.write_all(MODIFY_OTHER_KEYS_OFF.as_bytes());
    if KITTY_PUSHED.load(Ordering::SeqCst) {
        let _ = out.write_all(KITTY_POP.as_bytes());
    }
    for claim in CLAIMS.iter().rev() {
        let _ = out.write_all(claim.reset.as_bytes());
    }
    let _ = out.flush();
    let _ = disable_raw_mode();
}

// ── the key negotiation ─────────────────────────────────────────────────────

/// `CSI > 4 ; 2 m` — xterm's `modifyOtherKeys` mode 2, which is what makes a
/// terminal report `shift-enter` as something other than `enter`.
const MODIFY_OTHER_KEYS_ON: &str = "\x1b[>4;2m";
/// `CSI > 4 ; 0 m` — and it is sent on EVERY exit path whether or not it was
/// ever turned on, for the same reason every `Claim` is a mode RESET and not a
/// toggle.
const MODIFY_OTHER_KEYS_OFF: &str = "\x1b[>4;0m";
/// `CSI ? u` — "what keyboard-enhancement flags are in effect?". A terminal
/// that speaks the kitty protocol answers `CSI ? <flags> u`; one that does not
/// says nothing at all, which is the answer too.
const KITTY_QUERY: &str = "\x1b[?u";
/// The kitty keyboard protocol's push, used only outside tmux.
const KITTY_PUSH: &str = "\x1b[>1u";
const KITTY_POP: &str = "\x1b[<u";

static KITTY_PUSHED: AtomicBool = AtomicBool::new(false);

/// **WHAT THIS STACK CAN ACTUALLY TELL US — AND `IS-33` IS CLOSED HERE.**
///
/// # What the constraint WAS, and why this function used to refuse
///
/// crossterm parses the kitty `CSI <code> ; <mods> u` form and **has no arm at
/// all for xterm's `CSI 27 ; <mods> ; <code> ~`** — on 0.28.1 and on master. On
/// a parse failure its `Parser::advance` calls `self.buffer.clear()`, so those
/// bytes were gone before any code of ours existed.
///
/// tmux decides which of those two encodings it emits with the SERVER option
/// `extended-keys-format`, whose default is `xterm` — the one crossterm drops.
/// So asking tmux for extended keys on a default-configured server bought DEAD
/// KEYS, and this function used to read the option and request nothing unless
/// the answer was `csi-u`. On his desk that meant `shift-enter` was
/// byte-identical to `enter` and therefore SENT.
///
/// # What changed: we parse the bytes now
///
/// `0050` took the file descriptor. [`crate::vt`] reads BOTH dialects — the
/// xterm `CSI 27 ; m ; c ~` form and the kitty `CSI c ; m u` form — and a unit
/// test asserts the two decode to the same event for every modifier. So the
/// question this function used to ask has no consequences any more, and asking
/// it would only be a way to keep a working key switched off.
///
/// **Inside tmux, `modifyOtherKeys` is therefore requested unconditionally and
/// the capability is reported as available.** That is the whole of `IS-33`'s
/// close, and it needs nothing from his configuration: the option is not read,
/// not changed, and not depended on.
///
/// # Outside tmux it is still a probe, and still honest
///
/// There is no reliable way to ask a bare terminal whether it implements
/// `modifyOtherKeys` — the request has no reply, and a terminal that ignores it
/// leaves `shift-enter` arriving as a plain `\r`, which SENDS. Advertising a
/// key on that guess would put the `IS-33` defect back on a different stack. So
/// outside tmux the kitty probe still decides, and a terminal that answers no
/// still gets `enhanced_keys: false` and the withheld-key note in the `?`
/// overlay.
///
/// **It reads nothing of his config now.** Every branch produces a working
/// face.
pub fn negotiate() -> tui_render::Caps {
    let mut caps = tui_render::Caps {
        // `?2004h` is in CLAIMS and is requested unconditionally. A terminal
        // that ignores it is covered by `input::defuse`'s burst backstop, so
        // this is reported as claimed rather than as confirmed — there is no
        // reply to a DECSET and nothing here may pretend otherwise.
        bracketed_paste: true,
        enhanced_keys: false,
        // **WHAT THIS HOST CALLS THE MODIFIER, AND IT IS DETECTED, NOT
        // ASSUMED.** His words: *"alt- doesn't exist on my mac"*, and on the
        // fix, *"make sure it's operating system agnostic."* So the descriptor
        // comes from a TABLE over the host name rather than from a
        // `#[cfg(target_os = "macos")]` branch — the same binding, correctly
        // named, on whatever this is running on. `std::env::consts::OS` is the
        // host the binary is running on; it is the only signal available here,
        // and a terminal attached from a different kind of machine is a case
        // nothing on this wire can tell us about.
        mods: tui_render::ModKeys::for_host(std::env::consts::OS),
    };
    let mut out = io::stdout();
    if std::env::var_os("TMUX").is_some() {
        let _ = out.write_all(MODIFY_OTHER_KEYS_ON.as_bytes());
        let _ = out.flush();
        caps.enhanced_keys = true;
        tracing::info!(
            format = tmux_option("extended-keys-format")
                .as_deref()
                .unwrap_or("unreadable"),
            "modifyOtherKeys requested; vt reads both dialects, so the format is \
             logged as a fact and not acted on (IS-33 closed)"
        );
        return caps;
    }
    // **OUTSIDE TMUX THE PROBE IS ASKED HERE AND ANSWERED SOMEWHERE ELSE, and
    // that is a deliberate replacement for a blocking call.**
    //
    // This used to be `crossterm::terminal::supports_keyboard_enhancement()`,
    // which writes `CSI ? u` and then BLOCKS READING STDIN for up to two
    // seconds waiting for the reply — `reedline#132` and `brush#1329` are both
    // "the app hung at startup" against exactly that. It ran on a worker thread
    // under a 250 ms cap of our own, and the cap made it look safe.
    //
    // It was not, once `0050` took the descriptor. **A capped wait does not
    // cancel the read**: the worker sat in `read()` on stdin long after this
    // function returned, and when the input pump's own reader started there
    // were TWO THREADS ON ONE FD. Measured on a pty: the probe thread ate the
    // first keystroke of the session, every time.
    //
    // So the query is written and nothing waits for it. The answer comes back
    // through the ONE reader as `vt::Out::EnhancedKeys`, the loop turns the
    // capability on when it arrives, and a terminal that cannot answer simply
    // never does — which is the same verdict the timeout used to reach, with no
    // timeout, no second reader and no startup cost at all.
    let _ = out.write_all(KITTY_QUERY.as_bytes());
    let _ = out.flush();
    tracing::info!("kitty keyboard query sent; the capability lights up if it is answered");
    caps
}

/// **THE TERMINAL ANSWERED THE KITTY QUERY.** Push the flags that make it
/// report `shift-enter` apart from `enter`, and record the push so [`restore`]
/// pops it.
///
/// Called from the event loop rather than from [`negotiate`] because the answer
/// arrives as an event — see the comment in `negotiate` for the blocking probe
/// this replaced and the byte-stealing race that made it necessary.
pub fn push_kitty() {
    // Idempotent: a terminal is free to answer twice, and pushing twice would
    // leave a stack entry that `restore`'s single pop cannot reach.
    if KITTY_PUSHED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout();
    let _ = out.write_all(KITTY_PUSH.as_bytes());
    let _ = out.flush();
    tracing::info!("kitty keyboard protocol negotiated");
}

/// One tmux option, read once. `None` when there is no tmux to ask or it would
/// not answer — which is a different thing from an option that is set to
/// nothing, and the caller treats it as "do not request".
fn tmux_option(name: &str) -> Option<String> {
    let out = std::process::Command::new("tmux")
        .args(["show-options", "-gv", name])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// **`set_mouse` AND THE `M` KEY ARE GONE.**
//
// `M` handed the mouse back so the TERMINAL could drag-select, and `0018`
// priced that as the honest door paired with taking the mouse. `0036` measured
// the door and found it opened onto nothing: iTerm2's own selection had
// already been suppressed by tmux's `mouse on`, permanently, for every pane —
// so what `M` handed back was tmux's copy-mode, not the terminal's selection,
// and it cost the WHEEL to get there (a wheel notch against a pane claiming no
// mouse puts tmux into copy-mode, which is `IS-12` verbatim and resolved).
//
// The face now selects on every cell it draws. A toggle whose other side is
// worse than this side is not a door, and the escape hatch for a genuinely
// wedged process is the terminal's own modifier-drag — OPTION in iTerm2, and
// NOT Shift, which `IS-26` records as a mitigation that never worked. That
// hatch is checked by iTerm2 before a byte is written to the pty, so tmux
// cannot interfere with it and this program cannot either.

/// **How much text the clipboard path will carry before this face refuses to
/// try — and the number is measured, not guessed.**
///
/// `0036` read tmux's parser: `INPUT_BUF_LIMIT` caps an escape sequence at
/// 1 MiB, the payload is base64 and therefore 4/3 of the text, and the
/// ceiling that falls out is **786,426 raw bytes**. Over that, tmux
/// **DISCARDS THE WHOLE SEQUENCE SILENTLY** — not truncates, discards — so a
/// copy one byte too big produces nothing at all and says nothing about it.
/// iTerm2's own 1 MiB cap TRUNCATES instead, which corrupts rather than loses.
///
/// 700 KB is that ceiling with headroom, and the refusal NAMES it. The old
/// value here was 40,000 and its comment cited xterm's 32 kB
/// `maxStringParseSize` — a real number about a terminal he does not use,
/// applied to a path that does not go through it.
const CLIPBOARD_MAX: usize = 700_000;

/// Which route a copy took to the terminal, for a toast that can be honest
/// about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyPath {
    /// No `$TMUX`: `OSC 52` written straight at the terminal.
    Direct,
    /// Inside tmux, wrapped in the DCS passthrough. The path `0036` measured
    /// reaching iTerm2 under ALL THREE `set-clipboard` settings.
    Passthrough,
    /// Inside tmux with `allow-passthrough` off. The wrapper would be eaten,
    /// so the raw sequence goes instead — and under his `set-clipboard
    /// external` tmux will drop that too. **Said out loud in a toast**: this
    /// is the one case the face cannot make work, and silence here is the
    /// exact defect being fixed.
    RawInTmux,
}

/// What a copy did. `chars` is what was SENT — there is no acknowledgement in
/// the protocol, so nothing here claims it arrived.
#[derive(Debug, Clone)]
pub struct Copied {
    pub chars: usize,
    pub path: CopyPath,
}

impl Copied {
    /// The notice. It says what was done, never what was hoped (honesty rule 6
    /// applied where there is no echo to render).
    pub fn notice(&self) -> String {
        match self.path {
            CopyPath::Direct => format!("{} chars copied (OSC 52)", self.chars),
            CopyPath::Passthrough => {
                format!("{} chars copied (OSC 52 via tmux passthrough)", self.chars)
            }
            CopyPath::RawInTmux => format!(
                "{} chars sent RAW — tmux allow-passthrough is off, and with set-clipboard \
                 external tmux drops a raw OSC 52. The copy probably did not land.",
                self.chars
            ),
        }
    }

    /// Whether the notice is a warning rather than a confirmation.
    pub fn is_warning(&self) -> bool {
        self.path == CopyPath::RawInTmux
    }
}

/// Whether tmux will forward a DCS passthrough, asked ONCE per session.
///
/// **This reads his config; it does not depend on it.** The wrapper is sent
/// whenever `$TMUX` is set, whatever this returns — the answer only decides
/// whether the toast warns him. A face that refused to try because a setting
/// looked wrong would be a face that fails on a stale reading.
fn passthrough_allowed() -> bool {
    use std::sync::OnceLock;
    static ALLOWED: OnceLock<bool> = OnceLock::new();
    *ALLOWED.get_or_init(|| {
        match std::process::Command::new("tmux")
            .args(["show-options", "-gv", "allow-passthrough"])
            .output()
        {
            Ok(o) => {
                let v = String::from_utf8_lossy(&o.stdout).trim().to_string();
                // `on` and `all` both forward; anything else does not. An
                // EMPTY answer is an older tmux with no such option, where
                // passthrough is unconditional — so it counts as allowed.
                v.is_empty() || v == "on" || v == "all"
            }
            // tmux not on PATH, or it refused to answer. Assume the wrapper
            // works rather than warning him about a reading we do not have:
            // a spurious warning on every copy would train him to ignore the
            // one that matters.
            Err(_) => true,
        }
    })
}

/// **THE SYSTEM CLIPBOARD, AND THE LIVE DEFECT THIS REPLACES.**
///
/// The old body wrote `\x1b]52;c;<b64>\x07` at stdout and reported success.
/// **In his configuration that did nothing, every time, silently.** `0036`
/// read it out of tmux's own source: `input_osc_52()` opens with
///
/// ```c
/// state = options_get_number(global_options, "set-clipboard");
/// if (state != 2) return;
/// ```
///
/// State 2 is `on`. **He runs `external`**, so the branch returns and the
/// sequence is dropped before it can reach iTerm2. Measured on the wire
/// (probe `p4`, capturing the outer pty):
///
/// | path | `external` (his) | `on` | `off` |
/// |---|---|---|---|
/// | app writes `OSC 52` raw | **DROPPED** | reaches iTerm2 | dropped |
/// | app writes it via DCS passthrough | **reaches iTerm2** | reaches iTerm2 | **reaches iTerm2** |
///
/// So inside tmux the sequence is wrapped: `\x1bPtmux;` + the `OSC 52` with
/// every `\x1b` DOUBLED + `\x1b\\`. Passthrough bypasses the `set-clipboard`
/// policy entirely, which is why it is the only cell in that table that works
/// under all three settings — **and it is why he is not being asked to change
/// his tmux config.** Nothing in this function requires anything of it.
///
/// **The base64 MUST be padded.** tmux computes `len = (strlen / 4) * 3` and
/// then calls `b64_pton`, which fails outright on unpadded input. The
/// hand-rolled encoder below pads, and the RFC vectors in this module's tests
/// are what say so.
///
/// **What it still cannot promise.** There is no acknowledgement in `OSC 52`
/// — no reply, ever — so this returns what was SENT and by which route, the
/// toast says which, and nothing anywhere claims the copy landed.
pub fn copy_to_clipboard(text: &str) -> Result<Copied, String> {
    if text.is_empty() {
        return Err("nothing to copy".into());
    }
    if text.len() > CLIPBOARD_MAX {
        return Err(format!(
            "too much to copy through the terminal ({} bytes, limit {CLIPBOARD_MAX}) — \
             above ~786 KB tmux discards the whole sequence without a word, so this \
             refuses rather than copying nothing",
            text.len()
        ));
    }
    let payload = base64(text.as_bytes());
    // `c` is the CLIPBOARD selection (as opposed to `p`, the primary/X
    // selection), and BEL terminates rather than ST because more terminals
    // accept it — and because it carries no ESC of its own to double.
    let osc = format!("\x1b]52;c;{payload}\x07");
    let in_tmux = std::env::var_os("TMUX").is_some();
    let (bytes, path) = if in_tmux {
        let wrapped = format!("\x1bPtmux;{}\x1b\\", osc.replace('\x1b', "\x1b\x1b"));
        let path = if passthrough_allowed() {
            CopyPath::Passthrough
        } else {
            CopyPath::RawInTmux
        };
        match path {
            CopyPath::RawInTmux => (osc, path),
            _ => (wrapped, path),
        }
    } else {
        (osc, CopyPath::Direct)
    };
    let mut out = io::stdout();
    out.write_all(bytes.as_bytes())
        .map_err(|e| format!("the terminal would not take the copy: {e}"))?;
    out.flush()
        .map_err(|e| format!("the terminal would not take the copy: {e}"))?;
    Ok(Copied {
        chars: text.chars().count(),
        path,
    })
}

/// The exact bytes [`copy_to_clipboard`] would write, for a test or a probe to
/// read without a terminal in the way.
///
/// Split out so the WRAPPER is checkable as a pure function: the shape of
/// those bytes is the whole fix, and a proof that has to own a pty to look at
/// them is a proof that cannot run in `cargo test`.
pub fn clipboard_bytes(text: &str, in_tmux: bool) -> String {
    let osc = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    if in_tmux {
        format!("\x1bPtmux;{}\x1b\\", osc.replace('\x1b', "\x1b\x1b"))
    } else {
        osc
    }
}

/// Base64, hand-rolled rather than depended on.
///
/// Twelve lines against a crate in the tree forever, for the one place this
/// program needs it — and a dependency in `apps/tui`'s manifest is a
/// dependency a reader has to account for when asking what this face can
/// reach.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Restore BEFORE the default hook prints, so the backtrace lands on the
/// primary screen instead of inside a buffer that is discarded.
///
/// Also restores when the face is KILLED (`SIGTERM`, `SIGHUP`, `SIGINT`,
/// `SIGQUIT`; IS-59): `term_shell::restore_on_signals` runs THIS module's
/// [`restore`] on a watcher thread and exits `128 + signo`. `SIGKILL` cannot be
/// caught, so a `kill -9` still leaves the terminal claimed.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        default(info);
    }));
    if let Err(e) = term_shell::restore_on_signals(restore) {
        tracing::warn!("no restore on kill: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The philosophy check as a test: every claim is paired with a
    /// discoverable release.
    #[test]
    fn every_claim_has_a_release_key() {
        for c in CLAIMS {
            assert!(c.release_key.is_some(), "unreleasable claim: {}", c.what);
        }
    }

    /// The order that survives a partial restore: mouse released before the
    /// alternate screen is left.
    #[test]
    fn restore_order_releases_the_mouse_first() {
        let order: Vec<&str> = CLAIMS.iter().rev().map(|c| c.reset).collect();
        let alt = order.iter().position(|s| s.contains("1049")).unwrap();
        let m1002 = order.iter().position(|s| s.contains("1002")).unwrap();
        let m1006 = order.iter().position(|s| s.contains("1006")).unwrap();
        let cursor = order.iter().position(|s| s.contains("25h")).unwrap();
        assert!(
            m1006 < alt && m1002 < alt,
            "alternate screen left before the mouse: {order:?}"
        );
        assert!(
            m1006 < cursor && m1002 < cursor,
            "the mouse must be released first: {order:?}"
        );
        assert!(
            cursor < alt,
            "the cursor must come back before the screen does: {order:?}"
        );
    }

    /// The table is the WHOLE set, not a selection from it. `?25l` is here
    /// because ratatui issues it, and a mode this program causes without
    /// listing would make the enumeration a claim rather than a fact.
    #[test]
    fn the_cursor_is_in_the_table_because_something_hides_it() {
        assert!(
            CLAIMS.iter().any(|c| c.set.contains("25")),
            "the cursor hide is unlisted — the enumeration is not the set"
        );
    }

    /// **THE CLAIM THAT FIXES `IS-21`, AND THE ONE THAT MUST NEVER COME BACK.**
    ///
    /// `?1002h` is REQUIRED: without it the terminal reports press and release
    /// and no motion, which is the dead zone his drag fell into. `?1003h` is
    /// FORBIDDEN: it adds idle motion with no button held, which is a repaint
    /// per pixel of trackpad travel and buys nothing — the two are
    /// byte-identical for an actual drag (`0036` probe `p2`).
    #[test]
    fn drag_tracking_is_claimed_and_any_motion_tracking_never_is() {
        assert!(
            CLAIMS.iter().any(|c| c.set.contains("1002")),
            "?1002h is not claimed — the face cannot see a drag, which is IS-21"
        );
        for c in CLAIMS {
            assert!(
                !c.set.contains("1003"),
                "any-motion tracking re-entered CLAIMS — that is the repaint storm"
            );
            assert!(
                !c.set.contains("1000"),
                "?1000h is back beside ?1002h; ?1002h already carries press and release, \
                 and the pair is two modes where one will do"
            );
        }
    }

    /// Hand-rolled, so it is tested against the vectors the RFC publishes —
    /// including the two padding cases, which is where every hand-rolled
    /// base64 goes wrong.
    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // …and multi-byte text, which is most of what a transcript holds.
        assert_eq!(base64("日本".as_bytes()), "5pel5pys");
    }

    /// A copy tmux would discard whole is worse than one that did not happen,
    /// so the limit refuses BY NAME rather than sending bytes into a parser
    /// that will drop them without a word.
    #[test]
    fn an_over_long_copy_is_refused_by_name_rather_than_silently_discarded() {
        let big = "x".repeat(CLIPBOARD_MAX + 1);
        let err = copy_to_clipboard(&big).unwrap_err();
        assert!(err.contains("too much"), "{err}");
        assert!(
            err.contains(&CLIPBOARD_MAX.to_string()),
            "the refusal must name the limit: {err}"
        );
        // The measured ceiling tmux discards above (`0036`), as a value so
        // the comparison is not a constant clippy can fold away — the point
        // of the check is that a later edit cannot raise the budget past it.
        let tmux_discards_above: usize = 786_426;
        assert!(
            CLIPBOARD_MAX < tmux_discards_above,
            "the guard is above tmux's measured discard ceiling — it guards nothing"
        );
        assert!(
            copy_to_clipboard("").is_err(),
            "nothing to copy is not a copy"
        );
    }

    /// **THE WRAPPER, AS BYTES.** This is the fix for `IS-27` and it is one
    /// string; checking it needs no terminal, so it is checked here rather
    /// than only on a pty.
    #[test]
    fn inside_tmux_the_sequence_is_wrapped_and_every_escape_is_doubled() {
        let raw = clipboard_bytes("hi", false);
        assert_eq!(raw, "\x1b]52;c;aGk=\x07", "the direct path changed shape");

        let wrapped = clipboard_bytes("hi", true);
        assert!(wrapped.starts_with("\x1bPtmux;"), "{wrapped:?}");
        assert!(wrapped.ends_with("\x1b\\"), "{wrapped:?}");
        // Every ESC of the INNER sequence is doubled; the DCS introducer and
        // the ST are not. tmux un-doubles as it forwards, so what iTerm2 sees
        // is `raw` exactly.
        assert!(
            wrapped.contains("\x1b\x1b]52;c;aGk=\x07"),
            "the inner ESC was not doubled — tmux will eat the sequence: {wrapped:?}"
        );
        let inner = &wrapped["\x1bPtmux;".len()..wrapped.len() - 2];
        assert_eq!(
            inner.replace("\x1b\x1b", "\x1b"),
            raw,
            "un-doubling the wrapper must give back exactly the raw sequence"
        );
    }

    /// **THE PADDING, WHICH IS WHERE THIS FAILS SILENTLY.** tmux computes
    /// `len = (strlen / 4) * 3` and `b64_pton` refuses unpadded input — so an
    /// encoder that dropped `=` would produce a copy that reaches tmux and is
    /// then discarded, which looks exactly like the defect being fixed.
    #[test]
    fn the_payload_is_always_padded_to_a_multiple_of_four() {
        for text in ["a", "ab", "abc", "abcd", "abcde", "日本語"] {
            let seq = clipboard_bytes(text, false);
            let payload = seq
                .trim_start_matches("\x1b]52;c;")
                .trim_end_matches('\x07');
            assert_eq!(
                payload.len() % 4,
                0,
                "{text:?} encoded to {payload:?}, which tmux's b64_pton will reject"
            );
        }
    }
}
