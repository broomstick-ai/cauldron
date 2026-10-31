//! **Does the build stamp's dirt check look at the right files?**
//!
//! `lib/threads/build.rs` decides `THREADS_BUILD_DIRTY`, and that flag is the
//! version identity: `image::build_commit()` returns `None` unless it is exactly
//! `"0"`, and the registry and the rollback path both read that. A dirt check is
//! worth exactly as much as the set of files it looked at, so this file tests
//! the SET, not the flag.
//!
//! # Why this test exists in this shape
//!
//! `build.rs` cannot parse TOML. A build script with no dependencies has no
//! parser, and putting a third-party one in front of every build of this daemon
//! to read one field would be a bad trade — so it finds its local path
//! dependencies with a deliberately loose LINE SCAN over each `Cargo.toml`.
//!
//! A line scan has two failure directions and they are not symmetric:
//!
//! * **over-matching is harmless.** A `path = "…"` that is not a package
//!   resolves to a directory with no manifest and `build.rs` drops it. Worst
//!   case the dirt scope is too wide and a build is called dirty when it was
//!   clean, which costs a commit.
//! * **under-matching is the defect.** A path dependency declared in a form the
//!   scan does not recognise falls out of the scope ENTIRELY — and then that
//!   crate's uncommitted source compiles into this binary while the stamp says
//!   `dirty=0` and `build_commit()` confidently names a commit that does not
//!   describe these bytes. Nothing downstream can detect it: `promote`
//!   classifies another package's files as "another package" and reports them
//!   without refusing, so a dirty `shapewire` inside a signed `threads` is
//!   invisible on both sides.
//!
//! So the assertion that matters is CONTAINMENT in one direction: every package
//! whose bytes reach this product must appear in the published scope. This test
//! recomputes that closure with a real parser, which is the independence the
//! check rests on — if it reused the line scan it would only prove the scan
//! agrees with itself.
//!
//! # The sentinel
//!
//! A containment assertion over an empty expected set passes no matter what the
//! scope says, and a guard that cannot fail is worse than no guard because it
//! reports safety. So the closure is asserted to be non-empty and to contain
//! both packages that are known to be in it by inspection — `lib/threads`
//! itself and `lib/shapewire`, which `threads` links in. If a refactor ever
//! makes the expected set empty, these fail loudly instead of passing
//! vacuously.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

// The dirt check's parser — the SAME file `build.rs` `include!`s, so these tests
// run the function the build script runs, not a copy of it.
#[path = "../../src/build_dirt.rs"]
mod build_dirt;

/// The repo root, found by walking up from this crate until a directory holds
/// both a `Cargo.toml` and a `.git` entry.
///
/// Deliberately not `git rev-parse`: this test must not depend on a working
/// `git`, and in a linked worktree `.git` is a FILE rather than a directory,
/// which is the exact distinction that made `build.rs` wrong for twelve days.
/// `exists()` covers both shapes.
fn repo_root() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        if d.join("Cargo.toml").is_file() && d.join(".git").exists() {
            return d;
        }
        assert!(d.pop(), "walked past the filesystem root without finding a repo root");
    }
}

/// Every local path dependency of the manifest at `dir`, as directories,
/// parsed with a real TOML parser.
///
/// `dev-dependencies` are deliberately EXCLUDED: a test-only crate's bytes do
/// not reach the product binary, so it is not a compiled input of the product.
/// `build-dependencies` ARE included — they run during the build and can change
/// what it emits.
fn path_deps(dir: &Path) -> Vec<PathBuf> {
    let text = std::fs::read_to_string(dir.join("Cargo.toml"))
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.join("Cargo.toml").display()));
    let doc: toml::Value = text.parse().expect("manifest is not valid TOML");

    let mut out = Vec::new();
    for table in ["dependencies", "build-dependencies"] {
        let Some(deps) = doc.get(table).and_then(|v| v.as_table()) else { continue };
        for (_name, spec) in deps {
            if let Some(p) = spec.get("path").and_then(|v| v.as_str()) {
                out.push(dir.join(p));
            }
        }
    }
    out
}

