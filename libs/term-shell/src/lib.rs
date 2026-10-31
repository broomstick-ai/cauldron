//! **Terminal ownership.** Everything a face takes from the terminal, and every
//! way it gives it back.
//!
//! Extracted from `harness/apps/tui/src/term.rs` — see `PROVENANCE.md`. The
//! long-form rationale for each claim (measured against tmux, iTerm2 and the
//! other mouse-claiming panes on this machine) lives in that source file; the
//! essentials are kept on each item below.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;

pub type Tui = Terminal<CrosstermBackend<io::Stdout>>;

/// One thing this program takes from the terminal.
#[derive(Debug, Clone, Copy)]
pub struct Claim {
    /// The DECSET sequence that takes it.
    pub set: &'static str,
    /// The DECRST sequence that gives it back. A mode RESET, not a toggle —
    /// which is what makes [`restore`] safe to call from a panic hook without
    /// knowing how far entry got.
    pub reset: &'static str,
    pub what: &'static str,
}

pub const ALT_SCREEN: Claim = Claim {
    set: "\x1b[?1049h",
    reset: "\x1b[?1049l",
    what: "the alternate screen — nothing drawn reaches the terminal's scrollback",
};
pub const HIDE_CURSOR: Claim = Claim {
    set: "\x1b[?25l",
    reset: "\x1b[?25h",
    what: "the cursor, hidden — the face draws its own caret",
};
/// `?1002h`, not `?1000h`: `?1000h` alone is a dead zone under tmux (neither
/// side selects on drag). `?1002h` adds motion only while a button is held, so
/// it does not cause a repaint storm the way `?1003h` would.
pub const MOUSE_BUTTONS: Claim = Claim {
    set: "\x1b[?1002h",
    reset: "\x1b[?1002l",
    what: "mouse press/release and motion while a button is held",
};
pub const MOUSE_SGR: Claim = Claim {
    set: "\x1b[?1006h",
    reset: "\x1b[?1006l",
    what: "SGR extended mouse coordinates (columns past 223)",
};
/// Without bracketed paste, the trailing carriage return of a pasted paragraph
/// is indistinguishable from pressing enter, which SENDS.
pub const BRACKETED_PASTE: Claim = Claim {
    set: "\x1b[?2004h",
    reset: "\x1b[?2004l",
    what: "bracketed paste — a paste is one event, never a keystroke storm",
};

/// **The harness face's complete set, in the order it is requested.**
/// [`restore`] resets in REVERSE so the mouse release precedes the
/// alternate-screen exit: a terminal left in mouse mode looks fine until a
/// click types garbage into the shell, so the worse failure is undone first.
pub const CLAIMS: &[Claim] = &[ALT_SCREEN, HIDE_CURSOR, MOUSE_BUTTONS, MOUSE_SGR, BRACKETED_PASTE];

/// Synchronized output begin/end, written around ONE frame. Not a claim: it is
/// never held across frames. [`restore`] resets it anyway because a panic in
/// the middle of a draw is the one way it can be left open.
pub const FRAME_BEGIN: &str = "\x1b[?2026h";
pub const FRAME_END: &str = "\x1b[?2026l";

/// Enter with the harness's full claim set.
pub fn enter() -> anyhow::Result<Tui> {
    enter_with(CLAIMS)
}

/// Enter with a chosen subset of claims (each must be one of [`CLAIMS`], so
/// that [`restore`] covers it).
pub fn enter_with(claims: &[Claim]) -> anyhow::Result<Tui> {
    enable_raw_mode().context("enable raw mode")?;
    let mut out = io::stdout();
    for claim in claims {
        out.write_all(claim.set.as_bytes())
            .with_context(|| format!("claim {}", claim.what))?;
    }
    out.flush().context("flush terminal claims")?;
    Terminal::new(CrosstermBackend::new(out)).context("construct terminal")
}

/// Give everything back. Idempotent and safe from a panic hook: every reset is
/// a DECRST, which is a no-op for a mode that was never set.
pub fn restore() {
    let mut out = io::stdout();
    let _ = out.write_all(restore_bytes(KITTY_PUSHED.load(Ordering::SeqCst)).as_bytes());
    let _ = out.flush();
    let _ = disable_raw_mode();
}

