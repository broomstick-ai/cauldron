//! `tts --build-stamp`: the commit, how it was obtained, whether the scoped
//! tree was dirty, and the scope — read off the binary cargo just built.

use std::process::Command;

fn stamp() -> std::collections::BTreeMap<String, String> {
    let out = Command::new(env!("CARGO_BIN_EXE_tts")).arg("--build-stamp").env_clear().output().unwrap();
    assert!(out.status.success(), "the stamp needs no environment at all");
    String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .split(' ')
        .map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).expect("k=v"))
        .collect()
}

fn git(args: &[&str]) -> String {
    let o = Command::new("git").arg("-C").arg(env!("CARGO_MANIFEST_DIR")).args(args).output().unwrap();
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn the_stamp_names_this_trees_commit_and_scope() {
    let s = stamp();
    assert_eq!(s.keys().map(String::as_str).collect::<Vec<_>>(), ["commit", "dirt_scope", "dirty", "provenance"]);
    assert_eq!(s["provenance"], "git", "nobody pinned this build");
    assert_eq!(s["commit"], git(&["rev-parse", "HEAD"]).trim());
    let scope: Vec<&str> = s["dirt_scope"].split(',').collect();
    assert_eq!(scope, ["Cargo.lock", "Cargo.toml", "libs/env", "services/tts"], "the path-dependency closure, and nothing else");
}

/// `dirty` agrees with git over exactly the published scope — `1` or `0`,
/// and never `unknown` in a tree git can read.
#[test]
fn the_dirt_flag_agrees_with_git_over_the_published_scope() {
    let s = stamp();
    let mut args = vec!["status", "--porcelain", "--untracked-files=normal", "--"];
    let top = git(&["rev-parse", "--show-toplevel"]);
    let paths: Vec<String> = s["dirt_scope"].split(',').map(|p| format!("{}/{p}", top.trim())).collect();
    args.extend(paths.iter().map(String::as_str));
    let dirty = !git(&args).trim().is_empty();
    assert_eq!(s["dirty"], if dirty { "1" } else { "0" });
}
