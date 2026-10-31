//! **THE SUPERVISOR STATES ITS OWN COMMIT, on the same terms it demands.**
//!
//! `cauldron daemon` registers a seam like any producer, and a registered row
//! carries a commit. Without this file that commit would be empty — the one
//! row in the table that could not say what it is, written by the process that
//! refuses to start a service whose build record names no commit.
//!
//! Modelled on `lib/threads/build.rs`, deliberately, including the part that
//! matters: **the shipper may PIN the commit** with
//! `CAULDRON_BUILD_COMMIT_PIN`, and only if nobody did does this sniff
//! `git rev-parse HEAD`. A pin is a fact somebody shipped; a sniff is a guess
//! about the tree that happened to be checked out. The provenance of the
//! answer is recorded so the two are never confused.
//!
//! `rerun-if-env-changed` is what puts the pin's VALUE in cargo's fingerprint,
//! so changing it rebuilds rather than silently reusing a stamp from the last
//! value.
//!
//! **THE DIRT FLAG IS FAIL-CLOSED** (R1.S3b). It used to end in
//! `.unwrap_or(false)`, which read a `git status` that could not run as a
//! CLEAN tree — the inverse of the rule `build` enforces on every service. It
//! is now `1`, `0` or `unknown`, scoped to this binary's inputs and published
//! as `dirt_scope=`; the parser lives in `../libs/env/src/build_dirt.rs`,
//! shared with every service's build script, so the test runs
//! it.

use std::process::Command;

// `dirt_scope`, `scoped_porcelain`, `dirt_at`, `porcelain_dirt`,
// `porcelain_is_wellformed`, `path_deps`, `dirty_value` — shared with
// `tests/self_stamp.rs`.
include!("../libs/env/src/build_dirt.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=CAULDRON_BUILD_COMMIT_PIN");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../libs/env/src/build_dirt.rs");

    // ── MAKE THE STAMP TRACK THE COMMIT, OR IT IS A LIE LIKE ANY OTHER ───
    //
    // Without these, cargo caches this script's output and the stamp keeps
    // naming whatever commit was checked out the first time the crate was
    // built. The live prod holder was installed carrying `efa202c1b276+dirty`
    // five commits after that commit and from a CLEAN tree — a real,
    // resolvable, checkable commit that did not name the bytes, which is
    // exactly the defect class this stamp exists to prevent and the same one
    // that put `version = "290113e"` in a declaration for a day.
    //
    // `--absolute-git-dir` rather than a literal `.git`: in a WORKTREE `.git`
    // is a FILE pointing at `<main>/.git/worktrees/<name>`, and that is where
    // this tree's own HEAD lives. A literal would have watched the main
    // checkout's HEAD from every worktree, which is the same mistake
    // `echo.toml`'s absolute `workspace` made.
    if let Some(gitdir) = git(&["rev-parse", "--absolute-git-dir"]) {
        if !gitdir.is_empty() {
            // HEAD moves on commit and on checkout; the index moves on `add`,
            // which is what flips `dirty` for a staged change.
            println!("cargo:rerun-if-changed={gitdir}/HEAD");
            println!("cargo:rerun-if-changed={gitdir}/index");
        }
    }
    // An UNSTAGED edit moves neither HEAD nor the index, so those two alone
    // left `dirty` as of the last time this script ran. Every directory in the
    // dirt scope is watched below as a DIRECTORY (cargo walks it), so an edit,
    // add or delete under any of them re-runs this script and re-decides dirt.

    let (commit, provenance) = match std::env::var("CAULDRON_BUILD_COMMIT_PIN") {
        Ok(p) if !p.trim().is_empty() => (p.trim().to_string(), "pin"),
        _ => match git(&["rev-parse", "HEAD"]) {
            Some(c) if !c.is_empty() => (c, "git"),
            // EMPTY, not "unknown". An empty commit is what `probe`
            // deliberately never compares in either direction, so silence is
            // correctly not read as disagreement; the literal string
            // "unknown" would end up in the registry's COMMIT column looking
            // like a value.
            _ => (String::new(), "unknown"),
        },
    };

    // ── DIRT: SCOPED, UNTRIMMED, AND UNKNOWN RATHER THAN CLEAN ───────────
    //
    // Scoped to what compiles into THIS binary — `Cargo.toml`, `Cargo.lock`
    // and the closure of `cli/Cargo.toml`'s path dependencies — not the whole
    // worktree filtered by suffix, which marked cauldron dirty for a service
    // declaration's `.toml` or a curator's `.pen.lock`. Without a commit there
    // is nothing for dirt to be relative to, so that is `unknown` too.
    let manifest_dir = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()),
    );
    let scope = dirt_scope(&manifest_dir);
    if let Some((top, dirs)) = &scope {
        for d in dirs {
            println!("cargo:rerun-if-changed={}", top.join(d).display());
        }
    }
    let verdict = match &scope {
        Some((top, dirs)) if !commit.is_empty() => dirt_at(top, dirs),
        _ => None,
    };
    let published = match &scope {
        Some((_, dirs)) => dirs.iter().cloned().collect::<Vec<_>>().join(","),
        // A scope that could not be derived is published as such, so a reader
        // never mistakes "covered nothing" for "covered everything".
        None => "unknown".to_string(),
    };

    println!("cargo:rustc-env=CAULDRON_SELF_COMMIT={commit}");
    println!("cargo:rustc-env=CAULDRON_SELF_PROVENANCE={provenance}");
    println!("cargo:rustc-env=CAULDRON_SELF_DIRTY={}", dirty_value(verdict));
    println!("cargo:rustc-env=CAULDRON_SELF_DIRT_SCOPE={published}");
}

/// Trimmed stdout — for one-token answers (`rev-parse`) ONLY; porcelain goes
/// through `scoped_porcelain`, which never trims.
///
/// `git` in the directory of THIS crate's manifest, which is inside whatever
/// worktree is being built — never a hardcoded path. That is the same defect
/// `echo.toml`'s absolute `workspace` had: a literal names the main checkout
/// from every worktree, so a build in a worktree would stamp a sibling's HEAD.
fn git(args: &[&str]) -> Option<String> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let out = Command::new("git").arg("-C").arg(&dir).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
