//! Compile the answer to "what commit is this image built from" INTO the image.
//!
//! `daemon.image` reports the running image's build time and commit. Neither is
//! recoverable after the fact — the running process may be executing a file
//! that no longer exists at any path, so there is nothing left to inspect. The
//! only place the answer can live is inside the binary, and the only moment it
//! can be put there is now.
//!
//! **Every failure here is silent and degrades to empty**, never to a guess and
//! never to a build error. An empty value makes `daemon.image` report the field
//! as undeterminable with a reason, which is the honest outcome; a build that
//! refused to compile because `git` was missing would be a far worse trade for
//! a diagnostic field.
//!
//! # TWO SOURCES FOR THE COMMIT, AND ONLY ONE OF THEM IS A PROMISE
//!
//! This file used to have exactly one source — ask `git` at build time — and it
//! was **wrong for twelve days and could not become right**. The triggers that
//! were supposed to re-run it were `<git-dir>/HEAD` and `<git-dir>/refs`, and in
//! a linked worktree neither one works:
//!
//! * `<git-dir>/HEAD` is a **static symref** (`ref: refs/heads/<branch>`). It is
//!   written once when the worktree is created and never again while you stay on
//!   that branch, so its mtime is frozen at the worktree's birth.
//! * `<git-dir>/refs` **does not exist at all** in a linked worktree — refs are
//!   a *shared* path and live in the common dir. The old code guarded each
//!   trigger with `.exists()`, so this one was never even emitted, silently.
//! * The file that actually moves is `<common-dir>/refs/heads/<branch>`, and
//!   nothing watched it.
//!
//! The cost was not hypothetical. An agent read the stale field and reported the
//! running daemon as "built from 99e998bc, 133 commits behind"; a forensic pass
//! had to disprove it from strings in the image. The real answer was 25 commits.
//!
//! So there are now two sources, in a deliberate hierarchy:
//!
//! 1. **`THREADS_BUILD_COMMIT_PIN` — the pin, and the only authority.** Whoever
//!    is *deciding to ship a commit* passes it in. Declared to cargo with
//!    `rerun-if-env-changed`, which puts the **value** in cargo's fingerprint:
//!    the rebuild condition becomes literally "the commit changed", with no
//!    filesystem oracle in between, and a build for a new pin **cannot** reuse
//!    the old stamp. `promote` sets it and then reads the stamp back off the
//!    product it signed; see `--build-stamp` in `main.rs`.
//! 2. **The git sniff — a fallback for a plain `cargo build`, labelled as one.**
//!    It exists so a developer's own build still carries a useful stamp (and so
//!    the field is not empty for every build that is not a promotion), and it is
//!    reported as `provenance: "git"` so nothing downstream can mistake a sniff
//!    for a promise.
//!
//! The sniff is still a *proxy* — it asks "did a file's mtime change" and hopes
//! that answers "did the commit change". That proxy is now implemented correctly
//! (see [`watch_commit_sources`]) but it can never be made total: an mtime can go
//! backwards, a restored-from-backup ref keeps its old timestamp, and a clock
//! skew defeats it. That irreducible gap is exactly why the pin, not the sniff,
//! is what `promote` trusts.

use std::process::Command;

// The dirt check's parser, shared with `tests/build_provenance.rs` so the
// regression test runs THIS code: `git_status_porcelain`, `dirt_at`,
// `porcelain_dirt`, `porcelain_is_wellformed`, `porcelain_paths`, `classify`,
// `dirty_value`.
include!("src/build_dirt.rs");