/// Every byte [`restore`] writes, in order: end any synchronized frame, stop
/// asking for enhanced keys in BOTH dialects (neither is in [`CLAIMS`], so the
/// loop would not reset them, and a terminal left in `modifyOtherKeys` mode 2
/// sends his SHELL re-encoded control keys afterwards), then every claim's
/// reset in reverse. `kitty_pushed` is whether [`push_kitty`] ran.
pub fn restore_bytes(kitty_pushed: bool) -> String {
    let mut s = String::from(FRAME_END);
    s.push_str(MODIFY_OTHER_KEYS_OFF);
    if kitty_pushed {
        s.push_str(KITTY_POP);
    }
    for claim in CLAIMS.iter().rev() {
        s.push_str(claim.reset);
    }
    s
}

// ── the key negotiation (restored R7.S1b from harness `term.rs`) ────────────
//
// Left behind at extraction because it returned `tui_render::Caps` and logged
// through `tracing`. Restored here returning [`KeyCaps`], which a face maps onto
// its own render capabilities, and without the logging. The long-form argument
// (IS-33, why the outside-tmux probe is fire-and-forget, the byte-stealing race
// a blocking probe caused) is on `negotiate` in harness `apps/tui/src/term.rs`
// at `883204b4`, and in `apps/tui/src/term.rs` in this repo.

/// `CSI > 4 ; 2 m` — xterm's `modifyOtherKeys` mode 2: makes a terminal report
/// `shift-enter` as something other than `enter`.
pub const MODIFY_OTHER_KEYS_ON: &str = "\x1b[>4;2m";
/// `CSI > 4 ; 0 m` — sent on EVERY exit path whether or not it was turned on,
/// for the same reason every `Claim` is a mode RESET and not a toggle.
pub const MODIFY_OTHER_KEYS_OFF: &str = "\x1b[>4;0m";
/// `CSI ? u` — "what keyboard-enhancement flags are in effect?". A kitty
/// terminal answers `CSI ? <flags> u`; one that does not says nothing.
pub const KITTY_QUERY: &str = "\x1b[?u";
/// The kitty keyboard protocol's push, used only outside tmux.
pub const KITTY_PUSH: &str = "\x1b[>1u";
pub const KITTY_POP: &str = "\x1b[<u";

static KITTY_PUSHED: AtomicBool = AtomicBool::new(false);

/// What [`negotiate`] could establish about enhanced keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyCaps {
    /// `shift-enter` arrives distinct from `enter`. True inside tmux, where
    /// `modifyOtherKeys` is requested unconditionally (the face's VT parser
    /// reads both the xterm and the kitty dialect). Outside tmux it starts
    /// false and lights up only when the kitty query is ANSWERED — the face's
    /// one input reader sees the reply and calls [`push_kitty`].
    pub enhanced_keys: bool,
}

/// Ask for enhanced keys on stdout. Inside tmux (`$TMUX` set): request
/// `modifyOtherKeys` and report it available. Outside: send the kitty query
/// and wait for NOTHING — a blocking probe here was measured stealing the
/// session's first keystroke.
pub fn negotiate() -> KeyCaps {
    let mut out = io::stdout();
    let caps = negotiate_to(&mut out, std::env::var_os("TMUX").is_some());
    let _ = out.flush();
    caps
}

/// [`negotiate`] against any writer, with the tmux question answered by the
/// caller — the testable half.
pub fn negotiate_to(out: &mut impl Write, in_tmux: bool) -> KeyCaps {
    if in_tmux {
        let _ = out.write_all(MODIFY_OTHER_KEYS_ON.as_bytes());
        return KeyCaps { enhanced_keys: true };
    }
    let _ = out.write_all(KITTY_QUERY.as_bytes());
    KeyCaps { enhanced_keys: false }
}

/// The terminal answered the kitty query: push the flags that make it report
/// `shift-enter` apart from `enter`, and record the push so [`restore`] pops
/// it. Idempotent — a terminal may answer twice, and a second push would leave
/// a stack entry `restore`'s single pop cannot reach. Returns whether THIS call
/// pushed.
pub fn push_kitty() -> bool {
    if KITTY_PUSHED.swap(true, Ordering::SeqCst) {
        return false;
    }
    let mut out = io::stdout();
    let _ = out.write_all(KITTY_PUSH.as_bytes());
    let _ = out.flush();
    true
}

