//! **REL-6a4: isolation follow-ups (items 2 and 3).**
//!
//! 1. MOVED to REL-6a5 by the executor: the refusal at every reconnect is
//!    built there, as the single URL-parsed dial choke point.
//! 2. Every file the face writes under HOME — the draft journal, mic
//!    captures, every log (REL-6a3-I F2) and the first-run dir — resolves
//!    under an isolated HOME when the process is isolated (REL-1.S1c-I N3):
//!    `cauldron::live::face_home` for HOME-rooted state,
//!    `isolated_root_refusal` for the cauldron store.
//! 3. Every `Face` launch is bounded (REL-1.S1c-I N2): `face::FaceChild`
//!    kills only its own pid at the limit and fails the test.
//!
//! The test that moves process-wide env (`CAULDRON_HOME`) runs in a CHILD
//! process of this binary (`--exact`, one
//! test), so no concurrent test in this binary ever sees the moved values.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tui::cauldron::live;

fn scratch(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new().prefix(&format!("rel6a4-{tag}-")).tempdir().expect("scratch dir")
}

/// Runs ONE ignored child test of this binary in its own process, bounded by
/// the face helper's watchdog (`FaceChild`: kills only that pid at the limit).
pub(crate) fn run_child(test: &str, envs: &[(&str, &str)], limit: Duration) -> String {
    let (status, all) = run_child_raw(test, envs, limit);
    assert!(status.success(), "child test `{test}` failed ({status}):\n{all}");
    assert!(all.contains("1 passed"), "child test `{test}` did not run exactly one test:\n{all}");
    all
}

/// [`run_child`] without its verdict: the caller checks what it watches
/// (a stat sentinel) BEFORE the child's own assertions decide the test.
pub(crate) fn run_child_raw(test: &str, envs: &[(&str, &str)], limit: Duration) -> (std::process::ExitStatus, String) {
    use std::os::unix::process::CommandExt;
    let mut c = Command::new(std::env::current_exe().expect("this test binary"));
    c.process_group(0);
    c.args([test, "--exact", "--include-ignored", "--nocapture", "--test-threads=1"])
        .env("REL6A4_CHILD", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in envs {
        c.env(k, v);
    }
    let mut child = crate::face::FaceChild::watch(c.spawn().expect("spawn the child test"), test.to_string(), limit);
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let o = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut s);
        s
    });
    let e = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut err, &mut s);
        s
    });
    let status = child.wait().expect("wait for the child test");
    let all = format!("{}\n--- stderr ---\n{}", o.join().unwrap(), e.join().unwrap());
    for l in all.lines().filter(|l| l.starts_with("REL6A4")) {
        println!("{l}");
    }
    (status, all)
}

pub(crate) fn is_child() -> bool {
    if std::env::var("REL6A4_CHILD").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("run only as a child of its outer rel6a4 test; nothing done");
    false
}

// ─────────────────────────────────────────────────────────────────────────
//  2. Face-written paths resolve under the isolated HOME / CAULDRON_HOME
// ─────────────────────────────────────────────────────────────────────────

/// [`live::face_home_from`] and [`live::isolated_root_refusal_from`] over
/// explicit inputs, against a stand-in for his home this test made itself.
#[test]
fn rel6a4_face_home_table() {
    let his = scratch("his");
    let his = his.path();
    let other = scratch("other");
    let other = other.path();
    let sc = PathBuf::from("/scratch-sentinel");
    let s = || Some(sc.clone());
    let p: &[PathBuf] = &[his.to_path_buf()];
    let none: &[PathBuf] = &[];
    // Isolated: his home, inside it, an ancestor of it, none, relative → scratch.
    assert_eq!(live::face_home_from(true, Some(his.to_path_buf()), p, s), Some(sc.clone()), "his home");
    assert_eq!(live::face_home_from(true, Some(his.join(".tui")), p, s), Some(sc.clone()), "inside his home");
    assert_eq!(live::face_home_from(true, his.parent().map(Path::to_path_buf), p, s), Some(sc.clone()), "an ancestor");
    assert_eq!(live::face_home_from(true, None, p, s), Some(sc.clone()), "no HOME");
    assert_eq!(live::face_home_from(true, Some(PathBuf::from("rel")), p, s), Some(sc.clone()), "relative");
    assert_eq!(live::face_home_from(true, Some(other.to_path_buf()), none, s), Some(sc.clone()), "his home unknown");
    // Isolated, and HOME already a scratch of its own → kept.
    assert_eq!(live::face_home_from(true, Some(other.to_path_buf()), p, s), Some(other.to_path_buf()), "a scratch HOME");
    // His interactive face → $HOME, whatever it is.
    assert_eq!(live::face_home_from(false, Some(his.to_path_buf()), p, s), Some(his.to_path_buf()), "his face");
    // The store root.
    let iso = Some("a test");
    assert!(live::isolated_root_refusal_from(iso, &his.join(".cauldron"), p).is_some(), "his store, isolated");
    assert!(live::isolated_root_refusal_from(iso, Path::new("/"), p).is_some(), "an ancestor as a store");
    assert_eq!(live::isolated_root_refusal_from(iso, &other.join(".cauldron"), p), None, "a scratch store, isolated");
    assert_eq!(live::isolated_root_refusal_from(None, &his.join(".cauldron"), p), None, "his store, his face");
}

