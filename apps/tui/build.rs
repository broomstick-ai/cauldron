//! WHICH SOURCE PRODUCED THIS BINARY — compiled in, printed by
//! `tui --build-stamp` (R7.S4, H179-1: the face reports its own commit from
//! the process, like every service the supervisor launches).
//!
//! NOT A SECOND MECHANISM. This is `services/tts/build.rs` with the variable
//! names changed, and it `include!`s the same `libs/env/src/build_dirt.rs`
//! that `cli/build.rs` and `services/tts/build.rs` include, so the router, the
//! services and the face cannot disagree about what "dirty" means:
//!
//! * The commit may be PINNED by a shipper (`TUI_BUILD_COMMIT_PIN`); only if
//!   nobody pinned it is `git rev-parse HEAD` sniffed, and `provenance=` says
//!   which.
//! * Dirt is SCOPED to what compiles into this binary — the workspace
//!   `Cargo.toml` and `Cargo.lock` plus the closure of this manifest's path
//!   dependencies — and PUBLISHED as `dirt_scope=`.
//! * It is `1`, `0` or `unknown`, never a failed check read as clean.
//!
//! Before this file, Deploy 6 could prove which commit `~/.cauldron/bin/tui`
//! was built from only out of the BUILD (its log, and a shapewire version
//! string `strings` found in the bytes), never out of the process.

use std::process::Command;

include!("../../libs/env/src/build_dirt.rs");

fn main() {
    println!("cargo:rerun-if-env-changed=TUI_BUILD_COMMIT_PIN");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../libs/env/src/build_dirt.rs");
    // HEAD moves on commit and checkout, the index on `add`; `--absolute-git-dir`
    // because in a worktree `.git` is a file and the HEAD that matters is this
    // worktree's own.
    if let Some(gitdir) = git(&["rev-parse", "--absolute-git-dir"]).filter(|g| !g.is_empty()) {
        println!("cargo:rerun-if-changed={gitdir}/HEAD");
        println!("cargo:rerun-if-changed={gitdir}/index");
    }
    let (commit, provenance) = match std::env::var("TUI_BUILD_COMMIT_PIN") {
        Ok(p) if !p.trim().is_empty() => (p.trim().to_string(), "pin"),
        _ => match git(&["rev-parse", "HEAD"]) {
            Some(c) if !c.is_empty() => (c, "git"),
            // Empty, never the word "unknown" in a commit field.
            _ => (String::new(), "unknown"),
        },
    };
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()));
    let scope = dirt_scope(&manifest_dir);
    if let Some((top, dirs)) = &scope {
        // Each scope directory is watched as a directory, so an unstaged edit
        // under any of them re-decides dirt.
        for d in dirs {
            println!("cargo:rerun-if-changed={}", top.join(d).display());
        }
    }
    let verdict = match &scope {
        Some((top, dirs)) if !commit.is_empty() => dirt_at(top, dirs),
        _ => None,
    };
    let published = scope.as_ref().map_or_else(|| "unknown".to_string(), |(_, d)| d.iter().cloned().collect::<Vec<_>>().join(","));
    println!("cargo:rustc-env=TUI_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=TUI_BUILD_PROVENANCE={provenance}");
    println!("cargo:rustc-env=TUI_BUILD_DIRTY={}", dirty_value(verdict));
    println!("cargo:rustc-env=TUI_BUILD_DIRT_SCOPE={published}");
}

/// Trimmed stdout, for one-token answers only, run in this crate's own tree.
fn git(args: &[&str]) -> Option<String> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}
