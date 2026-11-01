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
        "#!/bin/sh\necho \"FACE env=$CAULDRON_ENV src=$CAULDRON_ENV_SOURCE home=$CAULDRON_HOME bin=$CAULDRON_BIN args=$*\"\n",
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
    // …and says it was the DEFAULT, so the face may open the last one he
    // used instead (V1.2); a name he gave says where it came from.
    assert!(face_line(&out).contains("src=default "), "{out}");
    let (_, out) = run(&s.0, &["tui"], Some("faceb"));
    assert!(face_line(&out).contains("src=$CAULDRON_ENV "), "{out}");
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

/// **V1.2b: the cauldron the face opens by default runs in ITS OWN build.**
/// A bare `cauldron tui` opens the last-used cauldron (V1.2, Q-F1); the router
/// must then launch that environment's `tui-<env>`, by the same R6.S8 rule a
/// named environment uses, not prod's plain `tui` it defaulted to.
///
/// The root holds `prod` and `alpha`, and `bin/` holds both `tui` (PLAIN)
/// and `tui-alpha` (OWN-ALPHA). The arms oppose: a router that ignores the
/// record runs PLAIN on the first arm; one that always follows the record (or
/// always picks a suffixed file) runs OWN-ALPHA on a later one, where a name he
/// gave, a gone record, no record, or the chat face must each run plain `tui`.
#[test]
fn the_last_used_cauldron_opens_in_its_own_tui_env_build() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let s = scratch();
    let root = s.0.join("root");
    std::fs::create_dir_all(root.join("data").join("prod")).unwrap();
    std::fs::create_dir_all(root.join("data").join("alpha")).unwrap();
    let plain = s.0.join("bin").join("tui");
    std::fs::write(&plain, "#!/bin/sh\necho \"FACE PLAIN env=$CAULDRON_ENV src=$CAULDRON_ENV_SOURCE args=$*\"\n").unwrap();
    let own = s.0.join("bin").join("tui-alpha");
    std::fs::write(&own, "#!/bin/sh\necho \"FACE OWN-ALPHA env=$CAULDRON_ENV src=$CAULDRON_ENV_SOURCE args=$*\"\n").unwrap();
    assert!(Command::new("/bin/chmod").arg("+x").arg(&plain).arg(&own).status().unwrap().success());
    let record = root.join("data").join("shared").join("tui").join("last_env.json");
    let set_record = |name: Option<&str>| match name {
        Some(n) => {
            std::fs::create_dir_all(record.parent().unwrap()).unwrap();
            std::fs::write(&record, format!("{{\n  \"env\": \"{n}\"\n}}")).unwrap();
        }
        None => {
            let _ = std::fs::remove_file(&record);
        }
    };

    // Sentinel: with nothing recorded, bare `tui` opens prod in plain `tui`.
    set_record(None);
    let (rc, out) = run(&s.0, &["tui"], None);
    assert_eq!(rc, 0, "{out}");
    assert!(face_line(&out).starts_with("FACE PLAIN env=prod src=default "), "no record: prod's plain tui: {out}");

    // The fix: alpha last used, nothing named -> alpha's own build. The face
    // still hears the defaulted name and its source, so it picks alpha itself.
    set_record(Some("alpha"));
    let (rc, out) = run(&s.0, &["tui"], None);
    assert_eq!(rc, 0, "{out}");
    assert!(
        face_line(&out).starts_with("FACE OWN-ALPHA env=prod src=default args=--cauldron"),
        "V1.2b: the last-used cauldron must open in its own tui-alpha: {out}"
    );
    // …bare `cauldron` too.
    let (rc, out) = run(&s.0, &[], None);
    assert_eq!(rc, 0, "{out}");
    assert!(face_line(&out).starts_with("FACE OWN-ALPHA "), "bare cauldron: {out}");

    // A name he gave wins over the record: router flag, variable, face flag.
    let (_, out) = run(&s.0, &["--env", "prod", "tui"], None);
    assert!(face_line(&out).starts_with("FACE PLAIN env=prod src=--env "), "--env prod: {out}");
    let (_, out) = run(&s.0, &["tui"], Some("prod"));
    assert!(face_line(&out).starts_with("FACE PLAIN "), "CAULDRON_ENV=prod: {out}");
    let (_, out) = run(&s.0, &["tui", "--env", "prod"], None);
    assert!(face_line(&out).starts_with("FACE PLAIN "), "face --env prod: {out}");
    // …and the face's own `--env alpha` runs alpha's build with no record at all.
    set_record(None);
    let (_, out) = run(&s.0, &["tui", "--env", "alpha"], None);
    assert!(face_line(&out).starts_with("FACE OWN-ALPHA "), "face --env alpha: {out}");

    // A record naming a cauldron that is gone is passed over: prod, plain —
    // even though `tui-alpha` is still installed beside the router.
    set_record(Some("alpha"));
    std::fs::rename(root.join("data").join("alpha"), s.0.join("alpha-away")).unwrap();
    let (_, out) = run(&s.0, &["tui"], None);
    assert!(face_line(&out).starts_with("FACE PLAIN env=prod "), "gone record: {out}");
    std::fs::rename(s.0.join("alpha-away"), root.join("data").join("alpha")).unwrap();

    // The chat face never opens a cauldron: it stays in the resolved build.
    set_record(Some("alpha"));
    let (_, out) = run(&s.0, &["tui", "--chat"], None);
    assert!(face_line(&out).starts_with("FACE PLAIN "), "--chat: {out}");

    // Asking wrote nothing: the record is still alpha's, and no env was made.
    assert!(std::fs::read_to_string(&record).unwrap().contains("\"alpha\""));
    assert_eq!(std::fs::read_dir(root.join("data")).unwrap().count(), 3, "prod, alpha, shared only");
}