/// In THIS process — isolated, and run with whatever HOME cargo gave it
/// (his, normally) — every face-written path resolves under the isolated
/// home, and a draft actually written lands there and not in his `~/.tui`.
#[test]
fn rel6a4_every_face_written_path_resolves_under_the_isolated_home() {
    assert!(live::isolation(false).is_some(), "sentinel: isolated");
    let passwd = env::guard::passwd_home().expect("sentinel: his home is findable");
    let home = live::face_home().expect("an isolated home");
    let paths: Vec<(&str, Option<PathBuf>)> = vec![
        ("face_home", Some(home.clone())),
        ("draft journal", tui::draft::dir()),
        ("mic captures", tui::mic::dir()),
        // REL-6a3-I F2: every log the face writes, both modes.
        ("log dir (~/.tui)", tui::first_run::state_dir()),
        ("tracing log", tui::first_run::state_dir().map(|d| d.join(tui::first_run::log_name(false)))),
        ("headless tracing log", tui::first_run::state_dir().map(|d| d.join(tui::first_run::log_name(true)))),
        ("diag log", tui::first_run::diag_default(false)),
        ("headless diag log", tui::first_run::diag_default(true)),
    ];
    for (what, p) in &paths {
        let p = p.as_ref().unwrap_or_else(|| panic!("{what}: no path"));
        assert!(live::is_isolated_root(p, Some(&passwd)), "{what} resolves to {}, under (or above) his home {}", p.display(), passwd.display());
        assert!(p.starts_with(&home), "{what} {} is outside the isolated home {}", p.display(), home.display());
    }
    // Behaviour, AFTER the resolution checks above (so a broken resolver
    // panics before anything is written into his home).
    let id = format!("rel6a4-guard-{}", std::process::id());
    tui::draft::save(&id, "rel6a4");
    let landed = tui::draft::dir().unwrap().join(&id);
    assert!(landed.exists(), "the draft did not land at {}", landed.display());
    assert!(!passwd.join(".tui/draft").join(&id).exists(), "the draft reached his ~/.tui/draft");
    tui::draft::clear(&id);
    assert!(!landed.exists(), "clear did not remove {}", landed.display());
}

/// Outer: the store root, in its own process (it moves `CAULDRON_HOME`).
#[test]
fn rel6a4_an_isolated_process_never_takes_his_store_as_its_root() {
    let out = run_child("rel6a4::rel6a4_child_store_root", &[("CAULDRON_ISOLATED", "1")], Duration::from_secs(60));
    assert!(out.contains("REL6A4-STORE-ROOT ok"), "{out}");
}

