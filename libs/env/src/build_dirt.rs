// THE DIRT CHECK EVERY BUILD SCRIPT IN THIS WORKSPACE SHARES — the part that
// turns `git status --porcelain` into a `*_BUILD_DIRTY` value, in its own file
// so the TEST runs the function the BUILD SCRIPT runs.
//
// It opens with `//` and not `//!` because an inner doc comment cannot be
// included mid-file, and every consumer `include!`s it: `cli/build.rs`,
// `services/tts/build.rs`, and every service migrated after tts.
// `cli/tests/self_stamp.rs` mounts it with `#[path]`.
//
// ── WHY IT LIVES IN `libs/env/src/` AND IS NOT A MODULE OF `env` (R3.S2.0) ──
//
// It used to live at `cli/src/build_dirt.rs` and be reached from
// `services/tts/build.rs` as `../../cli/src/build_dirt.rs` — a service's build
// script reaching into the CLI's source tree for a file the CLI does not
// compile either. R3.S2.0 gave it a home, and the choice is load-bearing
// rather than cosmetic:
//
//   * It is NOT A CRATE, and `libs/build-dirt` was rejected. `dirt_scope`
//     below derives a binary's dirt scope as the TRANSITIVE CLOSURE OF PATH
//     DEPENDENCIES from the manifests. A plain directory under `libs/` is not a
//     path dependency of anything, so the file that decides whether cauldron is
//     dirty would have fallen OUTSIDE the scope it computes — an edit to this
//     very file would have left the stamp reading `dirty=0`. Making it a crate
//     instead would have meant a `[build-dependencies]` entry that nothing
//     LINKS, declared only to buy back the scope membership.
//   * `libs/env` is already a path dependency of `cli` and of every service
//     (it is what makes a process a cauldron service at all), so sitting in
//     its `src/` puts this file inside the dirt scope of every binary that
//     includes it, with nothing hardcoded and the closure rule intact.
//   * It is deliberately NOT declared in `libs/env/src/lib.rs`, so cargo never
//     compiles it into the `env` library. Nothing at RUNTIME needs it; the
//     check runs at BUILD time. Were it a module, these seven `pub fn` would
//     join `env`'s public surface and its warnings attestation for no caller.
//
// Models: substrate `lib/tts/src/stamp_scope.rs` (2303409), substrate
// `lib/threads/src/build_dirt.rs` (R1.S3b) and harness `apps/aui/build.rs`
// (7a67418).
//
// THE RULE, fail-closed — the one cauldron enforces on every service it builds
// and, until this file, did not keep itself:
//   * a dirt check that cannot run says `unknown`, never `0` (the old build.rs
//     ended in `.unwrap_or(false)`, which read a FAILED `git status` as CLEAN);
//   * a porcelain line is never trimmed — ` M path` BEGINS with a space;
//   * a line that is not well formed makes the whole answer `unknown`;
//   * the dirt is SCOPED to this binary's inputs (the workspace `Cargo.toml`
//     and `Cargo.lock` plus the closure of `cli/Cargo.toml`'s path
//     dependencies), not the whole worktree filtered by a `.rs/.toml/.lock`
//     SUFFIX — a suffix rule called a curator's `.pen.lock` or any service
//     declaration `.toml` "dirt in cauldron".

/// Repo-relative pathspecs for every input of the binary whose manifest lives
/// in `manifest_dir`, and the repository top level they are relative to.
///
/// `Cargo.toml` and `Cargo.lock` at the top, then the TRANSITIVE closure of
/// local path dependencies, derived from the manifests — never hardcoded, so a
/// path dependency added to `cli/Cargo.toml` joins the scope by itself.
/// `None` when the scope cannot be fully established (git cannot name a top
/// level, a manifest cannot be read, a path dependency does not exist or lies
/// outside this repository, whose dirt this repository's `git status` cannot
/// see). An incomplete scope is UNKNOWN dirt, never a narrower clean.
#[allow(dead_code)]
pub fn dirt_scope(
    manifest_dir: &std::path::Path,
) -> Option<(std::path::PathBuf, std::collections::BTreeSet<String>)> {
    let top = git_one(manifest_dir, &["rev-parse", "--show-toplevel"])?;
    let top = canon(std::path::Path::new(&top));
    let mut scope = std::collections::BTreeSet::new();
    for f in ["Cargo.toml", "Cargo.lock"] {
        if !top.join(f).is_file() {
            return None;
        }
        scope.insert(f.to_string());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut todo = vec![canon(manifest_dir)];
    while let Some(dir) = todo.pop() {
        if !seen.insert(dir.clone()) {
            continue;
        }
        let rel = dir.strip_prefix(&top).ok()?;
        scope.insert(rel.to_string_lossy().replace('\\', "/"));
        let text = std::fs::read_to_string(dir.join("Cargo.toml")).ok()?;
        for dep in path_deps(&text) {
            let p = dir.join(dep);
            if !p.join("Cargo.toml").is_file() {
                return None;
            }
            todo.push(canon(&p));
        }
    }
    Some((top, scope))
}

/// `path = "..."` values inside `[*dependencies]` tables (inline-table form,
/// the only form this workspace uses). `[[bin]] path = ...` is not a
/// dependency and is skipped by section. `dev-dependencies` are included: they
/// over-report (a test helper's edit marks the binary dirty), which costs a
/// commit, never a wrong identity.
#[allow(dead_code)]
pub fn path_deps(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_deps = false;
    for line in manifest.lines() {
        let l = line.split('#').next().unwrap_or("").trim();
        if l.starts_with('[') {
            in_deps = l.trim_matches(|c| c == '[' || c == ']').ends_with("dependencies");
            continue;
        }
        if !in_deps {
            continue;
        }
        if let Some(i) = l.find("path") {
            let rest = l[i + 4..].trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                if let Some(rest) = rest.trim_start().strip_prefix('"') {
                    if let Some(end) = rest.find('"') {
                        out.push(rest[..end].to_string());
                    }
                }
            }
        }
    }
    out
}

