//! **A face KILLED gives the terminal back (IS-59).** Each test re-executes
//! this test binary as a CHILD under a real pty (`script(1)`), the child takes
//! the terminal exactly as a face does, the parent sends it a signal, and the
//! assertions are on the bytes the pty actually received and on the exit code
//! `script` passes through from the child. Nothing here reads the child's own
//! account of what it did.
//!
//! The child is the `child` test below, which does nothing unless
//! `TERM_SHELL_CHILD` names a mode — so in an ordinary run it is an empty pass.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MODE: &str = "TERM_SHELL_CHILD";
const PIDFILE: &str = "TERM_SHELL_PIDFILE";
const READY: &str = "<<child-ready>>";
const SLOW: &str = "<<slow-restore-started>>";

// ── the child ───────────────────────────────────────────────────────────────

#[test]
fn child() {
    let Ok(mode) = std::env::var(MODE) else { return };
    let mut out = std::io::stdout();
    if mode == "slow" {
        // A restore that takes long enough for a second signal to land inside it.
        term_shell::restore_on_signals(slow_restore).unwrap();
    } else {
        term_shell::install_panic_hook();
    }
    let _term = term_shell::enter().unwrap();
    match mode.as_str() {
        "kitty" => {
            term_shell::push_kitty();
        }
        // The query is out and nothing has answered it.
        "unanswered" => {
            term_shell::negotiate_to(&mut out, false);
        }
        _ => {}
    }
    if mode == "midframe" {
        // A frame opened and never closed: the signal lands INSIDE it.
        out.write_all(term_shell::FRAME_BEGIN.as_bytes()).unwrap();
        out.write_all(b"half a frame").unwrap();
    }
    out.write_all(READY.as_bytes()).unwrap();
    out.flush().unwrap();
    std::fs::write(std::env::var(PIDFILE).unwrap(), std::process::id().to_string()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if mode == "drawing" {
            // Frames drawn as many small writes, back to back, so the signal
            // lands while the main thread is writing and keeps trying to.
            for row in 0..40 {
                if row == 0 {
                    let _ = out.write_all(term_shell::FRAME_BEGIN.as_bytes());
                }
                let _ = out.write_all(format!("\x1b[{row};1Hrow {row} ......").as_bytes());
            }
            let _ = out.write_all(term_shell::ALT_SCREEN.set.as_bytes());
            let _ = out.write_all(term_shell::FRAME_END.as_bytes());
            let _ = out.flush();
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    panic!("child was never signalled");
}

fn slow_restore() {
    let mut out = std::io::stdout();
    let _ = out.write_all(SLOW.as_bytes());
    let _ = out.flush();
    std::thread::sleep(Duration::from_millis(600));
    term_shell::restore();
}

// ── the parent ──────────────────────────────────────────────────────────────

struct Run {
    out: Vec<u8>,
    code: Option<i32>,
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("term-shell-sig-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn wait_for(path: &Path, what: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let b = std::fs::read(path).unwrap_or_default();
        if what(&b) {
            return b;
        }
        assert!(Instant::now() < deadline, "timed out waiting on {}", path.display());
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// Run the child in `mode` under a pty, send `signals` in order (each after
/// the previous one's `gate` marker appears in the output), and collect.
fn run(mode: &str, signals: &[(&str, Option<&str>)]) -> Run {
    let dir = scratch(&format!("{mode}-{}", signals[0].0));
    let typescript = dir.join("typescript");
    let pidfile = dir.join("pid");
    let mut script = Command::new("/usr/bin/script")
        .arg("-q")
        .arg("-F")
        .arg(&typescript)
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "child", "--nocapture", "--test-threads=1"])
        .env(MODE, mode)
        .env(PIDFILE, &pidfile)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn script(1)");
    wait_for(&typescript, |b| contains(b, READY));
    let pid = String::from_utf8(wait_for(&pidfile, |b| !b.is_empty())).unwrap();
    for (sig, gate) in signals {
        if let Some(g) = gate {
            wait_for(&typescript, |b| contains(b, g));
        }
        let rc = Command::new("/bin/kill").arg(format!("-{sig}")).arg(&pid).status().unwrap();
        assert!(rc.success(), "kill -{sig} {pid}");
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(s) = script.try_wait().unwrap() {
            break s;
        }
        if Instant::now() > deadline {
            let _ = Command::new("/bin/kill").arg("-KILL").arg(&pid).status();
            let _ = script.kill();
            panic!("{mode}: the child did not exit after {signals:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let out = std::fs::read(&typescript).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    Run { out, code: status.code() }
}

fn assert_ends_restored(r: &Run, kitty: bool, what: &str) {
    let tail = term_shell::restore_bytes(kitty);
    // Spelled out here rather than taken from the crate, so a restore that
    // lost one of them cannot pass by changing `restore_bytes` too.
    let tail_bytes = &r.out[r.out.len().saturating_sub(tail.len())..];
    let mut must = vec![
        ("synchronized frame end", "\x1b[?2026l"),
        ("modifyOtherKeys off", "\x1b[>4;0m"),
        ("mouse buttons off", "\x1b[?1002l"),
        ("SGR mouse off", "\x1b[?1006l"),
        ("bracketed paste off", "\x1b[?2004l"),
        ("alternate screen off", "\x1b[?1049l"),
    ];
    if kitty {
        must.push(("kitty pop", "\x1b[<u"));
    }
    for (name, seq) in must {
        assert!(contains(tail_bytes, seq), "{what}: {name} is not in the final bytes");
    }
    assert!(
        r.out.ends_with(tail.as_bytes()),
        "{what}: the pty's last bytes are not the restore.\n want tail {tail:?}\n got tail  {:?}",
        String::from_utf8_lossy(&r.out[r.out.len().saturating_sub(tail.len() + 40)..])
    );
}

const SIGNOS: [(&str, i32); 4] = [("TERM", 15), ("HUP", 1), ("INT", 2), ("QUIT", 3)];

#[test]
fn each_caught_signal_restores_the_terminal_and_exits_128_plus_signo() {
    for (sig, no) in SIGNOS {
        let r = run("plain", &[(sig, None)]);
        assert_ends_restored(&r, false, sig);
        assert_eq!(r.code, Some(128 + no), "SIG{sig}: exit code");
        // The restore came AFTER the claims, so it undid something.
        assert!(contains(&r.out, term_shell::MOUSE_BUTTONS.set), "SIG{sig}: never claimed");
    }
}

#[test]
fn a_pushed_kitty_stack_is_popped_on_a_kill() {
    let r = run("kitty", &[("TERM", None)]);
    assert!(contains(&r.out, term_shell::KITTY_PUSH));
    assert_ends_restored(&r, true, "kitty");
    assert_eq!(r.code, Some(143));
}

#[test]
fn a_kill_before_the_kitty_answer_pops_nothing_and_still_turns_modify_other_keys_off() {
    let r = run("unanswered", &[("HUP", None)]);
    assert!(contains(&r.out, term_shell::KITTY_QUERY));
    assert!(!contains(&r.out, term_shell::KITTY_POP), "popped a push that never happened");
    assert_ends_restored(&r, false, "unanswered");
    assert_eq!(r.code, Some(129));
}

#[test]
fn a_kill_mid_frame_closes_the_frame() {
    let r = run("midframe", &[("TERM", None)]);
    assert_ends_restored(&r, false, "midframe");
    assert_eq!(r.code, Some(143));
}

#[test]
fn a_kill_while_drawing_leaves_no_frame_bytes_after_the_restore() {
    // The child re-claims the alternate screen in every frame, so any frame
    // byte written after the restore would end the output with a claim.
    for _ in 0..3 {
        let r = run("drawing", &[("TERM", None)]);
        assert_ends_restored(&r, false, "drawing");
        assert_eq!(r.code, Some(143));
    }
}

#[test]
fn a_second_signal_during_the_restore_neither_cuts_it_short_nor_changes_the_code() {
    let r = run("slow", &[("TERM", None), ("INT", Some(SLOW))]);
    assert_ends_restored(&r, false, "slow");
    assert_eq!(r.code, Some(143), "the exit code must be the FIRST signal's");
}
