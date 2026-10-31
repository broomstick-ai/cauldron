//! **A probe of the CLIPBOARD PATH, on a real terminal, writing real bytes.**
//!
//! `term::copy_to_clipboard`'s unit tests assert the SHAPE of the sequence.
//! They cannot assert the thing that actually went wrong in his session: that
//! a raw `OSC 52` written from inside a tmux pane never leaves tmux, because
//! `input_osc_52()` returns early unless `set-clipboard` is `on` and he runs
//! `external`. That is a fact about a byte stream leaving a pty, so it is
//! measured on one — by `drag_frames.sh`, which runs this inside a PRIVATE
//! tmux server and reads the outer pty.
//!
//! Three modes, so the wrapper can be held against its own control:
//!
//! ```text
//!   clip_probe wrapped <text>   the real path — raw outside tmux, DCS
//!                               passthrough when $TMUX is set
//!   clip_probe raw <text>       the OLD path, unconditionally raw. The
//!                               FORCED FAILURE: under `set-clipboard
//!                               external` this reaches nothing, which is the
//!                               defect `0041` fixes.
//!   clip_probe oversize         a copy past the ceiling. It must write NO
//!                               bytes at all and print a refusal that names
//!                               the limit — an honest toast instead of the
//!                               silent loss it has today.
//! ```
//!
//! It writes the sequence to STDOUT (the pane's tty, which is what the face
//! does) and its verdict to STDERR, so the two never mix on the wire being
//! measured.
//!
//! **It touches no daemon and no terminal state**: no alternate screen, no raw
//! mode, no DECSET. It writes one escape sequence and exits.
//!
//! `cargo build -p tui --example clip_probe`

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("wrapped");
    let text = args.get(1).cloned().unwrap_or_else(|| "PROBE_TEXT".into());

    match mode {
        // The real path, exactly as the face takes it.
        "wrapped" => match tui::term::copy_to_clipboard(&text) {
            Ok(done) => eprintln!("VERDICT ok path={:?} notice={}", done.path, done.notice()),
            Err(why) => eprintln!("VERDICT refused {why}"),
        },
        // The path this face shipped with until `0041`: raw, always.
        "raw" => {
            let seq = tui::term::clipboard_bytes(&text, false);
            let mut out = std::io::stdout();
            let _ = out.write_all(seq.as_bytes());
            let _ = out.flush();
            eprintln!("VERDICT raw {} bytes written", seq.len());
        }
        // Past the ceiling. Nothing may reach the wire.
        "oversize" => {
            let big = "x".repeat(800_000);
            match tui::term::copy_to_clipboard(&big) {
                Ok(done) => eprintln!("VERDICT ok path={:?} notice={}", done.path, done.notice()),
                Err(why) => eprintln!("VERDICT refused {why}"),
            }
        }
        other => eprintln!("VERDICT unknown mode {other}"),
    }
}
