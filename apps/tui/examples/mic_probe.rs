//! **THE REAL STT AND PLAYBACK LEGS, DRIVEN THROUGH THE REAL CODE.**
//!
//! `cargo test` proves mic mode's pipeline with the commands SUBSTITUTED, which
//! is what makes it hermetic and what keeps it away from the microphone. This
//! probe is the other half: it calls `tui::mic` with NOTHING injected, so the
//! gate runs against a real WAV and the real `retranscribe` and the real
//! `afplay` are the things on the other end.
//!
//! It is what `mic_conformance_test.sh` drives. It never opens the microphone —
//! it only ever READS a WAV it is pointed at.
//!
//! ```text
//! mic_probe gate   <wav>   # the dead-audio gate's verdict, and the peak
//! mic_probe stt    <wav>   # gate, then the real transcriber
//! mic_probe play   <wav>   # the real afplay, spawned detached, at volume 0
//! mic_probe repair <wav>   # repair an under-declaring header in place
//! ```
//!
//! Exit codes are the probe's own and are read by the script:
//! `0` the act succeeded, `1` bad usage, `2` the act was REFUSED (which for
//! `gate` and `stt` is the correct answer on silence).

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(act), Some(path)) = (args.first(), args.get(1)) else {
        eprintln!("usage: mic_probe <gate|stt|play|repair> <wav>");
        return ExitCode::from(1);
    };
    let p = Path::new(path);

    match act.as_str() {
        "gate" => match std::fs::read(p).map(|b| tui::mic::gate(&b)) {
            Ok(Ok(peak)) => {
                println!("PASS peak={peak}");
                ExitCode::SUCCESS
            }
            Ok(Err(e)) => {
                println!("REFUSED {}", e.plainly());
                ExitCode::from(2)
            }
            Err(e) => {
                eprintln!("cannot read {path}: {e}");
                ExitCode::from(1)
            }
        },
        // **THE WHOLE POINT OF THIS PROBE.** `transcribe` runs the gate first
        // and only then the real transcriber, so a silent WAV must come back
        // REFUSED here while the bare `retranscribe` on the same file returns
        // `you` at exit 0. The script puts those two side by side.
        "stt" => match tui::mic::transcribe(p) {
            Ok(text) => {
                println!("TRANSCRIPT {text}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                println!("REFUSED {}", e.plainly());
                ExitCode::from(2)
            }
        },
        "play" => {
            // **VOLUME 0.** He is on a live audio interface and a conformance
            // run must not make a noise in his room.
            std::env::set_var(tui::mic::PLAY_VAR, "/usr/bin/afplay -v 0 {}");
            match tui::mic::play(p) {
                Ok(mut child) => loop {
                    match tui::mic::reap(&mut child) {
                        Some(Ok(())) => {
                            println!("PLAYED");
                            return ExitCode::SUCCESS;
                        }
                        Some(Err(code)) => {
                            println!("REFUSED afplay exit {code}");
                            return ExitCode::from(2);
                        }
                        None => std::thread::sleep(std::time::Duration::from_millis(50)),
                    }
                },
                Err(e) => {
                    println!("REFUSED {e}");
                    ExitCode::from(2)
                }
            }
        }
        "repair" => match tui::mic::repair_header(p) {
            Ok(true) => {
                println!("REPAIRED");
                ExitCode::SUCCESS
            }
            Ok(false) => {
                println!("INTACT");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("repair failed: {e}");
                ExitCode::from(1)
            }
        },
        other => {
            eprintln!("unknown act {other}");
            ExitCode::from(1)
        }
    }
}
