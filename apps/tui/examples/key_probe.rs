//! **WHAT THE TERMINAL ACTUALLY EMITS FOR A MODIFIED KEY, AND WHAT WE DECODE
//! IT AS — measured, side by side, on a real pty.**
//!
//! `0052` was told to VERIFY `alt-←`/`alt-→` on his tmux rather than trust the
//! key table, and to decide `ctrl-←`/`ctrl-→`/`ctrl-↑`/`ctrl-↓` on a
//! MEASUREMENT — *"if it can be made to work in his terminal"* — with the one
//! legal "cannot" being a measured one. A table cannot answer that question and
//! neither can reasoning: only bytes can.
//!
//! So this prints, for every read, two things:
//!
//! * the RAW BYTES, escaped, exactly as they came off the descriptor;
//! * what [`tui::vt::Parser`] makes of them, and then what
//!   [`tui::input::to_key`] makes of THAT — which is the whole path a keystroke
//!   takes before the key table ever sees it.
//!
//! A key that arrives indistinguishably from another shows up as the same
//! decoded line, which is the failure this is looking for.
//!
//! **It claims almost nothing.** Raw mode, so keys arrive unbuffered, and — on
//! request (`--enhanced`) — the same `modifyOtherKeys` push the face makes, so
//! the measurement is of the stack the face actually runs on. No alternate
//! screen, no mouse, no clipboard: it is a measuring instrument, not a face.
//!
//! Usage (inside a pty, normally a private tmux pane):
//!   cargo run -p tui --example key_probe -- [--enhanced] [--seconds N]

use std::io::{IsTerminal, Read, Write};
use std::time::{Duration, Instant};

use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};

fn escaped(b: &[u8]) -> String {
    b.iter()
        .map(|&c| match c {
            0x1b => "ESC ".to_string(),
            0x20..=0x7e => format!("{} ", c as char),
            _ => format!("\\x{c:02x} "),
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let enhanced = args.iter().any(|a| a == "--enhanced");
    let seconds: u64 = args
        .iter()
        .position(|a| a == "--seconds")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    if !std::io::stdin().is_terminal() {
        eprintln!("key_probe needs a terminal on stdin");
        return;
    }
    let _ = enable_raw_mode();
    if enhanced {
        // The same request `term::negotiate` makes inside tmux. Written
        // straight at the terminal, exactly as the face does it.
        let mut o = std::io::stdout();
        let _ = o.write_all(b"\x1b[>4;2m");
        let _ = o.flush();
    }
    println!("key_probe: raw bytes -> vt::Out -> Key   (enhanced={enhanced})\r");
    println!("press keys; it stops after {seconds}s\r");

    let mut parser = tui::vt::Parser::new();
    let mut buf = [0u8; 1024];
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut stdin = std::io::stdin();
    while Instant::now() < deadline {
        let Ok(n) = stdin.read(&mut buf) else { break };
        if n == 0 {
            break;
        }
        let raw = escaped(&buf[..n]);
        let mut outs = Vec::new();
        parser.feed(&buf[..n], Instant::now(), &mut outs);
        // The idle rules are what turn a lone ESC into the Escape key; a probe
        // that never idles would hold one forever.
        std::thread::sleep(Duration::from_millis(60));
        parser.idle(Instant::now(), &mut outs);
        if outs.is_empty() {
            println!("BYTES  {raw}   -> (nothing yet)\r");
            continue;
        }
        for out in outs {
            let decoded = match &out {
                tui::vt::Out::Event(ratatui::crossterm::event::Event::Key(k)) => {
                    format!(
                        "{:?}+{:?} -> Key {:?}",
                        k.code,
                        k.modifiers,
                        tui::input::to_key(*k)
                    )
                }
                other => format!("{other:?}"),
            };
            println!("BYTES  {raw}   ->  {decoded}\r");
        }
        let _ = std::io::stdout().flush();
    }
    if enhanced {
        let mut o = std::io::stdout();
        let _ = o.write_all(b"\x1b[>4;0m");
        let _ = o.flush();
    }
    let _ = disable_raw_mode();
    println!("key_probe: done\r");
}
