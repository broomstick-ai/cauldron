// THE DIRT CHECK — the part of `build.rs` that turns `git status --porcelain`
// into `THREADS_BUILD_DIRTY`, in its own file so the TEST runs the function the
// BUILD SCRIPT runs.
//
// `build.rs` `include!`s this file (which is why it opens with `//` and not
// `//!`: an inner doc comment cannot be included mid-file), and
// `tests/build_provenance.rs` mounts it with `#[path]`. It is deliberately NOT
// a module of the library: nothing at runtime needs it. The model is `lib/tts`'s
// `src/stamp_scope.rs` (substrate `2303409`).
//
// THE RULE, fail-closed:
//   * a dirt check that cannot run says UNKNOWN, never `0`;
//   * a porcelain line is never trimmed — ` M path` BEGINS with a space;
//   * a line that is not well formed makes the whole answer UNKNOWN.
//
// Why the regression test has to reach in here: the leading-space fix first
// shipped with no committed red. An inspector put `s.trim().lines()` back into
// the old `git_lines` and every test stayed green, because nothing ever fed the
// real parser a ` M` line. `tests/build_provenance.rs` now does, and demands
// three DIFFERENT answers from a dirty line, a trimmed copy of it, and a clean
// tree.

/// Run `git status --porcelain --untracked-files=all` in `dir` and return its
/// stdout VERBATIM: not trimmed, not split, not filtered. `None` when git could
/// not answer (missing, not a repository, non-zero exit, non-UTF-8); `Some("")`
/// when it answered "clean". The two are never folded together.
///
/// Nothing on this path may trim, for this reason:
///
/// `git status --porcelain` puts the index status in column 1, the worktree
/// status in column 2 and a space in column 3, so a file modified but not staged
/// is `" M path"` — **with a leading space**. A whole-output trim (what
/// `build.rs`'s `git_ok` used to do) eats that space on the first line and shifts its path one character
/// left.
///
/// This was found by the positive sentinel in the fix's own verification, not by
/// reasoning: the old extension test could not see it, because it only ever
/// looked at a path's SUFFIX, and `ib/threads/src/image.rs` ends in `.rs` just
/// as happily as `lib/threads/src/image.rs` does. A scope that matches on a path
/// PREFIX is not immune, and read every first-line path as one character short.
/// Untracked entries (`?? path`) have no leading space, so they worked — which
/// is precisely the shape of bug that passes a test suite built only of the
/// cases that happen to work.
#[allow(dead_code)]
pub fn git_status_porcelain(dir: &std::path::Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// `Some(true)`/`Some(false)` — the repository at `dir` does/does not hold
/// uncommitted files that compile into this product. `None` means git could not
/// answer, which is NOT the same as clean and must never be reported as it:
/// this is the one place a failed `git status` could be read as clean (an
/// `unwrap_or_default()` on the porcelain would do it), and the test's
/// not-a-repository case exists to catch exactly that.
///
/// `--untracked-files=all` is load-bearing and is `promote`'s reasoning, kept:
/// without it git collapses an untracked directory into one entry
/// (`?? lib/threads/src/newmod/`) and never says what is inside, so an entire
/// untracked source directory matches no file rule and passes as clean.
///
/// # WHAT THIS STILL CANNOT SEE: an IGNORED file that changes the compile
///
/// Every answer here comes from `git status`, and `git status` does not report
/// an ignored file at ANY `--untracked-files` setting. This repository ignores
/// `/.cargo/config.toml` (`.gitignore` line 24) — a file that can set
/// `rustflags`, a linker or a target directory, and therefore change the bytes
/// this build produces. Constructed and measured: creating it leaves
/// `git status --porcelain --untracked-files=all` completely empty and the stamp
/// still reads `dirty=0`.
///
/// So the honest statement of what `dirty=0` means is **"no file git will tell
/// me about, among those that compile into this product, differs from that
/// commit"** — not "these bytes are that commit's bytes". `promote` has the same
/// hole for the same reason, and `.cargo/` is matched wide by BOTH of us
/// precisely because a missed rustflag is the expensive case; the ignore rule
/// defeats that intent on both sides at once.
///
/// It is left as a documented limit rather than closed here, because closing it
/// means hashing content instead of asking git, which is a different mechanism
/// and would make this check disagree with the gate in front of it. The two
/// checks disagreeing silently is the class of defect this whole file is a
/// repair of.
#[allow(dead_code)]
pub fn dirt_at(dir: &std::path::Path, scope: &[String]) -> Option<bool> {
    porcelain_dirt(&git_status_porcelain(dir)?, scope)
}

/// The verdict over one `git status --porcelain` stdout, exactly as git printed
/// it. `Some(true)` dirty, `Some(false)` clean (including the empty output of a
/// clean tree), `None` when ANY line is not well formed.
///
/// A malformed line is not skipped and above all not read as clean: the bug it
/// guards is a mangled status column turning an unstaged modification into a
/// path in no scope and a stamp of `dirty=0` over a dirty tree. Unknown
/// propagates to `dirty=unknown`, which `promote` and cauldron refuse — so a
/// parse bug fails as a loud refusal, not a flattering answer.
#[allow(dead_code)]
pub fn porcelain_dirt(stdout: &str, scope: &[String]) -> Option<bool> {
    let mut any = false;
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        if !porcelain_is_wellformed(line) {
            return None;
        }
        if porcelain_paths(line).iter().any(|p| classify(p, scope)) {
            any = true;
        }
    }
    Some(any)
}