fn main() {
    refuse_to_build_over_a_deploy_image();

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/build_dirt.rs");

    // THE PIN. `rerun-if-env-changed` is the whole mechanism: cargo hashes the
    // VALUE of this variable into the fingerprint for this build script, so a
    // build with a different pin re-runs this file and recompiles the crate,
    // and a build with the same pin does neither. That is an exact, total
    // dependency on the thing we actually care about — unlike an mtime, which
    // is a guess about it.
    println!("cargo:rerun-if-env-changed=THREADS_BUILD_COMMIT_PIN");

    // Triggers for the FALLBACK sniff. Emitted whether or not a pin is present,
    // because the pin can appear or disappear between builds and the trigger set
    // must not depend on which way this build went.
    watch_commit_sources();

    // The package directories whose sources compile into this product. Computed
    // ONCE and used for both halves of the dirt question — what to watch, and
    // what to count — so the two can never drift apart.
    let scope = product_packages();

    // Triggers for the DIRT check. Without these the flag is cached and nothing
    // invalidates it; see [`watch_dirt_sources`] for what that cost and why
    // this set is deliberately WIDER than the set `classify` counts.
    watch_dirt_sources(scope.as_deref());

    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=THREADS_BUILD_EPOCH={epoch}");

    // The pin wins. It is a statement of intent by whoever is shipping; the
    // sniff is this build script's best guess about a repository it happens to
    // be standing in.
    let pin = std::env::var("THREADS_BUILD_COMMIT_PIN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let (commit, provenance) = match pin {
        Some(p) => (p, "pin"),
        None => match git_value(&["rev-parse", "HEAD"]) {
            Some(c) => (c, "git"),
            None => (String::new(), ""),
        },
    };
    println!("cargo:rustc-env=THREADS_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=THREADS_BUILD_PROVENANCE={provenance}");

    // Dirty means "the compiled inputs did not all come from that commit", so
    // the commit alone does not describe this image. Scoped to the files that
    // actually compile into THIS product — see [`product_packages`] for the
    // boundary and [`classify`] for the rule — rather than "any modified file",
    // so an edited README does not label a build dirty.
    //
    // THE EMPTY-OUTPUT TRAP, which is why this reads `git_status_porcelain` and
    // not `git_value`: `git status --porcelain` prints NOTHING for a clean tree.
    // The old helper folded empty stdout into its failure case, so a CLEAN tree
    // produced exactly the same value as "git could not run" — and
    // `commit_dirty` reported `known: false` for every clean build there has
    // ever been. `git_status_porcelain` separates "ran and said nothing" from
    // "did not run", and hands the output on UNTRIMMED (see `src/build_dirt.rs`).

    // The scope is published so it can be AUDITED from outside this file. A
    // dirt check is only as good as its scope, and a scope that lives only in a
    // build script is a scope nothing can test. `tests/build_provenance.rs`
    // recomputes the same closure with a REAL TOML parser — independence being
    // the whole point, since reusing the line scan would only prove it agrees
    // with itself — and that is the one check that can catch
    // [`path_dependencies`] silently missing a crate whose bytes ship inside
    // this binary.
    println!(
        "cargo:rustc-env=THREADS_BUILD_DIRT_SCOPE={}",
        scope.as_deref().map(|s| s.join(",")).unwrap_or_default()
    );

    let dirty = if commit.is_empty() {
        // Without a commit there is nothing for "dirty" to be relative to, so
        // the honest answer is unknown rather than a bare `0`.
        String::new()
    } else {
        dirty_value(product_input_dirt(scope.as_deref())).to_string()
    };
    println!("cargo:rustc-env=THREADS_BUILD_DIRTY={dirty}");
}

/// The env var `promote` sets to identify itself as the one caller allowed to
/// build into a tree that holds a live, signed deploy image.
const PROMOTE_TOKEN: &str = "THREADS_PROMOTE";

/// The identifier a *deploy* image's designated requirement names. Mirrors
/// `crate::update::STABLE_BUNDLE_ID`, which this file cannot import.
const STABLE_BUNDLE_ID: &str = "com.harness.threads";