/// Whether [`push_kitty`] has pushed (so [`restore`] will pop).
pub fn kitty_pushed() -> bool {
    KITTY_PUSHED.load(Ordering::SeqCst)
}

/// Restore the terminal before the default panic message prints, so the
/// message lands on a sane screen instead of inside the alternate one. Also
/// installs [`restore_on_signals`] with [`restore`], so a face that calls this
/// before [`enter`] is covered when it is KILLED as well as when it panics.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        default(info);
    }));
    let _ = restore_on_signals(restore);
}

// ── restore on a kill (IS-59) ───────────────────────────────────────────────

/// The signals a face is killed with that CAN be caught: `kill`'s default,
/// a closed pane or terminal, and the two keyboard signals (which raw mode
/// turns off at the keyboard, but `kill -INT` / `-QUIT` still send).
/// **`SIGKILL` (and `SIGSTOP`) cannot be caught by any process**, so a
/// `kill -9` still leaves the terminal in whatever modes the face had claimed;
/// nothing in-process can fix that.
pub const RESTORE_SIGNALS: [i32; 4] = [
    signal_hook::consts::SIGTERM,
    signal_hook::consts::SIGHUP,
    signal_hook::consts::SIGINT,
    signal_hook::consts::SIGQUIT,
];

static SIGNALS_INSTALLED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// **A face killed instead of quit gives the terminal back.** Before this, a
/// `kill` (or a closed tmux pane) skipped both the normal exit path and the
/// panic hook, and left his terminal in mouse reporting and kitty keyboard
/// mode — clicks and keys arrived in his shell as escape-sequence garbage.
///
/// Watches [`RESTORE_SIGNALS`] on a thread (`signal-hook`'s iterator: the
/// handler only records the signal, so no unsafe code and nothing
/// async-signal-unsafe runs in it). On the first one it takes the stdout lock,
/// runs `restore`, and exits `128 + signo` WITHOUT letting go of the lock — so
/// a frame the face is in the middle of drawing, or a kitty push racing the
/// signal, can finish its current write but never reaches the terminal after
/// the restore. A second signal arriving during the restore is absorbed by the
/// same handler and does not cut it short.
///
/// Installs once per process; later calls (whichever face) are no-ops and
/// return `Ok`. `restore` must be idempotent and must not need the stdout lock
/// released (Rust's stdout lock is reentrant, so calling `io::stdout()` inside
/// it is fine). `SIGKILL` cannot be caught — see [`RESTORE_SIGNALS`].
pub fn restore_on_signals(restore: fn()) -> io::Result<()> {
    if SIGNALS_INSTALLED.get().is_some() {
        return Ok(());
    }
    let mut signals = signal_hook::iterator::Signals::new(RESTORE_SIGNALS)?;
    let _ = SIGNALS_INSTALLED.set(());
    std::thread::Builder::new()
        .name("term-shell-signals".into())
        .spawn(move || {
            if let Some(signo) = signals.forever().next() {
                let _held = io::stdout().lock();
                restore();
                std::process::exit(128 + signo);
            }
        })?;
    Ok(())
}

// ── clipboard (OSC 52) ──────────────────────────────────────────────────────

/// Above ~786 KB tmux discards the whole sequence silently, so refuse first.
const CLIPBOARD_MAX: usize = 700_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyPath {
    Direct,
    Passthrough,
    /// In tmux with `allow-passthrough` off: sent raw, probably dropped.
    RawInTmux,
}

#[derive(Debug, Clone)]
pub struct Copied {
    pub chars: usize,
    pub path: CopyPath,
}

impl Copied {
    pub fn notice(&self) -> String {
        match self.path {
            CopyPath::Direct => format!("{} chars copied (OSC 52)", self.chars),
            CopyPath::Passthrough => {
                format!("{} chars copied (OSC 52 via tmux passthrough)", self.chars)
            }
            CopyPath::RawInTmux => format!(
                "{} chars sent RAW — tmux allow-passthrough is off; the copy probably did not land.",
                self.chars
            ),
        }
    }