/// `git status --porcelain --untracked-files=normal -- <scope>` at `top`,
/// stdout VERBATIM — not trimmed, not split, not filtered. `None` when git
/// could not answer; `Some("")` when it answered "clean". The two are never
/// folded together, and nothing on this path may trim: an unstaged
/// modification is ` M path`, and a whole-output trim eats that space on the
/// first line and shifts every column (tts 2303409).
///
/// `--untracked-files=normal` is enough here, unlike threads' `=all`: the check
/// is "did git print ANY line inside the scope", not a per-file suffix test, so
/// a collapsed `?? cli/src/newdir/` is still a line and still dirt.
#[allow(dead_code)]
pub fn scoped_porcelain(
    top: &std::path::Path,
    scope: &std::collections::BTreeSet<String>,
) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(top)
        .args(["status", "--porcelain", "--untracked-files=normal", "--"])
        .args(scope.iter())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// The dirt verdict for the repository at `top` over `scope`: `Some(true)`
/// dirty, `Some(false)` clean, `None` UNKNOWN — git could not run, or printed a
/// line this cannot parse. `None` is never read as clean.
#[allow(dead_code)]
pub fn dirt_at(
    top: &std::path::Path,
    scope: &std::collections::BTreeSet<String>,
) -> Option<bool> {
    porcelain_dirt(&scoped_porcelain(top, scope)?)
}

/// The verdict over one scoped `git status --porcelain` stdout, exactly as git
/// printed it. Any well-formed line is dirt (the pathspec already scoped it);
/// the empty output of a clean tree is clean; ANY malformed line is unknown —
/// not skipped, and above all not clean.
#[allow(dead_code)]
pub fn porcelain_dirt(stdout: &str) -> Option<bool> {
    let mut any = false;
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        if !porcelain_is_wellformed(line) {
            return None;
        }
        any = true;
    }
    Some(any)
}

/// Is `line` a porcelain v1 entry, `XY <path>`? Column 3 (`b[2]`) is always a
/// space and at least one path byte follows, so the line is at least four
/// bytes; `X` and `Y` are status letters or a space, never both spaces. A
/// trimmed ` M cli/x.rs` becomes `M cli/x.rs`, whose third byte is `c`:
/// rejected, so the answer is unknown.
#[allow(dead_code)]
pub fn porcelain_is_wellformed(line: &str) -> bool {
    let b = line.as_bytes();
    if b.len() < 4 || b[2] != b' ' {
        return false;
    }
    let ok = |c: u8| matches!(c, b' ' | b'M' | b'A' | b'D' | b'R' | b'C' | b'U' | b'T' | b'?' | b'!');
    ok(b[0]) && ok(b[1]) && !(b[0] == b' ' && b[1] == b' ')
}

/// `CAULDRON_SELF_DIRTY`'s value for a verdict: `1`, `0` or `unknown`.
/// `unknown` is the word `Stamp` (`cli/src/main.rs` `read_stamp`) already
/// REFUSES, so a cauldron that cannot establish its own dirt cannot be
/// installed as a stamped version of itself either.
#[allow(dead_code)]
pub fn dirty_value(verdict: Option<bool>) -> &'static str {
    match verdict {
        Some(true) => "1",
        Some(false) => "0",
        None => "unknown",
    }
}

#[allow(dead_code)]
fn canon(p: &std::path::Path) -> std::path::PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// `git -C <dir> ...`, trimmed stdout on success — for ONE-TOKEN answers (a
/// sha, a path) only, never for porcelain.
#[allow(dead_code)]
fn git_one(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