/// **Refuse to emit into a target tree whose product is a live, signed deploy
/// image** — unless the caller is `promote`.
///
/// # The incident
///
/// On 7 October a `cargo test` relinked `target/debug/threads`, laying an
/// ad-hoc-signed file over the path the supervisor launches from. The running
/// daemon was unaffected (it holds its original image as an unlinked inode), but
/// the supervisor's cold-start gate refuses an unsigned image, so the next
/// restart would have logged `refused_unsigned` and fallen back to last-good.
///
/// Every agent working in this tree had been *instructed* to export a scoped
/// `CARGO_TARGET_DIR`. One invocation path did not honour it, and the instruction
/// had no way to notice. The proximate hole is fixed in
/// `scripts/clean-build-scratch.sh`, which pruned `$repo/target` regardless of
/// where the build it wrapped had actually written.
///
/// # THIS GUARD IS NOT SUFFICIENT ON ITS OWN, and the measurement says so
///
/// It is worth being exact about what this does and does not catch, because the
/// obvious reading — "a build script runs for every cargo invocation, so this
/// catches all of them" — **is false, and was measured to be false.**
///
/// Cargo runs a build script only when it considers it dirty. On a fully cached
/// build it does not run this file at all, and yet it *still* replaces the
/// product: cargo's uplift step **removes `<profile>/threads` and recreates it**
/// from `deps/threads-<hash>` on every invocation, even one that compiles
/// nothing. Measured directly — two consecutive `cargo build`s, 0.16s each,
/// `THREADS_BUILD_EPOCH` identical across both (so this script demonstrably did
/// not re-run), and the product's inode changed both times with its deploy
/// signature replaced by a bare ad-hoc `cdhash`. A cached `cargo test` needs to
/// rebuild nothing whatsoever to destroy a signed deploy image.
///
/// So the load-bearing guard cannot live inside cargo, because cargo can skip
/// every hook it offers while still unlinking the file. It is the **user
/// immutable flag** (`chflags uchg`) that `promote` sets on the deploy path after
/// signing it: cargo's uplift then fails with `Operation not permitted` and the
/// file is untouched, enforced by the kernel and independent of cargo's caching.
///
/// What *this* function is for, then, is the message. When it does run — which is
/// any build that has real work to do, the common case — it fails before anything
/// is written and explains the situation and the remedy. Without it the same
/// situation surfaces as `error: failed to remove file …: Operation not
/// permitted (os error 1)`, which is correct, loud, and tells an agent nothing
/// about why or what to do instead.
///
/// Its other virtue is indifference to *how* the target directory was chosen. It
/// reads `OUT_DIR` — cargo's own answer, settled after `CARGO_TARGET_DIR`, a
/// `--target-dir` flag, a `build.target-dir` in some `.cargo/config.toml` up the
/// parent chain, and a nested cargo that cleared its environment have all been
/// resolved. It never has to enumerate the bypasses, which is what made the
/// original incident possible: every agent had been *told* to scope the target
/// dir, and the one path that did not honour it had no way to notice.
///
/// Three layers, then, and they are not redundant — they cover different events:
/// the immutable flag covers every write to the deploy path including cached
/// uplifts; this function converts the ones it sees into an explanation;
/// `scripts/clean-build-scratch.sh` covers the **prune**, an `rm -rf` that runs
/// no build script and that no filesystem flag on the product protects, because
/// what it deletes is the `deps/` entry the product is linked from.
///
/// # What it keys on, and why that and nothing else
///
/// The question is not "is this the default target tree" — that is a path
/// heuristic, and the deploy path is a registry entry in `promote` that can move.
/// The question is "does the product at this location **carry a deploy
/// signature**", i.e. a designated requirement naming [`STABLE_BUNDLE_ID`]. That
/// is the exact property the daemon's absorb path and the supervisor's cold-start
/// gate test, only the holder of the signing key can produce it, and `cargo`
/// cannot create it by accident. So the guard protects precisely the asset that
/// has value and is silent everywhere else:
///
/// * an ordinary scratch tree — no product, or an ad-hoc one: **no refusal**,
///   nothing of value is at risk;
/// * a tree holding a signed deploy image: **refused**, loudly, naming the fix;
/// * `promote`, which is *supposed* to replace that image: allowed by token.
///
/// The refusal is deliberately broader than linking. `cargo check` does not
/// relink the product, so it could have been exempted — but there is no way for a
/// build script to tell a check from a build, and the stronger invariant is
/// simpler to hold and to reason about: **nothing but `promote` runs cargo in a
/// deploy tree.** The cost is one error message with the remedy in it; the thing
/// it replaces is a silent clobber nobody noticed for most of a day.
fn refuse_to_build_over_a_deploy_image() {
    // promote identifies itself. This is not a security boundary — a build
    // script cannot have one, since anything that can set an env var can set
    // this one. It is an interlock against ACCIDENT, which is what the incident
    // was, and it is honest about which of the two it is.
    if std::env::var(PROMOTE_TOKEN).ok().is_some_and(|v| v == "1") {
        return;
    }

    let Some(product) = deploy_product_path() else { return };
    if !product.exists() {
        return;
    }
    if !is_stably_signed(&product) {
        return;
    }

    // `cargo:warning` first: a build-script panic's output is easy to lose in a
    // long cargo run, and this message is the entire value of the refusal.
    for line in [
        format!("REFUSING to build into {}", product.display()),
        "that file is a SIGNED DEPLOY IMAGE — its designated requirement names".to_string(),
        format!("{STABLE_BUNDLE_ID}, which only the signing key can produce. A build here would"),
        "relink over it with an ad-hoc image, the supervisor's cold-start gate would".to_string(),
        "refuse that file, and a restart would fall back to last-good.".to_string(),
        String::new(),
        "Note that cargo would replace it even with NOTHING to rebuild: its uplift".to_string(),
        "step unlinks and recreates the product on every invocation, which is why the".to_string(),
        "file is also held immutable (chflags uchg) and why a cached build you expect".to_string(),
        "to be a no-op is not one.".to_string(),
        String::new(),
        "Build somewhere else:".to_string(),
        "    export CARGO_TARGET_DIR=\"$PWD/.target\"".to_string(),
        String::new(),
        "To replace the deploy image, use the one tool that is allowed to:".to_string(),
        "    core-plugins/core/tools/promote publish threads".to_string(),
    ] {
        println!("cargo:warning={line}");
    }
    eprintln!(
        "\nlib/threads/build.rs: refusing to build into {} — it holds a signed deploy image \
         (designated requirement names {STABLE_BUNDLE_ID}). Set CARGO_TARGET_DIR to a scratch \
         directory, or use `promote publish threads`.\n",
        product.display()
    );
    std::process::exit(1);
}