    pub fn is_warning(&self) -> bool {
        self.path == CopyPath::RawInTmux
    }
}

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
                v.is_empty() || v == "on" || v == "all"
            }
            Err(_) => true,
        }
    })
}

pub fn copy_to_clipboard(text: &str) -> Result<Copied, String> {
    if text.is_empty() {
        return Err("nothing to copy".into());
    }
    if text.len() > CLIPBOARD_MAX {
        return Err(format!(
            "too much to copy through the terminal ({} bytes, limit {CLIPBOARD_MAX})",
            text.len()
        ));
    }
    let in_tmux = std::env::var_os("TMUX").is_some();
    let path = match (in_tmux, in_tmux && passthrough_allowed()) {
        (false, _) => CopyPath::Direct,
        (true, true) => CopyPath::Passthrough,
        (true, false) => CopyPath::RawInTmux,
    };
    let bytes = clipboard_bytes(text, path == CopyPath::Passthrough);
    let mut out = io::stdout();
    out.write_all(bytes.as_bytes())
        .and_then(|_| out.flush())
        .map_err(|e| format!("the terminal would not take the copy: {e}"))?;
    Ok(Copied {
        chars: text.chars().count(),
        path,
    })
}

/// The exact bytes a copy writes; `wrap_for_tmux` wraps it in a DCS passthrough.
pub fn clipboard_bytes(text: &str, wrap_for_tmux: bool) -> String {
    let osc = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    if wrap_for_tmux {
        format!("\x1bPtmux;{}\x1b\\", osc.replace('\x1b', "\x1b\x1b"))
    } else {
        osc
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn every_claim_set_has_a_matching_reset() {
        for c in CLAIMS {
            // Same mode number; the final byte differs (h/l, or l/h for the cursor).
            assert_eq!(&c.set[..c.set.len() - 1], &c.reset[..c.reset.len() - 1]);
            assert_ne!(c.set, c.reset);
        }
    }

    #[test]
    fn negotiation_inside_tmux_requests_modify_other_keys_and_reports_enhanced() {
        let mut out = Vec::new();
        let caps = negotiate_to(&mut out, true);
        assert_eq!(caps, KeyCaps { enhanced_keys: true });
        assert_eq!(String::from_utf8(out).unwrap(), MODIFY_OTHER_KEYS_ON);
    }

    #[test]
    fn negotiation_outside_tmux_only_queries_kitty_and_claims_nothing() {
        let mut out = Vec::new();
        let caps = negotiate_to(&mut out, false);
        assert_eq!(caps, KeyCaps { enhanced_keys: false });
        assert_eq!(String::from_utf8(out).unwrap(), KITTY_QUERY);
    }

    #[test]
    fn restore_always_turns_modify_other_keys_off_and_pops_kitty_only_if_pushed() {
        let unpushed = restore_bytes(false);
        let pushed = restore_bytes(true);
        assert!(unpushed.starts_with(FRAME_END), "{unpushed:?}");
        assert!(unpushed.contains(MODIFY_OTHER_KEYS_OFF), "{unpushed:?}");
        assert!(!unpushed.contains(KITTY_POP), "popped a push that never happened: {unpushed:?}");
        assert!(pushed.contains(MODIFY_OTHER_KEYS_OFF), "{pushed:?}");
        assert!(pushed.contains(KITTY_POP), "a kitty push was never popped: {pushed:?}");
        // The enhanced-key resets come BEFORE the claim resets, as in harness.
        let first_claim = pushed.find(CLAIMS.last().unwrap().reset).unwrap();
        assert!(pushed.find(KITTY_POP).unwrap() < first_claim);
        for c in CLAIMS {
            assert!(unpushed.contains(c.reset), "claim {} not reset", c.what);
        }
    }

    #[test]
    fn push_kitty_pushes_once() {
        // The only test touching the process-wide flag, so no ordering hazard.
        assert!(!kitty_pushed());
        assert!(push_kitty(), "the first answer must push");
        assert!(kitty_pushed());
        assert!(!push_kitty(), "a second answer pushed again");
    }

    #[test]
    fn tmux_wrapping_doubles_escapes() {
        let b = clipboard_bytes("x", true);
        assert!(b.starts_with("\x1bPtmux;\x1b\x1b]52;c;"));
        assert!(b.ends_with("\x1b\\"));
    }
}