/// The transitive closure of local path dependencies, repo-root-relative and
/// `/`-separated — the same shape `build.rs` publishes.
fn product_closure(root: &Path) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut queue = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))];
    while let Some(dir) = queue.pop() {
        let dir = std::fs::canonicalize(&dir).expect("a declared path dependency must exist");
        let rel = dir
            .strip_prefix(root)
            .expect("a path dependency outside the repo would make the dirt check unanswerable")
            .to_string_lossy()
            .replace('\\', "/");
        if !seen.insert(rel) {
            continue;
        }
        queue.extend(path_deps(&dir));
    }
    seen
}

#[test]
fn the_dirt_scope_covers_every_package_whose_bytes_reach_this_binary() {
    let root = std::fs::canonicalize(repo_root()).unwrap();
    let expected = product_closure(&root);

    // ── THE SENTINEL ──────────────────────────────────────────────────────
    // Containment over an empty set is vacuous, so prove the closure saw real
    // packages before using it to judge anything.
    assert!(
        !expected.is_empty(),
        "the product closure came back EMPTY, so every assertion below would \
         pass without looking at anything — the manifest walk is broken, not the \
         build script"
    );
    for known in ["services/threads", "libs/shapewire"] {
        assert!(
            expected.contains(known),
            "the product closure does not contain {known}, which is in it by \
             inspection ({} declares `shapewire = {{ path = \"../../libs/shapewire\" }}`). \
             This test's own walk is broken; fix it before trusting its verdict. \
             Found: {expected:?}",
            "services/threads/Cargo.toml"
        );
    }

    let published: BTreeSet<String> = threads::image::BUILD_DIRT_SCOPE
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    assert!(
        !published.is_empty(),
        "build.rs published an EMPTY compiled-input scope (THREADS_BUILD_DIRT_SCOPE), \
         which means it could not establish one. That is reported honestly — \
         THREADS_BUILD_DIRTY is empty too and build_commit() is None — but this \
         image has no version identity, so it must not be promoted"
    );

    // THE LOAD-BEARING DIRECTION. A package missing here is a package whose
    // uncommitted source can compile into this binary with the stamp still
    // saying dirty=0.
    let missed: Vec<&String> = expected.difference(&published).collect();
    assert!(
        missed.is_empty(),
        "build.rs's dirt scope MISSES {missed:?} — their bytes compile into this \
         binary, so uncommitted changes there would ship while the stamp reports \
         dirty=0 and build_commit() names a commit that does not describe them. \
         `path_dependencies`' line scan did not recognise how the dependency is \
         declared.\n  published: {published:?}\n  expected:  {expected:?}"
    );

    // The other direction is allowed but should not be silent: a wider scope
    // only ever over-reports dirt, which costs a commit, not a wrong identity.
    let extra: Vec<&String> = published.difference(&expected).collect();
    if !extra.is_empty() {
        eprintln!(
            "note: build.rs's dirt scope is WIDER than the product closure: {extra:?}. \
             Harmless (it can only over-report dirt), but if this is unexpected, \
             `path_dependencies` is matching something that is not a dependency."
        );
    }
}

/// Every path in the published scope must be a real local package, or the scope
/// is describing files that do not exist and the dirt check is looking nowhere.
#[test]
fn every_published_scope_entry_is_a_real_package_directory() {
    let root = std::fs::canonicalize(repo_root()).unwrap();

    let published: Vec<&str> = threads::image::BUILD_DIRT_SCOPE
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    assert!(
        !published.is_empty(),
        "empty scope — see the companion test for what that means"
    );

    for entry in published {
        let manifest = root.join(entry).join("Cargo.toml");
        assert!(
            manifest.is_file(),
            "the dirt scope names {entry}, but {} is not a file, so no package \
             lives there and every path the check matched against it is dead",
            manifest.display()
        );
    }
}