/// The product path this build would write, derived from cargo's own `OUT_DIR`.
///
/// `OUT_DIR` is `<target>/[<triple>/]<profile>/build/<pkg>-<hash>/out`, so the
/// profile directory — the one the binary is uplifted into — is four levels up.
/// Climbing from `OUT_DIR` rather than reading `CARGO_TARGET_DIR` is the point:
/// it reports where the output is *actually* going, after cargo has resolved the
/// env var, any `--target-dir`, and any `build.target-dir` in a config file up
/// the parent chain.
fn deploy_product_path() -> Option<std::path::PathBuf> {
    let out_dir = std::env::var("OUT_DIR").ok()?;
    let profile_dir = std::path::Path::new(&out_dir).ancestors().nth(3)?;
    Some(profile_dir.join("threads"))
}

/// Does this file carry a DEPLOY signature — a designated requirement naming
/// [`STABLE_BUNDLE_ID`]?
///
/// Reads the **designated requirement**, never the `Identifier=` line: an ad-hoc
/// image from `cargo build` can carry a plausible-looking identifier while its DR
/// is a bare `cdhash H"…"` that names nothing and can never be absorbed. This is
/// the same question `crate::update`'s absorb path asks, asked the same way.
#[cfg(target_os = "macos")]
fn is_stably_signed(path: &std::path::Path) -> bool {
    match Command::new("codesign").args(["-d", "-r-"]).arg(path).output() {
        // codesign writes the requirement to stderr, so both streams are read.
        Ok(out) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            text.contains(&format!("identifier \"{STABLE_BUNDLE_ID}\""))
        }
        // No codesign, or it could not run. Degrade to NOT refusing: a guard
        // that cannot read the file must not block every build on this machine.
        Err(_) => false,
    }
}

#[cfg(not(target_os = "macos"))]
fn is_stably_signed(_path: &std::path::Path) -> bool {
    // No Mach-O code signatures to protect; the deploy regime this guards is
    // macOS-only.
    false
}

