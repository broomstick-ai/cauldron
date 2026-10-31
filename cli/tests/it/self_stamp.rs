//! **THE SUPERVISOR STATES ITS OWN COMMIT.** `cauldron --build-stamp` prints
//! the one line `build` reads off every service binary, and `--version` the
//! same commit for a human. Before this arm existed the flag fell through to
//! the TUI, which printed `Error: unknown argument` and exited 1 — so the one
//! process that signs for everyone else's commit could not report its own.
//!
//! THE WITNESS IS NOT THE STAMP. Comparing the printed commit to
//! `env!("CAULDRON_SELF_COMMIT")` would be two reads of one value. The
//! independent witness is `git rev-parse HEAD` in the tree this test was
//! built from, run at test time; when the build was pinned rather than
//! sniffed (`provenance=pin`) the pin is the witness, read from the same
//! environment variable `build.rs` read.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

// The dirt parser `cli/build.rs` `include!`s — the SAME file, so these tests
// run the function the build script runs.
#[path = "../../../libs/env/src/build_dirt.rs"]
mod build_dirt;

fn cauldron(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_cauldron"))
        .args(args)
        .env_remove("CAULDRON_HOME")
        .env_remove("CAULDRON_ENV")
        .output()
        .expect("spawn cauldron");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn head_of_this_tree() -> String {
    let out = Command::new("git")
        .args(["-C", env!("CARGO_MANIFEST_DIR"), "rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    assert!(out.status.success(), "git rev-parse HEAD must work where the test was built");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn build_stamp_is_one_parseable_line_naming_a_real_commit() {
    let (rc, out, err) = cauldron(&["--build-stamp"]);
    assert_eq!(rc, 0, "exit 0; stderr was: {err}");
    assert!(err.is_empty(), "nothing on stderr — the old fall-through said `Error: unknown argument`: {err}");
    assert!(!out.contains("unknown argument"), "{out}");
    assert_eq!(out.lines().count(), 1, "exactly one line: {out:?}");

    let kv: Vec<(&str, &str)> = out
        .split_whitespace()
        .map(|p| p.split_once('=').unwrap_or_else(|| panic!("`{p}` is not key=value")))
        .collect();
    let get = |k: &str| kv.iter().find(|(kk, _)| *kk == k).map(|(_, v)| *v);

    let commit = get("commit").expect("commit= present");
    assert_eq!(commit.len(), 40, "a full sha, not a prefix: {commit}");
    assert!(commit.chars().all(|c| c.is_ascii_hexdigit()), "{commit}");
    // git works in this tree at test time, so the build's dirt must be KNOWN.
    let dirty = get("dirty").expect("dirty= present");
    assert!(matches!(dirty, "0" | "1"), "dirty={dirty}: git works here, so unknown is a defect");

    // ── THE DIRT WITNESS — `git status` run NOW over the published scope ──
    //
    // Never `env!("CAULDRON_SELF_DIRTY")`: two reads of one value are not a
    // witness. The witness does not parse porcelain either — "git printed
    // anything inside the scope" is the whole verdict — so a parser defect in
    // `build_dirt.rs` cannot hide on both sides at once.
    let scope: Vec<&str> = get("dirt_scope").expect("dirt_scope= present").split(',').collect();
    for must in ["Cargo.toml", "Cargo.lock", "cli", "libs/env"] {
        assert!(scope.contains(&must), "dirt_scope lacks {must}: {out}");
    }
    assert!(!scope.contains(&"unknown"), "the scope could not be derived: {out}");
    let top = Command::new("git")
        .args(["-C", env!("CARGO_MANIFEST_DIR"), "rev-parse", "--show-toplevel"])
        .output()
        .expect("git rev-parse --show-toplevel");
    let top = String::from_utf8_lossy(&top.stdout).trim().to_string();
    let st = Command::new("git")
        .args(["-C", &top, "status", "--porcelain", "--untracked-files=normal", "--"])
        .args(&scope)
        .output()
        .expect("git status");
    assert!(st.status.success(), "git status over the scope failed");
    let witness = if st.stdout.is_empty() { "0" } else { "1" };
    assert_eq!(
        dirty,
        witness,
        "the stamp says dirty={dirty} but git status NOW over {scope:?} says {witness}:\n{}",
        String::from_utf8_lossy(&st.stdout)
    );

    match get("provenance") {
        Some("git") => assert_eq!(commit, head_of_this_tree(), "sniffed commit is HEAD of the tree it was built in"),
        Some("pin") => {
            let pin = std::env::var("CAULDRON_BUILD_COMMIT_PIN").unwrap_or_default();
            assert_eq!(commit, pin.trim(), "pinned commit is the pin");
        }
        other => panic!("provenance must be git or pin for a stamped build, got {other:?}"),
    }
}

#[test]
fn version_names_the_same_commit_for_a_human() {
    let (rc_s, stamp, _) = cauldron(&["--build-stamp"]);
    let (rc, out, err) = cauldron(&["--version"]);
    assert_eq!((rc_s, rc), (0, 0), "{err}");
    let commit = stamp
        .split_whitespace()
        .find_map(|p| p.strip_prefix("commit="))
        .expect("commit= in the stamp");
    assert!(out.starts_with(&format!("cauldron {commit}")), "`{out}` must start with `cauldron {commit}`");
    for flag in ["-v", "-V"] {
        let (rc_f, out_f, _) = cauldron(&[flag]);
        assert_eq!((rc_f, out_f.as_str()), (0, out.as_str()), "{flag} is the same answer as --version");
    }
    // The human suffix and the machine field are two renderings of ONE flag
    // and must agree: nothing iff `dirty=0`, ` (dirty)` iff `dirty=1`, and
    // ` (dirt unknown)` for anything else — never a bare (clean-looking) line.
    let dirty = stamp
        .split_whitespace()
        .find_map(|p| p.strip_prefix("dirty="))
        .expect("dirty= in the stamp");
    let want = match dirty {
        "0" => format!("cauldron {commit}"),
        "1" => format!("cauldron {commit} (dirty)"),
        _ => format!("cauldron {commit} (dirt unknown)"),
    };
    assert_eq!(out.trim_end(), want, "--version disagrees with the stamp's dirty={dirty}");
}

// ══════════════════════════════════════════════════════════════════════════
//  THE DIRT VERDICT — fail-closed, untrimmed, scoped, refuses what it cannot
//  parse. Fixtures fed to the REAL parser, then a real `git status` in a real
//  repository shaped like this one.
// ══════════════════════════════════════════════════════════════════════════

#[test]
fn a_leading_space_line_is_dirt_its_trimmed_copy_is_unknown_and_a_clean_tree_is_clean() {
    let line = " M cli/src/main.rs";
    let trimmed = line.trim();
    assert_ne!(line, trimmed, "the fixture must carry porcelain's leading space");

    let dirty = build_dirt::porcelain_dirt(&format!("{line}\n"));
    let mangled = build_dirt::porcelain_dirt(&format!("{trimmed}\n"));
    let clean = build_dirt::porcelain_dirt("");
    assert_eq!(dirty, Some(true), "an unstaged modification is dirt");
    assert_eq!(mangled, None, "a TRIMMED line ({trimmed:?}) must read UNKNOWN, never clean");
    assert_eq!(clean, Some(false), "empty porcelain is a clean tree");

    // POSITIVE SENTINEL: three inputs, three DIFFERENT answers, and three
    // different stamp values — `unknown` never renders as `0`.
    let answers: BTreeSet<Option<bool>> = [dirty, mangled, clean].into_iter().collect();
    assert_eq!(answers.len(), 3, "collapsed: dirty={dirty:?} trimmed={mangled:?} clean={clean:?}");
    let rendered: BTreeSet<&str> = [dirty, mangled, clean].into_iter().map(build_dirt::dirty_value).collect();
    assert_eq!(rendered, ["0", "1", "unknown"].into_iter().collect(), "stamp values");
}

fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("cauldron-dirt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn git_in(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=fixture", "-c", "user.email=fixture@invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .expect("spawn git");
    assert!(st.success(), "git {args:?} failed in {}", dir.display());
}

#[test]
fn a_real_repo_scoped_to_the_binary_reads_dirty_clean_and_unknown_honestly() {
    // A repository shaped like this one: a workspace manifest and lock, the
    // binary's crate with ONE path dependency, a crate it does NOT depend on,
    // and a file that is no crate's input at all.
    let repo = scratch("repo");
    write(&repo, "Cargo.toml", "[workspace]\nmembers = [\"cli\", \"libs/env\", \"apps/tui\"]\n");
    write(&repo, "Cargo.lock", "# lock\n");
    write(
        &repo,
        "cli/Cargo.toml",
        "[package]\nname = \"cauldron\"\n\n[[bin]]\nname = \"cauldron\"\npath = \"src/main.rs\"\n\n[dependencies]\nenv = { path = \"../libs/env\" }\n",
    );
    write(&repo, "cli/src/main.rs", "fn main() {}\n");
    write(&repo, "libs/env/Cargo.toml", "[package]\nname = \"env\"\n");
    write(&repo, "libs/env/src/lib.rs", "\n");
    write(&repo, "apps/tui/Cargo.toml", "[package]\nname = \"tui\"\n");
    write(&repo, "apps/tui/src/main.rs", "fn main() {}\n");
    write(&repo, "services/echo.toml", "name = \"echo\"\n");
    git_in(&repo, &["init", "-q"]);
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-q", "-m", "fixture"]);

    let (top, scope) = build_dirt::dirt_scope(&repo.join("cli")).expect("scope derivable");
    let want: BTreeSet<String> =
        ["Cargo.lock", "Cargo.toml", "cli", "libs/env"].into_iter().map(String::from).collect();
    assert_eq!(scope, want, "the scope is the binary's path-dependency closure, derived");

    let clean = build_dirt::dirt_at(&top, &scope);

    // OUTSIDE the scope: a service declaration `.toml` and a crate the binary
    // does not link. The old suffix rule called both of these cauldron's dirt.
    write(&repo, "services/echo.toml", "name = \"echo\" # edited\n");
    write(&repo, "apps/tui/src/main.rs", "fn main() { /* edited */ }\n");
    let outside = build_dirt::dirt_at(&top, &scope);
    assert_eq!(clean, Some(false), "a committed, untouched tree is clean");
    assert_eq!(outside, Some(false), "dirt OUTSIDE the binary's inputs is not the binary's dirt");

    // INSIDE, unstaged: ` M cli/src/main.rs`, leading space first.
    write(&repo, "cli/src/main.rs", "fn main() { /* edited */ }\n");
    let raw = build_dirt::scoped_porcelain(&top, &scope).expect("git status ran");
    assert_eq!(raw, " M cli/src/main.rs\n", "the fixture is not the case it claims to test");
    let dirty = build_dirt::dirt_at(&top, &scope);

    // NOT A REPOSITORY: git exits non-zero.
    let nowhere = scratch("not-a-repo");
    let probe = Command::new("git").current_dir(&nowhere).args(["rev-parse", "--git-dir"]).output().unwrap();
    assert!(!probe.status.success(), "{} is inside a repository", nowhere.display());
    let failed = build_dirt::dirt_at(&nowhere, &scope);

    assert_eq!(dirty, Some(true), "an unstaged edit inside the scope is dirt");
    assert_eq!(failed, None, "a git status that could not run is UNKNOWN, never clean");
    assert_eq!(build_dirt::dirty_value(failed), "unknown");
    assert!(build_dirt::dirt_scope(&nowhere).is_none(), "no repository, no scope — unknown");

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&nowhere);
}
