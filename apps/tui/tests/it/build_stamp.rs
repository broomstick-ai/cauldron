//! **`tui --build-stamp`: THE FACE STATES ITS OWN COMMIT** (R7.S4, H179-1).
//!
//! Deploy 6 installed `~/.cauldron/bin/tui` and could say which commit it was
//! only from the BUILD that produced it — the binary refused the flag with
//! `unknown argument`. This file holds the verb to the contract `cauldron` and
//! `tts` already keep (`cli/tests/self_stamp.rs`, `services/tts/tests/
//! build_stamp.rs`): one `key=value` line, exit 0, from the binary cargo just
//! built, and — the face's own extra — WITHOUT touching the terminal.
//!
//! THE WITNESS IS NOT THE STAMP. Comparing the printed commit to
//! `env!("TUI_BUILD_COMMIT")` would be two reads of one value. The independent
//! witnesses are `git rev-parse HEAD` and `git status` over the published
//! scope, run at TEST time in the tree the binary was built from.

use std::process::{Command, Output, Stdio};

use crate::face::Face;

fn git(args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .expect("spawn git")
}

fn git_one(args: &[&str]) -> String {
    let o = git(args);
    assert!(o.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

/// `tui --build-stamp` with stdout and stderr PIPED (not a terminal) and stdin
/// null, under exactly the environment given — nothing inherited. REL-1.S1c:
/// through the isolation helper; the verb connects to nothing, so every
/// isolation var the caller does not give is UNSET for it (an offline verb),
/// which keeps this file's "no environment at all".
fn stamp_with_env(env: &[(&str, &str)]) -> Output {
    let scratch = tempfile::tempdir().unwrap();
    let mut c = Face::new(scratch.path());
    c.arg("--build-stamp").env_clear().stdin(Stdio::null());
    for n in crate::face::names() {
        if !env.iter().any(|(k, _)| *k == n) {
            c.unset_for_offline_verb(n);
        }
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().expect("spawn tui")
}

/// The stamp's fields, IN ORDER.
fn fields(stdout: &str) -> Vec<(String, String)> {
    stdout
        .trim_end_matches('\n')
        .split(' ')
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or_else(|| panic!("`{kv}` is not key=value"));
            (k.to_string(), v.to_string())
        })
        .collect()
}

fn get<'a>(f: &'a [(String, String)], k: &str) -> &'a str {
    f.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.as_str()).unwrap_or_else(|| panic!("no {k}= in {f:?}"))
}

/// **(a) THE COMMIT IS HEAD OF THE TREE THIS WAS BUILT FROM, AND THE DIRT IS
/// WHAT GIT SAYS IT IS.** Forced red (report): `build.rs` sniffing `HEAD~1`,
/// `build.rs` inverting the dirt verdict, and a stale binary run after a
/// temporary commit. Opposed: the same unmodified test green with `dirty=1`
/// on a tree with an uncommitted edit in scope, and with `dirty=0` once clean,
/// so a constant `dirty=0` cannot pass both.
#[test]
fn the_stamp_names_head_of_the_tree_it_was_built_from_and_its_true_dirt() {
    let out = stamp_with_env(&[]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "exit 0 with NO environment at all; stderr: {stderr}");
    assert!(stderr.is_empty(), "nothing on stderr (the old binary said `unknown argument`): {stderr}");
    assert_eq!(stdout.lines().count(), 1, "exactly one line: {stdout:?}");
    assert!(stdout.ends_with('\n'), "{stdout:?}");

    let f = fields(&stdout);
    let keys: Vec<&str> = f.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        ["commit", "provenance", "dirty", "dirt_scope"],
        "the SAME fields in the SAME order as `cauldron --build-stamp` and `tts --build-stamp`"
    );

    let commit = get(&f, "commit");
    assert_eq!(commit.len(), 40, "a full sha: {commit}");
    assert!(commit.chars().all(|c| c.is_ascii_hexdigit()), "{commit}");
    match get(&f, "provenance") {
        "git" => assert_eq!(commit, git_one(&["rev-parse", "HEAD"]), "sniffed commit must be HEAD of the tree the binary was built in"),
        "pin" => assert_eq!(commit, std::env::var("TUI_BUILD_COMMIT_PIN").unwrap_or_default().trim(), "pinned commit is the pin"),
        other => panic!("provenance must be git or pin for a stamped build, got {other}"),
    }

    let scope: Vec<&str> = get(&f, "dirt_scope").split(',').collect();
    assert!(!scope.contains(&"unknown"), "the scope could not be derived: {stdout}");
    for must in ["Cargo.toml", "Cargo.lock", "apps/tui", "libs/env", "libs/tui-render"] {
        assert!(scope.contains(&must), "dirt_scope lacks {must}: {stdout}");
    }
    let dirty = get(&f, "dirty");
    assert!(matches!(dirty, "0" | "1"), "dirty={dirty}: git works here, so unknown is a defect");
    // The witness does not parse porcelain: "git printed anything inside the
    // scope" is the whole verdict, so a parser defect in `build_dirt.rs`
    // cannot hide on both sides at once.
    let top = git_one(&["rev-parse", "--show-toplevel"]);
    let st = Command::new("git")
        .arg("-C")
        .arg(&top)
        .args(["status", "--porcelain", "--untracked-files=normal", "--"])
        .args(&scope)
        .output()
        .expect("git status");
    assert!(st.status.success(), "git status over the scope failed");
    let witness = if st.stdout.is_empty() { "0" } else { "1" };
    assert_eq!(
        dirty,
        witness,
        "the stamp says dirty={dirty}, git status NOW over {scope:?} says {witness}:\n{}",
        String::from_utf8_lossy(&st.stdout)
    );
}