/// Emit a `rerun-if-changed` for every file whose change can move the commit
/// that the git sniff would record.
///
/// # Why this asks `git` for paths instead of joining them onto `--git-dir`
///
/// `git rev-parse --git-path <p>` is the one primitive that knows **which paths
/// are per-worktree and which are shared**. In a linked worktree:
///
/// ```text
/// --git-path HEAD        -> <git-dir>/HEAD                     (per-worktree)
/// --git-path refs/heads/b -> <common-dir>/refs/heads/b          (SHARED)
/// --git-path packed-refs -> <common-dir>/packed-refs            (SHARED)
/// ```
///
/// That `commondir` indirection is the whole bug the old code fell into, and
/// `--git-path` resolves it for us rather than us reimplementing it (which would
/// mean parsing `<git-dir>/commondir`, honouring its relative form, and keeping
/// up with git's own list of shared paths).
///
/// # Why the `.exists()` guard is safe HERE when it was not safe before
///
/// A `rerun-if-changed` on a path that does not exist makes cargo re-run this
/// script on **every** build, which would recompile the crate every time — and
/// since `THREADS_BUILD_EPOCH` changes on every run, that recompile would be
/// real work, not a no-op. So the guard has to stay. What makes it safe now is
/// that the trigger set is **overlapping and complete**, so no single file's
/// absence leaves the commit unwatched:
///
/// * on a branch, the ref file and `packed-refs` are *both* watched, and a ref
///   is in one or the other, always;
/// * `git gc` can pack a loose ref away mid-session — which **deletes** the ref
///   file — and `packed-refs` then both holds the ref and gets a fresh mtime;
/// * on a detached HEAD there is no branch ref, and `HEAD` itself is the only
///   thing that can move the commit, which is exactly what is watched.
///
/// The old code's guard dropped its *only* trigger for the moving ref and
/// nothing said so. A guard over a complete set drops redundancy; a guard over
/// a single point of failure drops the signal.
fn watch_commit_sources() {
    // 1. This worktree's own HEAD. Catches a detached-HEAD move and catches
    //    switching branches (which rewrites the symref). It is NOT enough on its
    //    own, because while you stay on one branch this file never changes.
    if let Some(p) = git_path("HEAD") {
        watch(&p);
    }

    // 2. The ref HEAD points at — THE FILE THAT ACTUALLY MOVES ON A COMMIT.
    //    Empty on a detached HEAD, where there is no such ref and (1) suffices.
    if let Some(full) = git_value(&["rev-parse", "--symbolic-full-name", "HEAD"]) {
        if let Some(p) = git_path(&full) {
            watch(&p);
        }
    }

    // 3. `packed-refs`, always, when it exists. A ref can live here instead of
    //    in a loose file, and can be moved here at any moment by `git gc`. This
    //    repository is a live example of the overlap mattering: `interface-layer`
    //    exists as a loose ref AND as a STALE entry in `packed-refs` that the
    //    loose file shadows. Watching only one of the two is a coin flip.
    if let Some(p) = git_path("packed-refs") {
        watch(&p);
    }
}

fn watch(p: &std::path::Path) {
    if p.exists() {
        println!("cargo:rerun-if-changed={}", p.display());
    }
}

