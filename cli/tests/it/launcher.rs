//! **THE PATH `cauldron` RUNS THE BINARY ITS CONFIG NAMES** (R9.CFG1,
//! implements E18-19). Drives the real `cauldron-launcher` binary against a
//! scratch cauldron root (`CAULDRON_HOME`), never the operator's `~/.cauldron`.
//!
//! THE WITNESS IS NOT THE LAUNCHER. The fake interface is a shell script that
//! writes its own argv to a file the launcher never touches; the test reads
//! that file. A launcher that printed the right thing without exec'ing would
//! leave the file absent.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const LAUNCHER: &str = env!("CARGO_BIN_EXE_cauldron-launcher");

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cauldron-launcher-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A fake cauldron: writes one argv element per line to `<dir>/argv.out` and
/// exits 7, so both the argv and the exit code are the CHILD's.
fn fake_interface(dir: &Path) -> PathBuf {
    let p = dir.join("fake-cauldron");
    let out = dir.join("argv.out");
    std::fs::write(
        &p,
        format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\nexit 7\n", out.display()),
    )
    .unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

fn launch(root: &Path, args: &[&str], which: bool) -> Output {
    let mut c = Command::new(LAUNCHER);
    c.args(args).env("CAULDRON_HOME", root).env_remove("CAULDRON_LAUNCHER_WHICH");
    if which {
        c.env("CAULDRON_LAUNCHER_WHICH", "1");
    }
    c.output().expect("spawn launcher")
}

fn set_key(root: &Path, value: &str) {
    std::fs::write(root.join("config.toml"), format!("[cli]\ninterface = {value:?}\n")).unwrap();
}

#[test]
fn set_key_execs_that_binary_with_the_same_argv_and_its_exit_code() {
    let root = scratch("set");
    let fake = fake_interface(&root);
    set_key(&root, fake.to_str().unwrap());
    let args = ["status", "--env", "dev", "two words", ""];
    let o = launch(&root, &args, false);
    assert_eq!(o.status.code(), Some(7), "the child's exit code, not the launcher's: {o:?}");
    let got = std::fs::read_to_string(root.join("argv.out")).expect("the fake ran and wrote its argv");
    let want: String = args.iter().map(|a| format!("{a}\n")).collect();
    assert_eq!(got, want);
}

#[test]
fn absent_key_resolves_to_the_globs2_demo_build() {
    for (tag, file) in [("nofile", None), ("nokey", Some("[cli]\n"))] {
        let root = scratch(tag);
        if let Some(t) = file {
            std::fs::write(root.join("config.toml"), t).unwrap();
        }
        let o = launch(&root, &[], true);
        let out = String::from_utf8_lossy(&o.stdout);
        let err = String::from_utf8_lossy(&o.stderr);
        // Read from the crate constant so the two cannot drift (ENV-1b); the
        // constant itself is pinned to the live PATH shim by
        // libs/env `default_equals_what_the_path_shim_execs_today`.
        let globs2 = env::cli_config::GLOBS2_BINARY;
        if Path::new(globs2).exists() {
            assert_eq!(o.status.code(), Some(0), "{tag}: {err}");
            assert!(out.starts_with(&format!("binary={globs2} origin=default")), "{tag}: {out}");
        } else {
            assert_eq!(o.status.code(), Some(125), "{tag}: {out}");
            assert!(err.contains(globs2) && err.contains("built-in default"), "{tag}: {err}");
        }
    }
}

#[test]
fn set_key_which_reports_the_key_not_the_default() {
    let root = scratch("which");
    let fake = fake_interface(&root);
    set_key(&root, fake.to_str().unwrap());
    let o = launch(&root, &[], true);
    let out = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    assert!(out.starts_with(&format!("binary={} origin=[cli] interface", fake.display())), "{out}");
    assert!(!root.join("argv.out").exists(), "WHICH must not run the interface");
}

#[test]
fn bad_path_exits_125_naming_the_key_and_runs_nothing() {
    let root = scratch("bad");
    let missing = root.join("no-such-cauldron");
    set_key(&root, missing.to_str().unwrap());
    let o = launch(&root, &["status"], false);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(125), "{err}");
    assert!(err.starts_with("cauldron-launcher: "), "{err}");
    assert!(err.contains("[cli] interface"), "names the key: {err}");
    assert!(err.contains(&missing.display().to_string()), "names the path: {err}");
    assert!(err.contains(&root.join("config.toml").display().to_string()), "names the file: {err}");
}

#[test]
fn key_naming_the_launcher_itself_is_refused_not_looped() {
    let root = scratch("self");
    set_key(&root, LAUNCHER);
    let o = launch(&root, &[], false);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(125), "{err}");
    assert!(err.contains("the launcher itself") && err.contains("[cli] interface"), "{err}");
}