/// **THE VERB IS ROUTED BEFORE ANYTHING THAT CAN REFUSE.** A half-set port
/// lever (`TUI_THREADS_PORT` without `TUI_THREADS_CHASSIS_PORT`) and an
/// environment name that does not exist each stop the face at startup. The
/// stamp must still answer, identically. Opposed: the SAME environment with
/// `--resolve` must be refused — otherwise this environment refuses nothing
/// and the first half proves nothing.
#[test]
fn the_stamp_answers_where_the_face_itself_would_refuse_to_start() {
    let home = tempfile::tempdir().unwrap();
    let home_s = home.path().to_str().unwrap();
    let hostile = [("HOME", home_s), ("TUI_THREADS_PORT", "1"), ("CAULDRON_ENV", "no-such-env-r7s4")];

    let clean = stamp_with_env(&[]);
    let under = stamp_with_env(&hostile);
    assert_eq!(under.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&under.stderr));
    assert_eq!(under.stdout, clean.stdout, "the stamp does not depend on the environment");

    // REL-1.S1c: the chassis port is UNSET for this offline verb (the
    // half-set lever is the refusal under test); every other isolation var,
    // CAULDRON_HOME included, is the helper's scratch value.
    let scratch = tempfile::tempdir().unwrap();
    let resolve = Face::new(scratch.path())
        .arg("--resolve")
        .env_clear()
        .envs(hostile)
        .unset_for_offline_verb("TUI_THREADS_CHASSIS_PORT")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_ne!(
        resolve.status.code(),
        Some(0),
        "sentinel: this environment must make the face refuse, or the test above proves nothing; got stdout {:?}",
        String::from_utf8_lossy(&resolve.stdout)
    );
    assert!(home.path().read_dir().unwrap().next().is_none(), "the stamp (and the refusal) wrote nothing under HOME");
    let root = scratch.path().join("cauldron");
    assert!(root.read_dir().unwrap().next().is_none(), "the refusal wrote nothing under its CAULDRON_HOME");
}