/// Resolve a repository path the way git itself would, honouring the
/// per-worktree vs shared split. Relative output is resolved against the crate
/// directory, which is where [`git_ok`] runs git.
fn git_path(p: &str) -> Option<std::path::PathBuf> {
    let out = git_value(&["rev-parse", "--git-path", p])?;
    let path = std::path::PathBuf::from(out);
    if path.is_absolute() {
        return Some(path);
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    Some(std::path::PathBuf::from(dir).join(path))
}

/// Run `git` in the crate directory and report whether it RAN SUCCESSFULLY,
/// keeping its output **even when that output is empty**, and VERBATIM: this
/// helper does not trim. Trimming is [`git_value`]'s job, for one-token answers
/// like `rev-parse` where whitespace carries nothing. Output whose leading
/// whitespace is part of the format (`git status --porcelain`) never comes
/// through here at all — see `git_status_porcelain` in `src/build_dirt.rs`.
///
/// `None` means git genuinely could not answer: it is missing, this is not a
/// repository, it exited non-zero, or it printed non-UTF-8. `Some("")` means it
/// ran and had nothing to say, which for `git status --porcelain` is the
/// affirmative answer "the tree is clean".
///
/// Collapsing those two into one value is the secondary defect this replaces.
fn git_ok(args: &[&str]) -> Option<String> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let out = Command::new("git").current_dir(dir).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// [`git_ok`] for queries whose empty answer is meaningless — `rev-parse` and
/// friends, where "" is not a value. `None` on failure **or** on no answer.
fn git_value(args: &[&str]) -> Option<String> {
    git_ok(args).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

// ──────────────────────────────────────────────────────────────────────────
//  IS THE TREE DIRTY IN FILES THAT GO INTO *THIS* PRODUCT?
//
//  Two things had to be fixed here, and they are different kinds of wrong.
//
//  # 1. THE SCOPE WAS THE WHOLE REPOSITORY, AND THE COMMENT SAID OTHERWISE
//
//  The old check ran `git status --porcelain` repo-wide and flagged any path
//  ending `.rs`, `.sql`, `.toml` or `.lock`. Its comment claimed it was "scoped
//  to what actually enters the product — the same scope `promote` refuses on."
//  It was neither. Measured on a real promotion: the single matching path was
//  `.mind/v5-pools/interface-layer/.pen.lock` — an agent's WORKSPACE PEN LEASE,
//  a tracked file that every agent writes just by picking up the board — and the
//  build stamped `dirty=1`. `promote`'s own correctly-scoped check passed at the
//  same moment. The stamp refused a promotion over a file that cannot reach a
//  compiler.
//
//  That is not a cosmetic error, because `image::build_commit()` — THE VERSION
//  IDENTITY, the thing the registry and the rollback path read — returns `None`
//  whenever `dirty != "0"`. A pen lease erased this image's identity. It is also
//  why `tests/self_update.rs` had to make its positive assertion conditional,
//  with the comment "this workspace is routinely dirty": the workspace was not
//  routinely dirty in anything that compiles, it was routinely holding a lock
//  file.
//
//  Three independent widenings, all of them removed:
//
//  * `.toml`/`.lock` ANYWHERE matched `.pen.lock`, `etc/gateway.toml`, and
//    `lib/threads/scripts/threads-app/uv.lock` (a Python app vendored inside
//    this very package). Only `Cargo.toml`, `Cargo.lock` and `rust-toolchain*`
//    can change a compile.
//  * No package scope at all: a doc-comment edit in `lib/tts` labelled a
//    `threads` build dirty.
//  * `l.get(3..)` took the path raw, so git's quoted form for an unusual name
//    kept its trailing `"` and defeated the suffix test — a MISS, the direction
//    that matters. A rename's `old -> new` was likewise never split.
//
//  # 2. THE FLAG WAS CACHED AND NOTHING INVALIDATED IT
//
//  The trigger set was `rerun-if-env-changed=THREADS_BUILD_COMMIT_PIN` plus the
//  commit's three file watches. **Not one of those observes working-tree dirt**,
//  so the answer was sticky in both directions, and both were reproduced:
//
//  * a tree that *was* dirty reported dirty forever. Measured: dirt reverted,
//    tree clean, `cargo build` finished in 0.09s reusing the cached script
//    output, stamp still `dirty=1`. The promoting agent had to `touch build.rs`
//    to get a true answer — a workaround is the tell that the mechanism is the
//    defect.
//  * a tree that went dirty AFTER a cached run reported **clean** — the
//    dangerous one. Measured: `lib/threads/src/image.rs` modified, the crate
//    genuinely recompiled (12.40s, new bytes), the build script did NOT re-run
//    (epoch identical), and the product stamped `dirty=0` while naming commit
//    `dff72ef`. `build_commit()` then returns `Some(dff72ef)` for bytes that
//    commit does not describe. That is the identity system lying with
//    confidence, which is strictly worse than saying nothing.
//
//  The fix is to watch the files that can change the answer. The reason that is
//  affordable here, and was not affordable for "watch the working tree", is the
//  SCOPE above: after (1) the answer depends on two `src/` trees and a handful
//  of manifests, not on 13,000 paths.
//
//  # WHY THIS FILE STILL ANSWERS AT ALL, rather than deferring to `promote`
//
//  `promote` already computes this correctly, before building, and reads the
//  stamp back off the product before signing — so "let `promote` be the sole
//  authority and have `build.rs` stop claiming to know" is a real option and was
//  considered seriously. It is REJECTED AS A UNILATERAL CHANGE, for a reason
//  that is a property of the gate and not a matter of taste: `promote`'s
//  read-back refuses only on `dirty=1` (`if [[ "$stamp_dirty" == "1" ]]`). An
//  empty/unknown dirt field PASSES it. So a `build.rs` that stopped answering
//  would publish images whose `build_commit()` is permanently `None` — no
//  version identity at all — and would do it SILENTLY, past a gate that thinks
//  it checked. Deferring requires `promote` to both pass the dirt in and refuse
//  `unknown`, which is a change in `harness`, not here. Until those two land
//  together, this file answering correctly is the only option that does not make
//  the stamp worse.
// ──────────────────────────────────────────────────────────────────────────

/// Every package directory whose sources compile INTO this product, relative to
/// the repo root and `/`-separated: this crate plus the transitive closure of
/// its LOCAL path dependencies. `None` when that cannot be established, which
/// propagates to an honest `dirty=` *unknown* rather than a cheerful `0`.
///
/// # How this boundary was determined
///
/// Mechanically, from three questions about this crate, not from a guess:
///
/// 1. *What does cargo compile for the product target?* The crate's own `src/`
///    tree, its `Cargo.toml` and its `build.rs`. Deliberately NOT `tests/`,
///    `benches/`, `examples/`, `docs/` or `scripts/`: those build other targets
///    or nothing at all, and none of their bytes reach the binary. (`promote` is
///    coarser and counts any `.rs` in the package — see the note below.)
/// 2. *What non-Rust files are compiled in?* Every `include_str!`/
///    `include_bytes!` argument in the crate was enumerated; the only one
///    outside `src/` is `../schema.sql`, which `store.rs` compiles in — the
///    near-miss `promote`'s own comment calls out, since a half-finished schema
///    ships straight into the live store. Hence root-level `*.sql`.
/// 3. *What other local crates' bytes end up inside this one?* `threads`
///    declares `shapewire = { path = "../shapewire" }`, whose code is linked in.
///    **`promote` does not catch this**: its classifier calls `lib/shapewire`
///    "another package" and reports it without refusing, so a dirty `shapewire`
///    could ship inside a signed `threads` and neither side would say so. The
///    closure is walked rather than hardcoded so a new path dependency cannot
///    quietly fall outside the scope.
///
/// Everything here is a SUBSET of what `promote` refuses on, except item 3
/// where it is a superset. The subset direction is safe by construction:
/// `promote` runs its own wider check BEFORE this build and refuses first, so
/// anything this scope omits never reaches a stamp. The comment this replaces
/// claimed the two scopes were identical; they are not, and the asymmetry is
/// stated here rather than asserted away.
fn product_packages() -> Option<Vec<String>> {
    let root = std::fs::canonicalize(git_value(&["rev-parse", "--show-toplevel"])?).ok()?;
    let me = std::fs::canonicalize(std::env::var("CARGO_MANIFEST_DIR").ok()?).ok()?;

    let mut out: Vec<String> = Vec::new();
    let mut queue = vec![me];
    while let Some(dir) = queue.pop() {
        // A dependency outside the repository cannot be judged by this repo's
        // `git status`, so the honest answer for the whole product is unknown.
        let rel = dir.strip_prefix(&root).ok()?.to_string_lossy().replace('\\', "/");
        if rel.is_empty() || out.contains(&rel) {
            continue;
        }
        let manifest = dir.join("Cargo.toml");
        let text = std::fs::read_to_string(&manifest).ok()?;
        out.push(rel);
        for dep in path_dependencies(&text) {
            // Only a directory that actually holds a manifest is a package. A
            // `path = "…"` the scan picked up that is NOT one (a build input, a
            // `[patch]` entry, an over-match) resolves to nothing and is
            // dropped — which is why over-matching is harmless here and
            // under-matching is what the published scope exists to catch.
            let Ok(p) = std::fs::canonicalize(dir.join(&dep)) else { continue };
            if !p.join("Cargo.toml").is_file() {
                continue;
            }
            queue.push(p);
        }
    }
    out.sort();
    Some(out)
}

/// The `path = "…"` values in a manifest, by line scan.
///
/// A build script with no dependencies cannot parse TOML, and acquiring a
/// `[build-dependencies]` entry on a TOML crate to answer this would put a
/// third-party parser in front of every build of this daemon. The scan handles
/// the form actually used here (`dep = { path = "../x", … }`) and the plain
/// `path = "../x"` under a `[dependencies.dep]` header.
///
/// It is deliberately LOOSE: a false positive is dropped by the caller (no
/// manifest at the path), while a false negative would silently shrink the dirt
/// scope. That asymmetry is why the resulting closure is published as
/// `THREADS_BUILD_DIRT_SCOPE` and re-derived with a real TOML parser in
/// `tests/build_provenance.rs` — the scan is not trusted, it is checked, and
/// that test is forced red by stubbing this function out.
fn path_dependencies(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let mut rest = line;
        while let Some(i) = rest.find("path") {
            rest = &rest[i + "path".len()..];
            let after = rest.trim_start();
            let Some(after) = after.strip_prefix('=') else { continue };
            let after = after.trim_start();
            let Some(after) = after.strip_prefix('"') else { continue };
            let Some(end) = after.find('"') else { continue };
            out.push(after[..end].to_string());
        }
    }
    out
}

/// The dirt verdict for THIS build: `dirt_at` (in `src/build_dirt.rs`, where the
/// full contract and the ignored-file limit are written down) over the crate
/// directory and [`product_packages`]' scope. An unknown scope is unknown dirt —
/// never an unscoped check and never a silent pass.
fn product_input_dirt(scope: Option<&[String]>) -> Option<bool> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    dirt_at(std::path::Path::new(&dir), scope?)
}