/// Child. A `CAULDRON_HOME` inside his home (a path that does not exist, so
/// nothing of his is read) is refused by both resolvers; a scratch one is not.
#[test]
#[ignore = "run in its own process by rel6a4_an_isolated_process_never_takes_his_store_as_its_root"]
fn rel6a4_child_store_root() {
    if !is_child() {
        return;
    }
    let passwd = env::guard::passwd_home().expect("his home is findable");
    let inside = passwd.join(format!(".cauldron-rel6a4-probe-{}", std::process::id()));
    assert!(!inside.exists(), "the probe path exists: {}", inside.display());
    std::env::set_var("CAULDRON_HOME", &inside);
    let e = tui::cauldron::app::resolve_env(None).expect_err("resolve_env took a root inside his home").to_string();
    assert!(e.contains("not an isolated root"), "{e}");
    let e = tui::cauldron_env::resolve().expect_err("cauldron_env::resolve took a root inside his home");
    assert!(e.contains("not an isolated root"), "{e}");
    let ok = scratch("store");
    std::env::set_var("CAULDRON_HOME", ok.path());
    let env = tui::cauldron::app::resolve_env(None).expect("a scratch root is taken");
    assert!(env.root.starts_with(ok.path()) || env.root.ends_with(ok.path().file_name().unwrap()), "{:?}", env.root);
    if let Err(e) = tui::cauldron_env::resolve() {
        assert!(!e.contains("not an isolated root"), "a scratch root was refused: {e}");
    }
    assert!(!inside.exists(), "resolving created {}", inside.display());
    println!("REL6A4-STORE-ROOT ok");
}

// The HOME census that stood here (`rel6a4_every_home_dir_call_in_src_is_routed_or_read_only`)
// allowlisted main.rs's App home as "display" when it fed the registry write
// (REL-6a4-I B1). It is replaced by `rel6a4b::rel6a4b_every_home_lookup_in_src_is_face_home_or_a_checked_read`.

// ─────────────────────────────────────────────────────────────────────────
//  3. Every Face launch is bounded, and the bound kills only its own child
// ─────────────────────────────────────────────────────────────────────────

fn panic_text(p: Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
}

fn alive(pid: u32) -> bool {
    Command::new("/bin/ps").args(["-p", &pid.to_string()]).stdout(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// A launch that outlives its limit is killed — that pid, and not a sibling
/// running the very same command line — and the test fails naming it, for
/// both `output()` and `spawn()`+`wait()`. Opposed: a launch that finishes
/// inside its limit is not touched (exit code 3 comes back).
#[test]
fn rel6a4_a_face_launch_past_its_limit_is_killed_by_pid_and_fails_the_test() {
    const SLEEP: &str = "exec /bin/sleep 7357.4";
    let dir = scratch("timeout");
    // The sibling: same command line, not the helper's.
    let mut sibling = Command::new("/bin/sh").args(["-c", SLEEP]).spawn().expect("sibling");

    // Opposed first: a quick launch is left alone.
    let mut quick = crate::face::Face::wrapped(dir.path(), "/bin/sh", &["-c", "exit 3"]);
    quick.timeout(Duration::from_secs(10));
    assert_eq!(quick.output().expect("runs").status.code(), Some(3), "a launch inside its limit was disturbed");

    for how in ["output", "spawn+wait"] {
        let d = dir.path().to_path_buf();
        let (tx, rx) = std::sync::mpsc::channel();
        let t0 = Instant::now();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut f = crate::face::Face::wrapped(&d, "/bin/sh", &["-c", SLEEP]);
                f.timeout(Duration::from_millis(700));
                if how == "output" {
                    let _ = f.output();
                } else {
                    let mut c = f.spawn().expect("spawn");
                    let _ = c.wait();
                }
            }));
            let _ = tx.send(r);
        });
        let r = rx
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("[{how}] still waiting 20 s after a 700 ms limit: the launch was never killed"));
        let msg = panic_text(r.expect_err(&format!("[{how}] a launch past its limit did not fail the test")));
        assert!(msg.contains("FACE TIMED OUT"), "[{how}] {msg}");
        assert!(t0.elapsed() < Duration::from_secs(10), "[{how}] took {:?}", t0.elapsed());
        let pid: u32 = msg
            .split("(pid ")
            .nth(1)
            .and_then(|s| s.split(')').next())
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("[{how}] no pid in {msg}"));
        assert!(!alive(pid), "[{how}] pid {pid} is still running after the timeout");
        assert!(sibling.try_wait().unwrap().is_none(), "[{how}] the sibling (pid {}) was killed too: the kill was not by pid", sibling.id());
    }
    let _ = sibling.kill();
    let _ = sibling.wait();
}