/// Run `tui <args>` as the session leader of a REAL pty (python's `pty.fork`;
/// this crate forbids `unsafe`, so it cannot open one itself) for at most
/// `seconds`, killing it after. Returns (exit code, or -signal; every byte it
/// wrote to the terminal).
fn on_a_pty(args: &[&str], env: &[(&str, &str)], seconds: f32) -> (i32, Vec<u8>) {
    const DRIVER: &str = r#"
import os, pty, select, sys, time, signal
secs, outp, binp = float(sys.argv[1]), sys.argv[2], sys.argv[3]
args = sys.argv[4:]
pid, fd = pty.fork()
if pid == 0:
    os.execv(binp, [binp] + args)
buf = b""
end = time.time() + secs
while time.time() < end:
    r, _, _ = select.select([fd], [], [], 0.05)
    if r:
        try:
            c = os.read(fd, 1 << 20)
        except OSError:
            break
        if not c:
            break
        buf += c
done, status = os.waitpid(pid, os.WNOHANG)
if done == 0:
    os.kill(pid, signal.SIGKILL)
    # REL-1.S1c: KEEP READING while it dies. A killed child's exit closes its
    # tty, and on macOS that close waits for unread output to drain; with
    # nobody reading the master, the child sat in exit (`?Es`) and a blocking
    # waitpid here hung the whole suite for three hours. Drain until it is
    # reaped (bounded), then close the master, which releases any drain left.
    stop = time.time() + 10
    while done == 0 and time.time() < stop:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                os.read(fd, 1 << 20)
            except OSError:
                pass
        done, status = os.waitpid(pid, os.WNOHANG)
    os.close(fd)
    if done == 0:
        _, status = os.waitpid(pid, 0)
open(outp, "wb").write(buf)
print(os.WEXITSTATUS(status) if os.WIFEXITED(status) else -os.WTERMSIG(status))
"#;
    let dir = tempfile::tempdir().unwrap();
    let outp = dir.path().join("pty.bytes");
    // REL-1.S1c: the driver execs the face with the environment it was given,
    // which is the isolation helper's (the face's args follow `binp`).
    let (secs, outs) = (seconds.to_string(), outp.to_str().unwrap().to_string());
    let mut c = Face::wrapped(dir.path(), "python3", &["-c", DRIVER, &secs, &outs]);
    c.args(args);
    c.env_clear().env("TERM", "xterm-256color");
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().expect("spawn python3");
    assert!(o.status.success(), "pty driver failed: {}", String::from_utf8_lossy(&o.stderr));
    let code: i32 = String::from_utf8_lossy(&o.stdout).trim().parse().expect("driver prints the exit code");
    (code, std::fs::read(&outp).unwrap())
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// **(b) ON A REAL TERMINAL THE VERB CLAIMS NOTHING.** No alternate screen
/// (`ESC[?1049h`), indeed no escape byte at all, and the line arrives as
/// `\r\n` — the pty's output post-processing (`OPOST`/`ONLCR`) was still on
/// when it was written, which raw mode turns off. Forced red (report): the
/// verb arm calling `tui::term::enter()` before it prints. Opposed: the
/// SENTINEL below — the same driver, the same predicate, the same binary, run
/// as the chat face — DOES see `ESC[?1049h`, so the predicate is capable of
/// finding it.
#[test]
fn on_a_pty_the_stamp_enters_neither_raw_mode_nor_the_alternate_screen() {
    let home = tempfile::tempdir().unwrap();
    let home_s = home.path().to_str().unwrap();
    let (code, bytes) = on_a_pty(&["--build-stamp"], &[("HOME", home_s)], 5.0);
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(code, 0, "exit 0 on a pty: {text:?}");
    assert!(!contains(&bytes, b"\x1b[?1049h"), "the stamp entered the alternate screen: {text:?}");
    assert!(!bytes.contains(&0x1b), "no escape sequence of any kind: {text:?}");
    assert!(bytes.ends_with(b"\r\n"), "`\\n` became `\\r\\n`, so the tty was NOT in raw mode at print: {text:?}");
    let piped = stamp_with_env(&[]);
    assert_eq!(
        text.trim_end(),
        String::from_utf8_lossy(&piped.stdout).trim_end(),
        "the same line on a terminal as on a pipe"
    );

    // ── SENTINEL: the predicate can see a claim when there is one ─────────
    // The chat face, pointed at a port nothing listens on (both halves of the
    // lever, so it is not refused) and with HOME a tempdir (its log and its
    // flight recorder land there), killed after two seconds.
    let face_home = tempfile::tempdir().unwrap();
    let face_home_s = face_home.path().to_str().unwrap();
    let (_, face) = on_a_pty(
        &[],
        &[("HOME", face_home_s), ("TUI_THREADS_CHASSIS_PORT", "1"), ("TUI_THREADS_PORT", "1"), ("TUI_DIAG", "off")],
        2.0,
    );
    assert!(
        contains(&face, b"\x1b[?1049h"),
        "sentinel: the chat face on the same pty driver must show ESC[?1049h, or the assertions above cannot fail; got {:?}",
        String::from_utf8_lossy(&face)
    );
}