/// Is `line` a porcelain v1 entry, `XY <path>`?
///
/// Column 3 (`b[2]`) is ALWAYS a space and at least one path byte follows it,
/// so the line is at least four bytes. `X` and `Y` are status letters or a
/// space, never both spaces. A trimmed ` M lib/x.rs` becomes `M lib/x.rs`,
/// whose third byte is `l`: rejected, so the answer becomes unknown rather
/// than a path one character short that matches no scope.
#[allow(dead_code)]
pub fn porcelain_is_wellformed(line: &str) -> bool {
    let b = line.as_bytes();
    if b.len() < 4 || b[2] != b' ' {
        return false;
    }
    let ok = |c: u8| matches!(c, b' ' | b'M' | b'A' | b'D' | b'R' | b'C' | b'U' | b'T' | b'?' | b'!');
    ok(b[0]) && ok(b[1]) && !(b[0] == b' ' && b[1] == b' ')
}

/// `THREADS_BUILD_DIRTY`'s value for a verdict: `"1"`, `"0"`, or EMPTY for
/// unknown. Empty is what `image.rs` (`BUILD_DIRTY`) and `promote` already read
/// as unknown, and `image::build_stamp_line` renders it `dirty=unknown`; it is
/// never `"0"`.
#[allow(dead_code)]
pub fn dirty_value(verdict: Option<bool>) -> &'static str {
    match verdict {
        Some(true) => "1",
        Some(false) => "0",
        None => "",
    }
}

/// Every path one `git status --porcelain` entry names.
///
/// An entry is `XY <path>`, except a rename, which is `XY <old> -> <new>`. BOTH
/// sides of a rename are yielded: moving a file INTO a package's `src/` changes
/// the product exactly as much as editing one already there. Git quotes a path
/// containing unusual characters; the quotes are stripped, because an
/// unstripped trailing `"` defeats a `.rs` suffix test, and that is a MISS.
#[allow(dead_code)]
pub fn porcelain_paths(line: &str) -> Vec<String> {
    let p = line.get(3..).unwrap_or("");
    let sides: Vec<&str> = match p.split_once(" -> ") {
        Some((old, new)) => vec![old, new],
        None => vec![p],
    };
    sides
        .into_iter()
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Is `path` (repo-root-relative) a file whose contents can reach this product?
///
/// `scope` is `build.rs`'s `product_packages`. The three arms, in order:
///
/// * **global inputs** — `Cargo.toml`, `Cargo.lock` and `rust-toolchain*` at the
///   repo root, and anything under `.cargo/`. These change every package's
///   build, whichever one is being built. `.cargo/` is matched wide on purpose:
///   it is a tiny category and a miss there ships an unintended rustflag.
/// * **per-package inputs** — for each package `P` in scope: `P/Cargo.toml`,
///   `P/build.rs`, a root-level `P/*.sql`, or a `.rs`/`.sql` anywhere under
///   `P/src/`.
/// * everything else is not a compiled input.
#[allow(dead_code)]
pub fn classify(path: &str, scope: &[String]) -> bool {
    if matches!(path, "Cargo.toml" | "Cargo.lock") || path.starts_with("rust-toolchain") {
        return true;
    }
    if path == ".cargo" || path.starts_with(".cargo/") {
        return true;
    }

    for pkg in scope {
        let Some(tail) = path.strip_prefix(pkg.as_str()).and_then(|t| t.strip_prefix('/')) else {
            continue;
        };
        if tail == "Cargo.toml" || tail == "build.rs" {
            return true;
        }
        // A root-level asset of the package, e.g. `schema.sql`.
        if !tail.contains('/') && tail.ends_with(".sql") {
            return true;
        }
        if let Some(inner) = tail.strip_prefix("src/") {
            if inner.ends_with(".rs") || inner.ends_with(".sql") {
                return true;
            }
        }
    }
    false
}