/// Emit a `rerun-if-changed` for everything that can change the dirt answer.
///
/// # The choice, and what it costs
///
/// Cargo caches a build script's output and re-runs it only when a declared
/// trigger moves. The commit had such triggers; dirt had none, so the flag was
/// stale in both directions. A build script cannot watch a whole working tree —
/// but after the scope fix it does not have to: the answer depends on two `src/`
/// trees and a few manifests.
///
/// **The watch is deliberately WIDER than [`classify`] counts.** `src` is
/// watched as a DIRECTORY, so cargo re-runs this script when any file under it
/// is added, changed or removed, including an untracked one that no watch could
/// have named in advance. Watching more than is counted is the safe asymmetry:
/// an extra re-run costs a recomputation that arrives at the same answer, while
/// an uncounted change costs a stamp that lies.
///
/// Cost, measured on this crate: a re-run of this script plus the crate
/// recompile it forces (the epoch changes, so the compile is real) is **12.2s**.
/// It is charged when anything under a watched `src/` changes — which is exactly
/// when that recompile was going to happen anyway, so the marginal cost of
/// knowing the truth is this script's own runtime, four `git` calls. A cached
/// build is unaffected: still 0.09s, because none of these triggers moved.
///
/// What is NOT watched, and why:
///
/// * `tests/`, `benches/`, `docs/`, `scripts/` — not product inputs, so a test
///   edit no longer forces a lib recompile and no longer counts as dirt.
/// * the package root as a directory — `lib/threads/threads.db` lives there and
///   is a multi-gigabyte store that changes constantly. Watching the package
///   root would re-run this script, and recompile the crate, on every write to
///   it. `src` is watched instead, which is where source lives.
///
/// # The residual gap, stated rather than papered over
///
/// This is still an mtime proxy, and inherits the same irreducible limits the
/// commit sniff documents above: an mtime can move backwards, a restore from
/// backup keeps an old timestamp, and a clock skew defeats both. A new compiled
/// input placed at a package's ROOT (not under `src/`) is counted but unwatched;
/// it can only reach the product through an `include_str!` in a watched `src/`
/// file, so the edit that makes it matter does fire a re-run.
fn watch_dirt_sources(scope: Option<&[String]>) {
    let Some(scope) = scope else { return };
    let Some(root) = git_value(&["rev-parse", "--show-toplevel"]) else { return };
    let root = std::path::Path::new(&root);

    for g in ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "rust-toolchain", ".cargo"] {
        watch(&root.join(g));
    }

    for pkg in scope {
        let dir = root.join(pkg);
        // A directory watch: cargo walks it, so an ADDED or REMOVED file counts,
        // which a per-file watch could never cover for a file that does not
        // exist yet.
        watch(&dir.join("src"));
        watch(&dir.join("Cargo.toml"));
        watch(&dir.join("build.rs"));
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "sql") {
                    watch(&p);
                }
            }
        }
    }
}