/// **V1.2d: a bare cauldron name opens in ITS OWN build too.** The face takes
/// `tui --cauldron [--env <name> | <name>]`; the router reads the name the
/// same way (`env::face::named_in`), so `cauldron tui prod` with alpha last
/// used runs prod's plain `tui`, and `cauldron tui alpha` runs `tui-alpha`
/// with no record at all. A flag's value is never taken for a name. And the
/// two `--env` edge cases V1.2bc-I named untested: `--env` with no value
/// names nothing (the resolved name's build, never the record's), and a name
/// given still runs its own build when the root cannot be listed.
///
/// The arms oppose: a router blind to the bare name runs OWN-ALPHA on the
/// first arm (the record) and PLAIN on the second; one that took any bare
/// word, flag values included, runs OWN-ALPHA on `--draft alpha`.
#[test]
fn a_bare_cauldron_name_opens_in_its_own_tui_env_build() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let s = scratch();
    let root = s.0.join("root");
    std::fs::create_dir_all(root.join("data").join("prod")).unwrap();
    std::fs::create_dir_all(root.join("data").join("alpha")).unwrap();
    let plain = s.0.join("bin").join("tui");
    std::fs::write(&plain, "#!/bin/sh\necho \"FACE PLAIN env=$CAULDRON_ENV src=$CAULDRON_ENV_SOURCE args=$*\"\n").unwrap();
    let own = s.0.join("bin").join("tui-alpha");
    std::fs::write(&own, "#!/bin/sh\necho \"FACE OWN-ALPHA env=$CAULDRON_ENV src=$CAULDRON_ENV_SOURCE args=$*\"\n").unwrap();
    assert!(Command::new("/bin/chmod").arg("+x").arg(&plain).arg(&own).status().unwrap().success());
    let record = root.join("data").join("shared").join("tui").join("last_env.json");
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(&record, "{\n  \"env\": \"alpha\"\n}").unwrap();

    // Sentinel: alpha recorded, nothing named -> alpha's own build.
    let (rc, out) = run(&s.0, &["tui"], None);
    assert_eq!(rc, 0, "{out}");
    assert!(face_line(&out).starts_with("FACE OWN-ALPHA "), "sentinel, the record: {out}");

    // F1: `cauldron tui prod` opens prod, so prod's plain build runs.
    let (rc, out) = run(&s.0, &["tui", "prod"], None);
    assert_eq!(rc, 0, "{out}");
    assert!(face_line(&out).starts_with("FACE PLAIN env=prod src=default args=--cauldron prod"), "tui prod: {out}");
    // …among other flags too.
    let (_, out) = run(&s.0, &["tui", "--dump", "prod", "--tab", "status"], None);
    assert!(face_line(&out).starts_with("FACE PLAIN "), "tui --dump prod --tab status: {out}");

    // `cauldron tui alpha`, no record: alpha's own build.
    std::fs::remove_file(&record).unwrap();
    let (_, out) = run(&s.0, &["tui", "alpha"], None);
    assert!(face_line(&out).starts_with("FACE OWN-ALPHA "), "tui alpha, no record: {out}");
    // A flag's value is not a name: `--draft alpha` names nothing, so prod.
    let (_, out) = run(&s.0, &["tui", "--draft", "alpha"], None);
    assert!(face_line(&out).starts_with("FACE PLAIN "), "tui --draft alpha: {out}");

    // Edge (1): `--env` with no value names nothing — the resolved name's
    // build, never the record's, even with alpha recorded.
    std::fs::write(&record, "{\n  \"env\": \"alpha\"\n}").unwrap();
    let (_, out) = run(&s.0, &["tui", "--env"], None);
    assert!(face_line(&out).starts_with("FACE PLAIN env=prod "), "tui --env (no value): {out}");

    // Edge (2): the root cannot be listed (`data/` is mode 0300: it can be
    // entered, not read), a name given: that name's own build still runs;
    // with no name, the resolved one's. (A `data` that is a FILE is not this
    // case: the router cannot resolve an environment at all then.)
    std::fs::remove_file(&record).unwrap();
    let data = root.join("data");
    // Restored on drop, before the scratch is removed, even when an arm fails.
    struct Readable(std::path::PathBuf);
    impl Drop for Readable {
        fn drop(&mut self) {
            let _ = Command::new("/bin/chmod").arg("0755").arg(&self.0).status();
        }
    }
    assert!(Command::new("/bin/chmod").arg("0300").arg(&data).status().unwrap().success());
    let _readable = Readable(data.clone());
    assert!(std::fs::read_dir(&data).is_err(), "sentinel: data/ must be unlistable for this arm to mean anything");
    let (_, out) = run(&s.0, &["tui", "alpha"], None);
    assert!(face_line(&out).starts_with("FACE OWN-ALPHA "), "unlistable root, tui alpha: {out}");
    let (_, out) = run(&s.0, &["tui", "--env", "alpha"], None);
    assert!(face_line(&out).starts_with("FACE OWN-ALPHA "), "unlistable root, tui --env alpha: {out}");
    let (_, out) = run(&s.0, &["tui"], None);
    assert!(face_line(&out).starts_with("FACE PLAIN env=prod "), "unlistable root, nothing named: {out}");
}
