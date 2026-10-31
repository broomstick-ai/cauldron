//! **REL-6a4b: the face's home under isolation, and four smaller leaks**
//! (REL-6a4-I B1, N1, N2, N3, N6).
//!
//! - B1: under isolation the chat face's `App` home (the registry write on
//!   register / reorder) is `cauldron::live::face_home`, and
//!   `index::apply_registry` refuses any home that is not an isolated root.
//!   Every home lookup in `src/` is `face_home` or a CHECKED read (census).
//! - N1: `TUI_DIAG=<path>` is honoured under isolation only under the
//!   isolated home.
//! - N2: the pool board's `set-live-login` child is refused under isolation
//!   (unless the tool is a stub in a scratch dir).
//! - N3: the scratch home never resolves under his home, whatever `$TMPDIR`.
//! - N6: a `Face` launch is its own process group; the limit kills the
//!   group and the descendants, and `output()` cannot hang on a pipe a
//!   descendant holds.
//!
//! Every test that would WRITE, if its guard were broken, writes under a
//! STAND-IN for his home (`CAULDRON_PROTECTED_HOME`, in a child process),
//! never under his real one; his real files are only ever `stat`ed.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tui::cauldron::live;

use crate::rel6a4::{is_child, run_child, run_child_raw};

fn scratch(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new().prefix(&format!("rel6a4b-{tag}-")).tempdir().expect("scratch dir")
}

/// `(inode, size, mtime ns)` of `p`, or `None` if absent. A `stat`, nothing else.
fn stat(p: &Path) -> Option<(u64, u64, i128)> {
    std::fs::metadata(p).ok().map(|m| (m.ino(), m.size(), m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128))
}

