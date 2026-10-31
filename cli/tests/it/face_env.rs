//! **`cauldron [--env <name>] tui` HANDS THE FACE ITS ENVIRONMENT** (R7.S1c).
//!
//! The router execs the app found BESIDE its own binary, so the test copies the
//! built `cauldron` into a scratch directory and puts a fake `tui` beside it
//! that prints what it was handed. What is asserted is what the FACE sees:
//! `CAULDRON_ENV`, `CAULDRON_HOME`, `CAULDRON_BIN` and the passed-through args.
//!
//! The two cases oppose each other: the flag names one environment while the
//! process environment names another, and the face must see the flag's; with
//! no flag, it must see the variable's. A router that dropped the environment,
//! or always exported one fixed name, fails one of the two.

use std::path::{Path, PathBuf};
use std::process::Command;

/// **THE TWO TESTS RUN ONE AT A TIME.** Each copies a binary and then execs
/// it; with both in parallel, one thread's `fork` can inherit the OTHER's
/// still-open write descriptor on its freshly copied file, and that exec fails
/// with ETXTBSY. The forcing sweep hit exactly that (a panic in `chmod` /
/// `output`, 0.01 s in), which reads like a red and measures nothing.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> Scratch {
    let p = std::env::temp_dir().join(format!(
        "cauldron-face-env-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(p.join("bin")).unwrap();
    std::fs::create_dir_all(p.join("root")).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_cauldron"), p.join("bin").join("cauldron")).unwrap();
    let fake = p.join("bin").join("tui");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho \"FACE env=$CAULDRON_ENV home=$CAULDRON_HOME bin=$CAULDRON_BIN args=$*\"\n",
    )
    .unwrap();
    assert!(Command::new("/bin/chmod").arg("+x").arg(&fake).status().unwrap().success());
    Scratch(p)
}

fn run(s: &Path, args: &[&str], env_var: Option<&str>) -> (i32, String) {
    let mut c = Command::new(s.join("bin").join("cauldron"));
    c.args(args).env("CAULDRON_HOME", s.join("root")).env_remove("ENV");
    match env_var {
        Some(v) => c.env("CAULDRON_ENV", v),
        None => c.env_remove("CAULDRON_ENV"),
    };
    let o = c.output().expect("spawn the copied cauldron");
    (
        o.status.code().unwrap_or(-1),
        format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)),
    )
}

fn face_line(out: &str) -> &str {
    out.lines().find(|l| l.starts_with("FACE ")).unwrap_or_else(|| panic!("the face never ran: {out}"))
}

#[test]
fn the_env_flag_reaches_the_face_and_beats_the_variable() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let s = scratch();
    let (rc, out) = run(&s.0, &["--env", "facea", "tui", "--chat", "--headless", "--seconds", "1"], Some("faceb"));
    assert_eq!(rc, 0, "{out}");
    let line = face_line(&out);
    assert!(line.contains("env=facea "), "the flag's environment must reach the face: {line}");
    assert!(
        line.contains(&format!("home={} ", s.0.join("root").display())),
        "the root must reach the face: {line}"
    );
    assert!(
        line.contains(&format!("bin={} ", s.0.join("bin").join("cauldron").display())),
        "the router must name ITSELF as the cauldron the face runs: {line}"
    );
    assert!(line.ends_with("args=--headless --seconds 1"), "args pass through untouched: {line}");
}

#[test]
fn with_no_flag_the_variables_environment_reaches_the_face() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let s = scratch();
    let (rc, out) = run(&s.0, &["tui"], Some("faceb"));
    assert_eq!(rc, 0, "{out}");
    assert!(face_line(&out).contains("env=faceb "), "{out}");
    // `cauldron tui` opens the CAULDRON face by default (an internal thread).
    assert!(face_line(&out).ends_with("args=--cauldron"), "the default face is the cauldron face: {out}");
    // …and with neither, the default is `prod`, said explicitly.
    let (rc, out) = run(&s.0, &["tui"], None);
    assert_eq!(rc, 0, "{out}");
    assert!(face_line(&out).contains("env=prod "), "{out}");
}

/// **R6.S8: a non-prod environment's face is `tui-<env>`, prod's is `tui`.**
/// `cauldron --env <e> install` places the TUI at `<root>/bin/tui-<e>` so it
/// never lands on prod's `bin/tui`; this proves the router then FINDS it.
/// Both files sit side by side and print different markers, and the two runs
/// oppose: a router that always picks the suffixed file fails the prod run,
/// one that always picks the plain file fails the stage run.
#[test]
fn a_named_env_runs_its_own_tui_env_file_while_prod_runs_plain_tui() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let s = scratch();
    let own = s.0.join("bin").join("tui-stage");
    std::fs::write(&own, "#!/bin/sh\necho \"FACE OWN-STAGE-FACE env=$CAULDRON_ENV\"\n").unwrap();
    assert!(Command::new("/bin/chmod").arg("+x").arg(&own).status().unwrap().success());

    let (rc, out) = run(&s.0, &["--env", "stage", "tui"], None);
    assert_eq!(rc, 0, "{out}");
    assert!(face_line(&out).contains("OWN-STAGE-FACE env=stage"), "stage must run bin/tui-stage: {out}");

    let (rc, out) = run(&s.0, &["tui"], None);
    assert_eq!(rc, 0, "{out}");
    let line = face_line(&out);
    assert!(!line.contains("OWN-STAGE-FACE"), "prod must run plain bin/tui: {out}");
    assert!(line.contains("env=prod "), "{out}");
}