// ══════════════════════════════════════════════════════════════════════════
//  THE DIRT VERDICT — fail-closed, untrimmed, refuses what it cannot parse
// ══════════════════════════════════════════════════════════════════════════
//
// The leading-space fix first landed with NO committed red: an inspector put
// `s.trim().lines()` back into the parser and every test here stayed green,
// because no test ever fed the real parser an unstaged modification. These
// do — as a fixture string, and through a real `git status` in a real
// repository — and they demand that a dirty line, the trimmed copy of that
// same line, and a clean tree give THREE DIFFERENT answers.

const FIXTURE_PATH: &str = "services/threads/src/image.rs";

fn threads_scope() -> Vec<String> {
    vec!["services/threads".to_string()]
}

#[test]
fn a_leading_space_line_is_dirt_its_trimmed_copy_is_unknown_and_a_clean_tree_is_clean() {
    let scope = threads_scope();
    let dirty_line = format!(" M {FIXTURE_PATH}");
    assert!(dirty_line.starts_with(' '), "the fixture must carry porcelain's leading space");
    let trimmed = dirty_line.trim().to_string();
    assert_ne!(trimmed, dirty_line, "the trimmed copy must differ from the line, or it tests nothing");

    let dirty = build_dirt::porcelain_dirt(&format!("{dirty_line}\n"), &scope);
    let mangled = build_dirt::porcelain_dirt(&format!("{trimmed}\n"), &scope);
    let clean = build_dirt::porcelain_dirt("", &scope);

    assert_eq!(dirty, Some(true), "an unstaged modification of {FIXTURE_PATH} is not dirt");
    assert_eq!(
        mangled, None,
        "a TRIMMED porcelain line ({trimmed:?}) must read UNKNOWN — never clean, never dirty"
    );
    assert_eq!(clean, Some(false), "a clean tree (empty porcelain) must read clean");

    // A well-formed line OUTSIDE the scope is clean, not unknown: the check is
    // scoped, and refusing is for lines it cannot parse, not lines it ignores.
    assert_eq!(build_dirt::porcelain_dirt(" M README.md\n", &scope), Some(false));

    // ── THE POSITIVE SENTINEL ─────────────────────────────────────────────
    // Three inputs, three DIFFERENT answers. A parser that collapses any two —
    // unknown read as clean, a trimmed line read as dirty-or-clean — fails here
    // even if every equality above were weakened.
    let answers: BTreeSet<Option<bool>> = [dirty, mangled, clean].into_iter().collect();
    assert_eq!(answers.len(), 3, "the three cases collapsed: dirty={dirty:?} trimmed={mangled:?} clean={clean:?}");

    let rendered: BTreeSet<&str> = [dirty, mangled, clean]
        .into_iter()
        .map(build_dirt::dirty_value)
        .collect();
    assert_eq!(rendered.len(), 3, "the stamp values collapsed: {rendered:?}");
    assert_ne!(build_dirt::dirty_value(None), "0", "unknown must never be stamped as clean");
}