/// Names in a dir, sorted (empty if absent).
fn listing(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(d)
        .map(|r| r.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

fn registry_names(home: &Path) -> Vec<String> {
    projects::Registry::load(home)
        .expect("the registry loads")
        .expect("the registry is present")
        .names()
        .map(str::to_string)
        .collect()
}

/// A registry at `home` holding `names`, each a dir under `home/code`.
fn seed_registry(home: &Path, names: &[&str]) {
    std::fs::create_dir_all(projects::cauldron_dir(home)).expect("mkdir .cauldron");
    let mut reg = projects::Registry::empty();
    for n in names {
        let d = home.join("code").join(n);
        std::fs::create_dir_all(&d).expect("mkdir project");
        reg.register(&d, Some(n)).expect("registered");
    }
    reg.save(home).expect("saved");
}

/// His real registry files, watched by `stat` only (never opened).
fn his_real_registry() -> Vec<(PathBuf, Option<(u64, u64, i128)>)> {
    let passwd = env::guard::passwd_home().expect("sentinel: his home is findable");
    ["projects.json", "projects.lock"]
        .iter()
        .map(|f| passwd.join(".cauldron").join(f))
        .map(|p| {
            let s = stat(&p);
            (p, s)
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────
//  B1: register / reorder under isolation write only the isolated home
// ─────────────────────────────────────────────────────────────────────────

/// **REGISTER AND REORDER, DRIVEN THROUGH THE CHAT FACE'S KEYS, WRITE ONLY
/// THE ISOLATED HOME.** Outer: builds a stand-in for his home (`S/his`, with a
/// real registry in it) and runs the child with `HOME=S/his`,
/// `CAULDRON_PROTECTED_HOME=S/his`, `TMPDIR=S/tmp`. The stand-in's registry
/// and `.cauldron` listing are watched by `stat`, and so are his REAL
/// `~/.cauldron/projects.{json,lock}` (stat only). After the child:
/// - the stand-in is byte-for-byte unchanged (inode, size, mtime, listing);
/// - his real files are unchanged;
/// - the write LANDED, in the isolated home under `S/tmp` (positive).
#[test]
fn rel6a4b_register_and_reorder_under_isolation_write_only_the_isolated_home() {
    let s = scratch("registry");
    let his = s.path().join("his");
    let tmp = s.path().join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    seed_registry(&his, &["alpha", "bravo"]);
    let reg = his.join(".cauldron/projects.json");
    let before = (stat(&reg), listing(&his.join(".cauldron")));
    assert!(before.0.is_some(), "sentinel: the stand-in registry exists");
    let real_before = his_real_registry();

    let (status, out) = run_child_raw(
        "rel6a4b::rel6a4b_child_registry",
        &[
            ("CAULDRON_ISOLATED", "1"),
            ("HOME", his.to_str().unwrap()),
            (live::PROTECTED_HOME_ENV, his.to_str().unwrap()),
            ("TMPDIR", tmp.to_str().unwrap()),
        ],
        Duration::from_secs(90),
    );

    let after = (stat(&reg), listing(&his.join(".cauldron")));
    assert_eq!(after, before, "THE STAND-IN FOR HIS HOME WAS WRITTEN (stat/listing of {} changed)", his.display());
    assert_eq!(his_real_registry(), real_before, "his REAL ~/.cauldron/projects.{{json,lock}} changed");
    // The sentinels first (above); then the child's own verdict.
    assert!(status.success() && out.contains("1 passed"), "the child failed ({status}):\n{out}");
    let home = out
        .lines()
        .find_map(|l| l.split_once("REL6A4B-REGISTRY home=").map(|(_, h)| h.trim()))
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("the child never said where it wrote:\n{out}"));
    let tmp_c = tmp.canonicalize().unwrap();
    assert!(home.starts_with(&tmp) || home.starts_with(&tmp_c), "the face's home {} is not under the scratch TMPDIR {}", home.display(), tmp.display());
    assert_eq!(registry_names(&home), ["bravo", "alpha", "charlie"], "the register/reorder did not land in the isolated home");
}

/// Child. `HOME` is the stand-in for his home, which is protected.
#[test]
#[ignore = "run in its own process by rel6a4b_register_and_reorder_under_isolation_write_only_the_isolated_home"]
fn rel6a4b_child_registry() {
    if !is_child() {
        return;
    }
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tui::event::{Ev, Input};
    use tui_render::RootItem;

    let his = PathBuf::from(std::env::var("HOME").unwrap());
    let protected = live::protected_homes();
    assert!(protected.contains(&his), "sentinel: the stand-in is protected: {protected:?}");
    assert!(!live::is_isolated_root_of(&his, &protected), "sentinel: the stand-in is not an isolated root");
    let passwd = env::guard::passwd_home().expect("his home is findable");

    // What `main` hands `app::run` / `app::run_headless`.
    let home = live::face_home_string();
    let h = PathBuf::from(&home);
    assert!(!h.starts_with(&passwd) && !home.is_empty(), "the face's home {home:?} is his: refusing to seed it");
    if projects::Registry::load(&h).ok().flatten().is_none() {
        seed_registry(&h, &["alpha", "bravo"]);
    }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (handle, _trx) = tui::mirror::spawn_dead();
        let (tx, mut erx) = tokio::sync::mpsc::channel::<Ev>(16);
        let mut app = tui::app::App::new(handle, tx, "ws://127.0.0.1:1/ws".into(), home.clone(), 0);
        let key = |app: &mut tui::app::App, code: KeyCode, mods: KeyModifiers| {
            app.apply(Ev::Input(vec![Input::Key(KeyEvent::new(code, mods))]));
        };
        async fn settle(app: &mut tui::app::App, erx: &mut tokio::sync::mpsc::Receiver<Ev>) {
            let ev = tokio::time::timeout(Duration::from_secs(10), erx.recv())
                .await
                .expect("the registry act never reported back")
                .expect("the channel closed");
            app.apply(ev);
        }
        // ^R re-reads the registry the face was handed.
        key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        let names: Vec<String> = app.state().projects.rows.iter().map(|r| r.name.clone()).collect();
        assert_eq!(&names[..2], ["alpha", "bravo"], "WITNESS: the face is not drawing the registry it was handed: {names:?}");

        // REORDER: ctrl-↑ on bravo (cursor starts at the top, row 0).
        let list = &app.state().projects;
        let at = (0..list.selectable()).find(|&i| list.item(i) == Some(RootItem::Project(1))).expect("bravo's row");
        for _ in 0..at {
            key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        }
        key(&mut app, KeyCode::Up, KeyModifiers::CONTROL);
        settle(&mut app, &mut erx).await;

        // REGISTER: `p` on a project row opens the add field; a path registers.
        let target = h.join("code").join("charlie");
        std::fs::create_dir_all(&target).unwrap();
        key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
        for c in target.to_string_lossy().chars() {
            key(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        settle(&mut app, &mut erx).await;
        let notice = app.state().notice.as_ref().map(|n| n.text.clone()).unwrap_or_default();
        assert!(notice.contains("done"), "the register did not complete: {notice:?}");

        // THE SECOND LAYER: a registry write handed his (stand-in) home
        // directly is refused before anything is read, locked or written.
        // A NEW project (made in the isolated home), so a missing refusal
        // really writes the stand-in and the outer stat sentinel sees it.
        let delta = h.join("code").join("delta");
        std::fs::create_dir_all(&delta).unwrap();
        let w = tui::acts::RegistryWrite::Register { path: delta, name: Some("delta".into()) };
        let e = tui::index::apply_registry(his.to_str().unwrap(), &w).expect_err("a registry write into his home was not refused");
        assert!(e.contains("refused") && e.contains("isolated"), "{e}");
    });
    println!("REL6A4B-REGISTRY home={home}");
}

// ─────────────────────────────────────────────────────────────────────────
//  N3: the scratch home never resolves under his home
// ─────────────────────────────────────────────────────────────────────────

/// [`live::scratch_home_from`]: a `$TMPDIR` under (or above) a protected home
/// falls back; a scratch `$TMPDIR` is kept; both bad is `None`.
#[test]
fn rel6a4b_scratch_home_table() {
    let his = scratch("his");
    let his = his.path();
    let ok = scratch("ok");
    let ok = ok.path();
    let fb = scratch("fallback");
    let fb = fb.path();
    let p: &[PathBuf] = &[his.to_path_buf()];
    let name = "tui-isolated-home-7";
    assert_eq!(live::scratch_home_from(ok, fb, 7, p), Some(ok.join(name)), "a scratch TMPDIR is kept");
    assert_eq!(live::scratch_home_from(&his.join("tmp"), fb, 7, p), Some(fb.join(name)), "TMPDIR inside his home falls back");
    assert_eq!(live::scratch_home_from(his, fb, 7, p), Some(fb.join(name)), "TMPDIR = his home falls back");
    assert_eq!(live::scratch_home_from(&his.join("tmp"), &his.join("fb"), 7, p), None, "both under his home: none");
    assert_eq!(live::scratch_home_from(ok, fb, 7, &[]), None, "his home unknown: none");
}

/// Outer: a child with `TMPDIR` INSIDE the stand-in for his home. Its face
/// home must land under [`live::SCRATCH_FALLBACK`], and nothing may appear
/// in the stand-in.
#[test]
fn rel6a4b_a_tmpdir_under_his_home_never_makes_the_scratch_home() {
    let s = scratch("tmpdir");
    let his = s.path().join("his");
    let tmp_in_his = his.join("tmp");
    std::fs::create_dir_all(&tmp_in_his).unwrap();
    let before = listing(&tmp_in_his);
    let out = run_child(
        "rel6a4b::rel6a4b_child_tmpdir",
        &[
            ("CAULDRON_ISOLATED", "1"),
            ("HOME", his.to_str().unwrap()),
            (live::PROTECTED_HOME_ENV, his.to_str().unwrap()),
            ("TMPDIR", tmp_in_his.to_str().unwrap()),
        ],
        Duration::from_secs(60),
    );
    assert!(out.contains("REL6A4B-TMPDIR ok"), "{out}");
    assert_eq!(listing(&tmp_in_his), before, "the scratch home was made inside the stand-in's TMPDIR");
}

/// Child.
#[test]
#[ignore = "run in its own process by rel6a4b_a_tmpdir_under_his_home_never_makes_the_scratch_home"]
fn rel6a4b_child_tmpdir() {
    if !is_child() {
        return;
    }
    let his = PathBuf::from(std::env::var("HOME").unwrap());
    assert!(std::env::temp_dir().starts_with(&his), "sentinel: TMPDIR is inside the stand-in");
    let home = live::face_home().expect("a scratch home");
    assert!(!home.starts_with(&his), "the face home {} is inside the stand-in {}", home.display(), his.display());
    assert!(home.starts_with(live::SCRATCH_FALLBACK), "the face home {} is not under {}", home.display(), live::SCRATCH_FALLBACK);
    let _ = std::fs::remove_dir(&home);
    println!("REL6A4B-TMPDIR ok");
}

// ─────────────────────────────────────────────────────────────────────────
//  N1: TUI_DIAG under isolation
// ─────────────────────────────────────────────────────────────────────────

/// [`tui::diag::path_for_from`] over explicit inputs.
#[test]
fn rel6a4b_tui_diag_table() {
    let h = scratch("diaghome");
    let h = h.path();
    let fb = h.join(".tui/diag.log");
    let iso = Some(("a test", Some(h)));
    let f = |v: Option<&str>, iso| tui::diag::path_for_from(v, fb.clone(), iso);
    // Isolated: under the isolated home → kept; anywhere else → refused.
    let inside = h.join("x/diag.log");
    assert_eq!(f(Some(inside.to_str().unwrap()), iso), Ok(Some(inside.clone())), "under the isolated home");
    assert!(f(Some("/private/tmp/elsewhere/diag.log"), iso).is_err(), "outside the isolated home");
    assert!(f(Some("rel/diag.log"), iso).is_err(), "relative");
    let escape = format!("{}/../escape.log", h.display());
    assert!(f(Some(&escape), iso).is_err(), "`..` out of the isolated home");
    assert!(f(Some(inside.to_str().unwrap()), Some(("a test", None))).is_err(), "no isolated home");
    // Unchanged: off, unset, and his face (not isolated).
    assert_eq!(f(Some("off"), iso), Ok(None));
    assert_eq!(f(None, iso), Ok(Some(fb.clone())));
    assert_eq!(f(Some("/anywhere/diag.log"), None), Ok(Some(PathBuf::from("/anywhere/diag.log"))), "his face");
}

/// **THE BUILT FACE, HEADLESS AND ISOLATED, REFUSES A `TUI_DIAG` OUTSIDE ITS
/// HOME AND HONOURS ONE INSIDE IT.** Through [`crate::face::Face`] (scratch
/// HOME, port 1). Both arms in one test, so a constant answer fails one.
#[test]
fn rel6a4b_headless_face_refuses_a_tui_diag_outside_its_isolated_home() {
    let s = scratch("diag");
    let elsewhere = s.path().join("elsewhere/diag.log");
    let inside = s.path().join("home/.tui/diag-in.log");
    for (target, honoured) in [(&elsewhere, false), (&inside, true)] {
        let mut f = crate::face::Face::new(s.path());
        f.env("TUI_DIAG", target).args(["--headless", "--seconds", "1"]).timeout(Duration::from_secs(60));
        let o = f.output().expect("the face runs");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "{target:?}: {} {err}", o.status);
        assert_eq!(target.exists(), honoured, "TUI_DIAG={} exists={} (stderr: {err})", target.display(), target.exists());
        assert_eq!(err.contains("refused: TUI_DIAG"), !honoured, "{target:?}: stderr {err}");
    }
}

// ─────────────────────────────────────────────────────────────────────────
//  N2: the pool board's set-live-login child under isolation
// ─────────────────────────────────────────────────────────────────────────

/// [`tui::pool::action::isolated_set_live_refusal_from`]: the default tool
/// (in this repo, under his home) and any tool under his home are refused in
/// an isolated process; a stub in a scratch dir is not; his board is never
/// refused.
#[test]
fn rel6a4b_set_live_refusal_table() {
    use tui::pool::action::isolated_set_live_refusal_from as r;
    let passwd = env::guard::passwd_home().expect("his home");
    let p: &[PathBuf] = &[passwd.clone()];
    let stub = scratch("stub");
    let stub = stub.path().join("claude-accounts");
    let real = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../services/threads/scripts/claude-accounts");
    assert!(real.starts_with(&passwd), "sentinel: the default tool is under his home");
    assert!(r(Some("a test"), &real, p).is_some(), "the default tool");
    assert!(r(Some("a test"), &passwd.join("bin/claude-accounts"), p).is_some(), "a copy under his home");
    assert_eq!(r(Some("a test"), &stub, p), None, "a stub in scratch");
    assert_eq!(r(None, &real, p), None, "his board");
}

/// **`perform(SetLive)` IN AN ISOLATED PROCESS NEVER RUNS A TOOL UNDER HIS
/// HOME.** Child (it sets `TUI_POOL_ACCOUNTS_TOOL`, process-wide): the tool is
/// a recording stub placed under the STAND-IN for his home (protected), so a
/// forced-red run records into the stand-in, never touches his login.
/// Opposed: the same stub copied to a scratch dir runs.
#[test]
fn rel6a4b_set_live_child_is_refused_under_isolation() {
    let s = scratch("setlive");
    let his = s.path().join("his");
    std::fs::create_dir_all(his.join("bin")).unwrap();
    let out = run_child(
        "rel6a4b::rel6a4b_child_set_live",
        &[("CAULDRON_ISOLATED", "1"), ("HOME", his.to_str().unwrap()), (live::PROTECTED_HOME_ENV, his.to_str().unwrap())],
        Duration::from_secs(60),
    );
    assert!(out.contains("REL6A4B-SETLIVE ok"), "{out}");
}

#[test]
#[ignore = "run in its own process by rel6a4b_set_live_child_is_refused_under_isolation"]
fn rel6a4b_child_set_live() {
    if !is_child() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    use tui::pool::{client, perform, Outcome};
    let his = PathBuf::from(std::env::var("HOME").unwrap());
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    let plant = |dir: &Path| -> (PathBuf, PathBuf) {
        let tool = dir.join("claude-accounts");
        let log = dir.join("calls.log");
        std::fs::write(&tool, format!("#!/bin/sh\necho \"$*\" >> '{}'\necho \"--dry-run: stopping here\"\n", log.display())).unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        (tool, log)
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let h = client::spawn("ws://127.0.0.1:1/ws".into(), tx);
        let a = tui::pool::action::Action::SetLive("beta".into());
        // Under the (protected) stand-in → refused, and the stub never ran.
        let (tool, log) = plant(&his.join("bin"));
        std::env::set_var("TUI_POOL_ACCOUNTS_TOOL", &tool);
        let o = perform(&h, &a).await;
        assert!(matches!(&o, Outcome::Done(n, _) if n.contains("refused: set-live-login")), "{o:?}");
        assert!(!log.exists(), "the set-live tool under his home RAN from an isolated process");
        // Opposed: a stub in scratch runs (the preflight reaches its stop).
        let sc = tempfile::tempdir().unwrap();
        let (tool, log) = plant(sc.path());
        std::env::set_var("TUI_POOL_ACCOUNTS_TOOL", &tool);
        let o = perform(&h, &a).await;
        assert!(matches!(&o, Outcome::NeedsTerminal(_)), "{o:?}");
        assert!(log.exists(), "the scratch stub did not run");
    });
    println!("REL6A4B-SETLIVE ok");
}

// ─────────────────────────────────────────────────────────────────────────
//  N6: a Face launch's limit kills its group and descendants; output()
//      cannot hang on a descendant's pipe
// ─────────────────────────────────────────────────────────────────────────

fn alive(pid: u32) -> bool {
    Command::new("/bin/ps").args(["-p", &pid.to_string()]).stdout(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn panic_text(p: Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
}

fn read_pid(p: &Path) -> u32 {
    let t0 = Instant::now();
    loop {
        if let Some(n) = std::fs::read_to_string(p).ok().and_then(|s| s.trim().parse().ok()) {
            return n;
        }
        assert!(t0.elapsed() < Duration::from_secs(10), "{} never written", p.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Runs `f` on a thread, expecting it to panic within `within`; the text.
fn expect_panic(within: Duration, f: impl FnOnce() + Send + 'static) -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)));
    });
    let r = rx.recv_timeout(within).unwrap_or_else(|_| panic!("still running {within:?} later: the launch hung"));
    panic_text(r.expect_err("the launch did not fail the test"))
}

/// **AT THE LIMIT, THE LAUNCH'S GRANDCHILDREN DIE TOO** — one in the
/// launch's own process group, one that left it (`set -m`: a job-control
/// shell puts a background job in a group of its own, as `script(1)` / a pty
/// driver put the face in a session of its own). A sibling with the same
/// command line, not the helper's, survives.
#[test]
fn rel6a4b_a_face_launch_past_its_limit_kills_its_group_and_descendants() {
    let dir = scratch("tree");
    let d = dir.path().to_path_buf();
    let (g, j) = (d.join("in-group.pid"), d.join("own-group.pid"));
    let script = format!(
        "/bin/sleep 7358.1 & echo $! > '{}'; set -m; /bin/sleep 7358.2 & echo $! > '{}'; wait",
        g.display(),
        j.display()
    );
    let mut sibling = Command::new("/bin/sleep").arg("7358.1").spawn().expect("sibling");
    let msg = expect_panic(Duration::from_secs(20), move || {
        let mut f = crate::face::Face::wrapped(&d, "/bin/sh", &["-c", &script]);
        f.timeout(Duration::from_millis(1500));
        let mut c = f.spawn().expect("spawn");
        let _ = c.wait();
    });
    assert!(msg.contains("FACE TIMED OUT"), "{msg}");
    let (gp, jp) = (read_pid(&g), read_pid(&j));
    std::thread::sleep(Duration::from_millis(200));
    let survivors: Vec<(&str, u32)> = [("in-group grandchild", gp), ("own-group grandchild", jp)].into_iter().filter(|(_, p)| alive(*p)).collect();
    for (_, p) in &survivors {
        let _ = Command::new("/bin/kill").args(["-KILL", &p.to_string()]).status();
    }
    assert!(survivors.is_empty(), "outlived the launch's limit: {survivors:?}");
    assert!(sibling.try_wait().unwrap().is_none(), "the sibling (pid {}) was killed: a kill by name", sibling.id());
    let _ = sibling.kill();
    let _ = sibling.wait();
}

/// **`output()` CANNOT HANG ON A PIPE A DESCENDANT HOLDS.** The launch exits
/// 0 at once and leaves a background `sleep` holding its stdout. Before
/// REL-6a4b `output()` joined the reader with no bound and hung for the
/// sleep's lifetime (7358 s); now it kills the group past
/// [`crate::face::PIPE_GRACE`] and fails, naming it. Opposed: a launch whose
/// pipes close normally returns its output.
#[test]
fn rel6a4b_face_output_cannot_hang_on_a_descendants_pipe() {
    let dir = scratch("pipe");
    let mut ok = crate::face::Face::wrapped(dir.path(), "/bin/sh", &["-c", "echo fine"]);
    ok.timeout(Duration::from_secs(10));
    let o = ok.output().expect("runs");
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "fine", "a clean launch's output was disturbed");

    let d = dir.path().to_path_buf();
    let pidf = d.join("holder.pid");
    let script = format!("/bin/sleep 7358.3 & echo $! > '{}'; exit 0", pidf.display());
    let t0 = Instant::now();
    let msg = expect_panic(crate::face::PIPE_GRACE + Duration::from_secs(15), move || {
        let mut f = crate::face::Face::wrapped(&d, "/bin/sh", &["-c", &script]);
        f.timeout(Duration::from_secs(60));
        let _ = f.output();
    });
    assert!(msg.contains("FACE LEFT A DESCENDANT HOLDING ITS OUTPUT"), "{msg}");
    assert!(t0.elapsed() < crate::face::PIPE_GRACE + Duration::from_secs(10), "took {:?}", t0.elapsed());
    let holder = read_pid(&pidf);
    std::thread::sleep(Duration::from_millis(200));
    let up = alive(holder);
    if up {
        let _ = Command::new("/bin/kill").args(["-KILL", &holder.to_string()]).status();
    }
    assert!(!up, "the descendant holding the pipe (pid {holder}) was not killed");
}

// ─────────────────────────────────────────────────────────────────────────
//  The census: every home lookup in src/ is face_home or a CHECKED read
// ─────────────────────────────────────────────────────────────────────────

/// What a home lookup is, and what the census CHECKS about it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    /// `face_home` itself.
    Router,
    /// The homes to PROTECT (never written under): its fn writes nothing.
    Protect,
    /// A read of his assets: its fn writes nothing.
    Read,
    /// A string for the eye: its fn writes nothing AND returns `String`.
    Display,
    /// Text inside an assertion message, not a lookup: the line is an `assert`.
    Text,
}

/// Every needle that is a home lookup (or names one). Matched per LINE.
const NEEDLES: &[&str] = &["home_dir(", "dirs::", "\"HOME\"", "$HOME", "passwd_home(", "operator_homes(", "home::", "BaseDirs", "UserDirs"];

/// Calls that write, create, remove, rename or spawn.
const WRITES: &[&str] = &[
    "fs::write", "create_dir", "File::create", "OpenOptions", "rename(", "remove_file", "remove_dir", "apply_registry",
    "Registry::update", ".save(", "Command::new", "copy(",
];

/// A comment line: `//…`, or a block comment's `/*`, ` * …`, `*` or `*/`.
/// NOT any line starting with `*` — that is also a deref assignment
/// (`*slot = std::env::var("HOME")…`), which the census must see (MERGE-15,
/// REL-6a4b-I F1).
fn is_comment(t: &str) -> bool {
    t.starts_with("//") || t.starts_with("/*") || t.starts_with("*/") || t == "*" || t.starts_with("* ")
}

fn is_hit(line: &str) -> bool {
    let t = line.trim_start();
    if is_comment(t) {
        return false;
    }
    NEEDLES.iter().any(|n| line.contains(n)) || (t.starts_with("use ") && line.contains("home_dir"))
}

/// The nearest `fn <name>` at or above line `i`, and its line index.
fn enclosing_fn(lines: &[&str], i: usize) -> Option<(String, usize)> {
    (0..=i).rev().find_map(|k| {
        let l = lines[k];
        let at = l.find("fn ")?;
        if at > 0 && l.as_bytes()[at - 1].is_ascii_alphanumeric() {
            return None;
        }
        let name: String = l[at + 3..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        (!name.is_empty()).then_some((name, k))
    })
}

/// The CODE of the fn starting at line `k`: signature through its matching
/// `}`, comment lines dropped (a comment naming a write is not a write — the
/// first forcing of this census went red on main.rs's own comment).
fn fn_text(lines: &[&str], k: usize) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    let mut opened = false;
    for l in &lines[k..] {
        let t = l.trim_start();
        if is_comment(t) {
            continue;
        }
        out.push_str(l);
        out.push('\n');
        for c in l.chars() {
            match c {
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        if opened && depth <= 0 {
            break;
        }
    }
    out
}

fn writes(body: &str) -> Vec<&'static str> {
    WRITES.iter().copied().filter(|w| body.contains(w)).collect()
}

/// **EVERY HOME LOOKUP IN `src/` IS `face_home` OR A CHECKED READ.** Each hit
/// is attributed to its enclosing fn and must be listed by (file, fn) with a
/// KIND, and the kind is CHECKED, not trusted: a `Read` / `Protect` /
/// `Display` fn may contain no write, spawn or registry call, and a `Display`
/// fn must return `String`. So the REL-6a4-I B1 row — main.rs `run`'s home,
/// called "display" while it fed `apply_registry` — can no longer be written:
/// `run` returns `Result`, and it is not `tilde`. Counts are exact per
/// (file, fn).
#[test]
fn rel6a4b_every_home_lookup_in_src_is_face_home_or_a_checked_read() {
    let allowed: &[(&str, &str, usize, Kind, &str)] = &[
        ("cauldron/live.rs", "face_home", 1, Kind::Router, "the router: $HOME for his face, an isolated home under isolation"),
        ("cauldron/live.rs", "protected_homes", 1, Kind::Protect, "his passwd home: the home an isolated process may NOT write under"),
        ("cauldron/artpage.rs", "tilde", 1, Kind::Display, "a path written from `~` for a notice or tag; every other tilde calls this one"),
        ("mic.rs", "model_path", 1, Kind::Read, "finds the whisper model file, which whisper opens read-only"),
        ("mic.rs", "stt_default", 1, Kind::Read, "the retranscribe tool's path in the default STT command line (it reads only, REL-6a4-I)"),
        ("mic.rs", "aui_is_capturing", 1, Kind::Read, "lists the AUI's ~/.aui/captures to see whether it is recording"),
        ("mic.rs", "the_model_default_is_large_and_the_override_still_wins", 1, Kind::Read, "cfg(test): the model candidates the default resolves to"),
        ("store.rs", "the_list_puts_the_newest_thread_at_the_bottom", 1, Kind::Text, "an assertion's message says `$HOME`"),
    ];
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    // SENTINELS over the same predicates the census uses.
    for l in ["let h = std::env::home_dir();", "use dirs::home_dir;", "x(\"$HOME/y\")", "std::env::var_os(\"HOME\")", "dirs::data_dir()", "env::guard::passwd_home()"] {
        assert!(is_hit(l), "sentinel: the needle set misses {l:?}");
    }
    assert!(!is_hit("    // dirs::home_dir() in a comment"), "sentinel: a comment is not a lookup");
    assert!(is_hit("        *slot = std::env::var(\"HOME\").ok();"), "sentinel: a deref assignment is code (F1)");
    assert!(!is_hit("     * dirs::home_dir() in a block comment"), "sentinel: a block comment line is not a lookup");
    let index = std::fs::read_to_string(src.join("index.rs")).unwrap();
    let il: Vec<&str> = index.lines().collect();
    let k = il.iter().position(|l| l.contains("pub fn apply_registry(")).expect("sentinel: apply_registry");
    assert!(writes(&fn_text(&il, k)).contains(&"Registry::update"), "sentinel: the write check cannot see apply_registry's write");
    let main = std::fs::read_to_string(src.join("main.rs")).unwrap();
    let ml: Vec<&str> = main.lines().collect();
    let k = ml.iter().position(|l| l.starts_with("async fn run(")).expect("sentinel: main.rs run");
    let run = fn_text(&ml, k);
    assert!(run.contains("app::run(") && run.contains("app::run_headless("), "sentinel: main.rs run's body was not extracted whole");
    assert!(run.matches("face_home_string()").count() == 2, "main.rs hands app::run / run_headless something other than face_home_string()");

    let mut found: std::collections::BTreeMap<(String, String), usize> = Default::default();
    let mut stack = vec![src.clone()];
    let mut files = 0;
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().is_none_or(|x| x != "rs") {
                continue;
            }
            files += 1;
            let rel = p.strip_prefix(&src).unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&p).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            for (i, l) in lines.iter().enumerate() {
                if !is_hit(l) {
                    continue;
                }
                let (f, k) = enclosing_fn(&lines, i).unwrap_or_else(|| ("<no fn>".into(), i));
                *found.entry((rel.clone(), f.clone())).or_default() += 1;
                let Some((_, _, _, kind, _)) = allowed.iter().find(|(af, an, ..)| *af == rel && *an == f) else {
                    continue;
                };
                let body = fn_text(&lines, k);
                let sig = lines[k];
                match kind {
                    Kind::Router => {}
                    Kind::Protect | Kind::Read => {
                        assert!(writes(&body).is_empty(), "{rel}::{f} is listed {kind:?} but writes: {:?}", writes(&body))
                    }
                    Kind::Display => {
                        assert!(writes(&body).is_empty(), "{rel}::{f} is listed Display but writes: {:?}", writes(&body));
                        assert!(sig.contains("-> String"), "{rel}::{f} is listed Display but does not return String: {sig}");
                    }
                    Kind::Text => assert!(l.trim_start().starts_with("assert"), "{rel}:{} is listed Text but is not an assertion: {l}", i + 1),
                }
            }
        }
    }
    assert!(files > 50, "sentinel: the walk found only {files} .rs files under {}", src.display());
    assert!(found.contains_key(&("cauldron/live.rs".to_string(), "face_home".to_string())), "sentinel: the router's own lookup is attributed to face_home: {found:?}");
    let want: std::collections::BTreeMap<(String, String), usize> = allowed.iter().map(|(f, n, c, ..)| ((f.to_string(), n.to_string()), *c)).collect();
    assert_eq!(found, want, "a home lookup in src/ that is neither face_home nor a listed, checked read (allowlist: {allowed:#?})");
}
