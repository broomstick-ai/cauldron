//! IS-111 (task 0327, 8e4fca10 msgs 199/200/234): task 0166's pty test pressed
//! `o` and the face really ran the system `open` on his machine. Every path to
//! an exec now goes through `cli::Opener`, read from `CAULDRON_OPENER`. These
//! drive the BUILT face under a real pty (`script(1)`), with the opener a
//! recording script, so what `o` runs is observed rather than inferred; and with
//! `none`, that nothing runs and the face says so. Hermetic: a scratch
//! CAULDRON_HOME, CAULDRON_TYPE_ONE=off. Nothing here ever execs `open`.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use env::paths::Source;
use env::Environment;
use tui::cauldron::app::{App, Item, KTab};
use tui::cauldron::cli::Opener;
use tui::cauldron::store::seed_dir;

/// The longest period between a key landing and the opener being reached: the
/// run loop drains the outbox at the top of every pass, and a pass waits at
/// most 200 ms on `event::poll`. Every negative here waits well past it, AFTER
/// the drain is witnessed.
const LOOP_POLL: Duration = Duration::from_millis(200);
/// How long a NEGATIVE waits for a recorder that must not run, past the
/// drain (a few ms in practice when one does run).
const RECORDER_GRACE: Duration = Duration::from_secs(3);
/// The deadline of a POSITIVE wait: for the recorder's file to appear.
/// `wait_file` returns the moment it does, so this costs nothing on a pass;
/// it only bounds a failure. V1.8.S3: it was `RECORDER_GRACE` (3 s), and a
/// shell spawn under the full suite's load outlasted it ("the opener never
/// ran"); the sibling tests already waited 20 s.
const SPAWN_DEADLINE: Duration = Duration::from_secs(20);

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "opener0327-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    // The face opens on prod's seed: prod must EXIST, or an empty root is
    // the first-run screen (V1.2).
    std::fs::create_dir_all(p.join("data").join("prod")).unwrap();
    p
}

/// A recording opener: writes each argument it was given, one per line, to
/// `<root>/opened`. A marked twin of it is `<root>/bin/open`, and every face here runs
/// with `PATH=<root>/bin:/bin`, so a regression to the system `open` execs
/// that twin, never his `open`, and is caught rather than suffered.
fn recorder(root: &Path) -> (PathBuf, PathBuf) {
    let log = root.join("opened");
    let script = root.join("record-open.sh");
    std::fs::write(&script, format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done\n", log.display())).unwrap();
    let mut perm = std::fs::metadata(&script).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perm, 0o755);
    std::fs::set_permissions(&script, perm).unwrap();
    // The stand-in system `open` marks what it records, so an arm that wanted
    // the CAULDRON_OPENER program can tell the two apart.
    let open = root.join("bin").join("open");
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::write(&open, format!("#!/bin/sh\nfor a in \"$@\"; do printf 'SYSTEM-OPEN %s\\n' \"$a\" >> '{}'; done\n", log.display())).unwrap();
    std::fs::set_permissions(&open, std::fs::metadata(&script).unwrap().permissions()).unwrap();
    (script, log)
}

/// The face's own scratch (HOME), BESIDE the cauldron root rather than in it.
fn face_scratch(root: &Path) -> PathBuf {
    root.with_extension("face")
}

fn path_env(root: &Path) -> String {
    format!("{}:/bin", root.join("bin").display())
}

/// Where X-2's Questions page puts its inserted artifact (0126-report-reaudit),
/// as the built face will see it from the same seed: (item index, its path).
fn the_artifact(root: &Path) -> (usize, String) {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let env = Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") };
    let mut a = App::open(env, seed_dir(), false).expect("seed loads");
    a.select(a.forest.find("X-2").unwrap());
    a.set_ktab(KTab::Questions);
    let at = a.items().iter().position(|i| matches!(i, Item::QArt { .. })).expect("X-2 has an inserted artifact");
    a.item = at;
    let path = a.artifact_path().expect("the artifact has a path");
    assert!(path.ends_with("0126-report-reaudit.md"), "{path}");
    (at, path)
}