/// A scratch directory under cargo's per-target temp root, fresh per call.
fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("build-dirt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
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
fn a_real_git_status_reports_an_unstaged_edit_as_dirt_and_a_failed_git_as_unknown() {
    let scope = threads_scope();

    // ── A real repository, one committed file inside the scope. ────────────
    let repo = scratch("repo");
    git_in(&repo, &["init", "-q"]);
    let file = repo.join(FIXTURE_PATH);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "pub fn a() {}\n").unwrap();
    git_in(&repo, &["add", FIXTURE_PATH]);
    git_in(&repo, &["commit", "-q", "-m", "fixture"]);

    let clean = build_dirt::dirt_at(&repo, &scope);

    // Modified, NOT staged: git prints ` M lib/threads/src/image.rs`, leading
    // space first on the first (only) line — the exact shape a whole-output
    // trim mangles.
    std::fs::write(&file, "pub fn a() { /* edited */ }\n").unwrap();
    let raw = build_dirt::git_status_porcelain(&repo).expect("git status ran");
    assert_eq!(raw, format!(" M {FIXTURE_PATH}\n"), "the fixture is not the case it claims to test");
    let dirty = build_dirt::dirt_at(&repo, &scope);

    // ── Not a repository: git exits non-zero. ─────────────────────────────
    let nowhere = scratch("not-a-repo");
    let probe = Command::new("git")
        .current_dir(&nowhere)
        .args(["rev-parse", "--git-dir"])
        .output()
        .expect("spawn git");
    assert!(
        !probe.status.success(),
        "{} is inside a git repository, so it cannot stand for a failed git status",
        nowhere.display()
    );
    let failed = build_dirt::dirt_at(&nowhere, &scope);

    assert_eq!(clean, Some(false), "a committed, untouched tree must read clean");
    assert_eq!(dirty, Some(true), "a real unstaged edit inside the scope must read dirty");
    assert_eq!(failed, None, "a git status that could not run must read UNKNOWN, never clean");
    assert_eq!(build_dirt::dirty_value(failed), "", "unknown must stamp as empty (dirty=unknown), never 0");

    let answers: BTreeSet<Option<bool>> = [dirty, failed, clean].into_iter().collect();
    assert_eq!(answers.len(), 3, "dirty={dirty:?} failed={failed:?} clean={clean:?} collapsed");

    let _ = std::fs::remove_dir_all(&repo);
    let _ = std::fs::remove_dir_all(&nowhere);
}

/// The stamp's `dirty=` against a `git status` run NOW — never against the
/// compiled-in `THREADS_BUILD_DIRTY`, because two reads of one value are not a
/// witness.
///
/// The witness does not parse porcelain at all: the pathspecs below ARE the
/// compiled-input rule (`classify`) written as git pathspecs, over the PUBLISHED
/// scope, so "git printed anything" is the whole verdict. A parser defect in
/// `build_dirt.rs` therefore cannot hide on both sides at once.
#[test]
fn the_stamp_dirt_agrees_with_a_git_status_run_now_over_the_published_scope() {
    let root = std::fs::canonicalize(repo_root()).unwrap();
    let line = threads::image::build_stamp_line();
    let dirty = line
        .split(' ')
        .find_map(|kv| kv.strip_prefix("dirty="))
        .unwrap_or_else(|| panic!("no dirty= field: {line}"));
    assert!(
        matches!(dirty, "0" | "1"),
        "git works in this tree at test time, so the build's dirt must be known: {line}"
    );

    let mut specs: Vec<String> = vec![
        "Cargo.toml".into(),
        "Cargo.lock".into(),
        ":(glob)rust-toolchain*".into(),
        ".cargo".into(),
    ];
    let published: Vec<&str> = threads::image::BUILD_DIRT_SCOPE
        .split(',')
        .filter(|s| !s.is_empty())
        .collect();
    assert!(published.contains(&"services/threads"), "scope sentinel: {published:?}");
    for p in &published {
        specs.push(format!("{p}/Cargo.toml"));
        specs.push(format!("{p}/build.rs"));
        specs.push(format!(":(glob){p}/*.sql"));
        specs.push(format!(":(glob){p}/src/**/*.rs"));
        specs.push(format!(":(glob){p}/src/**/*.sql"));
    }
    let out = Command::new("git")
        .current_dir(&root)
        .args(["status", "--porcelain", "--untracked-files=all", "--"])
        .args(&specs)
        .output()
        .expect("spawn git");
    assert!(out.status.success(), "git status failed: {}", String::from_utf8_lossy(&out.stderr));
    let witness = if out.stdout.is_empty() { "0" } else { "1" };
    assert_eq!(
        dirty,
        witness,
        "the stamp says dirty={dirty} but git status NOW over the scope says {witness}:\n{}\nstamp: {line}",
        String::from_utf8_lossy(&out.stdout)
    );
}