fn face_args(at: usize) -> Vec<String> {
    // The fixture seed, named: a face on a real environment reads no seed (REL-3.S2).
    let seeds = seed_dir().to_string_lossy().into_owned();
    ["--cauldron", "--seeds", seeds.as_str(), "--cluster", "X-2", "--cluster-tab", "questions", "--item"].iter().map(|s| s.to_string()).chain([at.to_string()]).collect()
}

/// The face under a pty: `script -q /dev/null` gives it a terminal, `stty`
/// gives that terminal a size. Its screen is collected as it is written.
struct Pty {
    child: crate::face::FaceChild,
    stdin: ChildStdin,
    screen: Arc<Mutex<Vec<u8>>>,
}

impl Pty {
    fn spawn(root: &Path, opener: &str, args: &[String]) -> Pty {
        // REL-1.S1c: through the isolation helper (scratch HOME beside the
        // root, ports, AUI, CAULDRON_BIN), the opener a recorder in the root.
        let mut c = crate::face::Face::wrapped(&face_scratch(root), "/usr/bin/script", &["-q", "/dev/null", "/bin/sh", "-c", "stty rows 50 cols 150 && exec \"$0\" \"$@\""]);
        c.args(args)
            .env("CAULDRON_HOME", root)
            .env("CAULDRON_TYPE_ONE", "off")
            .env("CAULDRON_OPENER", opener)
            .env("TERM", "xterm-256color")
            .env("PATH", path_env(root))
            .env_remove("CAULDRON_ENV")
            .env_remove("ENV")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = c.spawn().expect("script(1) runs");
        let stdin = child.stdin.take().unwrap();
        let mut out = child.stdout.take().unwrap();
        let screen = Arc::new(Mutex::new(Vec::new()));
        let s = screen.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = out.read(&mut buf) {
                if n == 0 {
                    break;
                }
                s.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        Pty { child, stdin, screen }
    }

    /// The screen so far, escape sequences stripped.
    fn text(&self) -> String {
        let raw = String::from_utf8_lossy(&self.screen.lock().unwrap()).to_string();
        let mut out = String::new();
        let mut it = raw.chars().peekable();
        while let Some(ch) = it.next() {
            if ch != '\x1b' {
                out.push(ch);
                continue;
            }
            match it.next() {
                Some('[') => {
                    while let Some(c) = it.next() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = it.next() {
                        if c == '\x07' || c == '\x1b' {
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn wait_for(&self, what: &str, within: Duration) -> bool {
        let t = Instant::now();
        while t.elapsed() < within {
            if self.text().contains(what) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn press(&mut self, s: &str) {
        self.stdin.write_all(s.as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    /// `q`, and the face (and `script`) gone; ^C, then a kill, if not.
    fn quit(mut self) {
        self.press("q");
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(5) {
            if self.child.try_wait().unwrap().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.press("\x03");
        std::thread::sleep(Duration::from_millis(500));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The face is up once its Questions page shows the artifact card.
fn face_up(p: &Pty) {
    assert!(p.wait_for("0126-report-reaudit", Duration::from_secs(20)), "the face never drew X-2's artifact:\n{}", p.text());
}

fn wait_file(path: &Path, within: Duration) -> Option<String> {
    let t = Instant::now();
    while t.elapsed() < within {
        if let Ok(s) = std::fs::read_to_string(path) {
            if !s.is_empty() {
                return Some(s);
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

#[test]
fn o_under_a_pty_runs_the_cauldron_opener_with_the_artifacts_path() {
    let root = scratch_root();
    let (at, path) = the_artifact(&root);
    let (script, log) = recorder(&root);
    let mut p = Pty::spawn(&root, script.to_str().unwrap(), &face_args(at));
    face_up(&p);
    assert!(!log.exists(), "nothing ran before `o`");
    p.press("o");
    let got = wait_file(&log, SPAWN_DEADLINE);
    p.quit();
    assert_eq!(got.as_deref(), Some(format!("{path}\n").as_str()), "the opener ran once, with the artifact's path as its one argument");
}

#[test]
fn o_under_a_pty_with_cauldron_opener_none_runs_nothing_past_the_loop_poll_and_says_suppressed() {
    let root = scratch_root();
    let (at, _path) = the_artifact(&root);
    let (_script, log) = recorder(&root);
    // `none` is the opener; the recorder is `open` on this face's PATH, the
    // witness a regression to the system `open` would hit.
    let mut p = Pty::spawn(&root, "none", &face_args(at));
    face_up(&p);
    p.press("o");
    // The notice is set IN the drain, so seeing it witnesses that the opener
    // was reached; then wait past the loop's poll and the recorder's grace.
    let said = p.wait_for("open (suppressed): ", Duration::from_secs(10));
    std::thread::sleep(LOOP_POLL * 5 + RECORDER_GRACE);
    let screen = p.text();
    p.quit();
    // The notice names the path (the card's own title is not the witness:
    // only what follows the notice's words counts).
    let notice = screen.rfind("open (suppressed): ").map(|i| screen[i..].chars().take(120).collect::<String>()).unwrap_or_default();
    assert!(said && notice.contains("0126-report-reaudit.md"), "no `open (suppressed): <path>` notice: {notice:?}\n{screen}");
    assert!(!log.exists(), "nothing ran");
}

#[test]
fn a_dump_with_keys_o_never_execs_though_the_same_keys_live_do() {
    let root = scratch_root();
    let (at, path) = the_artifact(&root);
    let (script, log) = recorder(&root);
    let mut args = face_args(at);
    args.extend(["--keys".to_string(), "o".to_string()]);
    // --dump: the recorder is the opener, and `o` is pressed.
    let out = crate::face::Face::new(&face_scratch(&root))
        .args(&args)
        .arg("--dump")
        .env("CAULDRON_ISOLATED", "1")
        .env("CAULDRON_HOME", &root)
        .env("CAULDRON_TYPE_ONE", "off")
        .env("CAULDRON_OPENER", &script)
        .env("PATH", path_env(&root))
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let frame = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(frame.contains("open ~") || frame.contains("open /"), "the dump's `o` did press: its notice shows\n{frame}");
    std::thread::sleep(LOOP_POLL * 5 + RECORDER_GRACE);
    assert!(!log.exists(), "a --dump exec'd the opener");
    // The counterpart: the same args live, under a pty, do reach it.
    let p = Pty::spawn(&root, script.to_str().unwrap(), &args);
    let got = wait_file(&log, SPAWN_DEADLINE);
    p.quit();
    assert_eq!(got.as_deref(), Some(format!("{path}\n").as_str()));
}

#[test]
fn the_opener_takes_a_path_with_spaces_whole_and_a_leading_dash_never_as_an_option() {
    let root = scratch_root();
    let (script, log) = recorder(&root);
    let opener = Opener::parse(Some(script.clone().into()));
    for path in ["/tmp/a dir/with spaces.md", "-n", "--args evil", "/tmp/-plain.md"] {
        let _ = std::fs::remove_file(&log);
        assert_eq!(opener.open(path).unwrap(), true);
        let got = wait_file(&log, SPAWN_DEADLINE).unwrap_or_else(|| panic!("{path}: the opener never ran"));
        let want = if path.starts_with('-') { format!("./{path}\n") } else { format!("{path}\n") };
        assert_eq!(got, want, "{path}: one argument, never an option");
    }
    // `none` runs nothing, whatever the path.
    let _ = std::fs::remove_file(&log);
    assert_eq!(Opener::parse(Some("none".into())).open("/tmp/x.md").unwrap(), false);
    std::thread::sleep(RECORDER_GRACE);
    assert!(!log.exists());
}

#[test]
fn cauldron_opener_parses_unset_and_empty_as_the_system_open_none_as_suppressed_else_a_program() {
    // What `o` does for him is unchanged: no variable is the system `open`.
    assert_eq!(Opener::parse(None), Opener::System);
    assert_eq!(Opener::parse(Some("".into())), Opener::System);
    assert_eq!(Opener::parse(Some("none".into())), Opener::Suppressed);
    assert_eq!(Opener::parse(Some("None".into())), Opener::Program("None".into()));
    assert_eq!(Opener::parse(Some("/bin/echo".into())), Opener::Program("/bin/echo".into()));
}
