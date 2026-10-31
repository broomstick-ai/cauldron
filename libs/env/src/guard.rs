//! The production guard.
//!
//! **Where the live store is, as of 2026-10-07.** It moved out of the repo to
//! `~/.cauldron/data/prod/threads/store/`, and the byte-identical copy left at
//! `~/code/substrate-interface-layer/lib/threads/threads.db`
//! (3.8 GB) is now the ROLLBACK. Both are off limits, for different reasons:
//!
//! * the old tree is the rollback and must stay untouched, so every path under
//!   it is refused by name and by prefix ([`PROTECTED`],
//!   [`PROTECTED_NAME_SCOPE`]);
//! * the new store is production, and it is refused *structurally* rather than
//!   by filename — it sits inside the `prod` environment's data root, so
//!   [`require_inside`] keeps every other environment out of it, and nothing
//!   hands a `dev` service a path in it.
//!
//! The distinction matters because the name rule used to match `threads.db`
//! wherever it appeared, which meant that after the move this guard refused
//! production's own correct home. See [`PROTECTED_NAME_SCOPE`].
//!
//! **`tts` moved the same way on 2026-10-08** — out of `lib/tts` (the crate
//! directory, holding `tts.db` and 1.0 GB of `audio/` beside `src/`) into
//! `~/.cauldron/data/prod/tts/store/` — with one difference the guard has to
//! account for: the daemon was NOT restarted, so `lib/tts` is simultaneously
//! the rollback AND the directory a live process is writing to. That makes it
//! strictly more dangerous than `lib/threads` is today, not less, and it is
//! refused by prefix and by name on the same terms.
//!
//! The guard has two halves and both are needed.
//!
//! * [`refuse_protected`] is a **denylist**. It is what makes the refusal
//!   message name production, and it catches a path that arrives from
//!   anywhere — an environment variable, a declaration, an argument.
//! * [`require_inside`] is an **allowlist by containment**. Every path a
//!   service is handed as a data root must sit inside that environment's data
//!   root. A denylist alone only knows the dangers it was told about; this
//!   half is what makes an unlisted danger refused too.
//!
//! A guard that refuses everything is not a guard, so both halves accept
//! ordinary paths and the test suite asserts that they do.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Result};

/// **WHOSE HOME THE PROTECTED TREES LIVE UNDER — DERIVED, NEVER WRITTEN.**
///
/// The protected trees are the operator's, and they live under his home
/// directory. Until REL-4.S4 they were written here as literal absolute paths
/// naming that directory, which meant the guard only protected the machine
/// whose username was typed into the source. REL-4.S4 replaced the literal with
/// a placeholder home that exists nowhere, and the denylist silently stopped
/// refusing anything real (REL-4.S4-I F1). So the home is now DERIVED at
/// runtime and every list below is RELATIVE to it.
///
/// Two derivations, and the guard protects the UNION:
///
/// * the **passwd home** of the running user — what `~` means with `$HOME`
///   unset. It does not follow a `$HOME` that a proof has repointed, so a proof
///   that sets `HOME=/tmp/ph` still cannot hand cauldron the operator's real
///   tree; and
/// * **`$HOME`**, when it is set and absolute — so a scratch HOME that mirrors
///   the layout is guarded on the same terms, which is what lets the test
///   suite prove the denylist against a tree it built itself.
///
/// Refusing under both is strictly more protective than either: there is no
/// path either one admits that the guard would have refused before.
pub fn operator_homes() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(h) = passwd_home() {
        out.push(h.clone());
    }
    if let Some(h) = std::env::var_os("HOME").map(PathBuf::from) {
        if h.is_absolute() && h != Path::new("/") {
            if let Ok(h) = normalize(&h) {
                if !out.contains(&h) {
                    out.push(h);
                }
            }
        }
    }
    out
}

/// The running user's home from the password database, independent of
/// `$HOME`. Read once per process by asking `/bin/sh` to expand `~` with an
/// EMPTY environment: with `HOME` unset a POSIX shell falls back to the passwd
/// entry. (`unsafe_code` is forbidden in this crate, so `getpwuid` is not
/// called directly.) `None` if that fails, and then `$HOME` is the only half.
pub fn passwd_home() -> Option<PathBuf> {
    static HOME: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let out = std::process::Command::new("/bin/sh")
            .env_clear()
            .args(["-c", "echo ~"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8(out.stdout).ok()?;
        let p = PathBuf::from(s.trim());
        (p.is_absolute() && p != Path::new("/")).then_some(p).and_then(|p| normalize(&p).ok())
    })
    .clone()
}

/// Every home in [`operator_homes`] joined with `rel`.
fn under_homes(rel: &str) -> Vec<PathBuf> {
    operator_homes().into_iter().map(|h| h.join(rel)).collect()
}

/// Every protected prefix `refuse_protected` refuses on this machine right now,
/// resolved: each entry of [`PROTECTED`] under each of [`operator_homes`].
pub fn protected_prefixes() -> Vec<(PathBuf, &'static str)> {
    let mut out = Vec::new();
    for (rel, why) in PROTECTED {
        for p in under_homes(rel) {
            out.push((p, *why));
        }
    }
    out
}

/// **STORES: paths that hold the operator's DATA, refused to every caller for
/// every purpose.**
///
/// Nothing in cauldron may write here, and nothing may treat a file here as an
/// image to launch. These are the two directories that still hold the
/// authoritative copies of what the two real services keep, and the second one
/// is still the directory the live `tts` daemon has open.
///
/// RELATIVE to each of [`operator_homes`]; see there for why.
pub const PROTECTED_STORES: &[(&str, &str)] = &[
    (
        "code/substrate-interface-layer/lib/threads",
        "the live threads store (threads.db, turns/) with a running daemon on it",
    ),
    (
        "code/substrate-interface-layer/lib/tts",
        "the tts store (tts.db, 1.0 GB of audio/) — the ROLLBACK since 2026-10-08, and still \
         the directory the live tts daemon has open",
    ),
];

/// **BUILD TREES: paths cauldron may not WRITE, but may READ an image out of.**
///
/// The distinction is new and it is the operator's ruling that forced it.
/// Wave 1's reason for refusing this prefix was, verbatim, *"wave 1 does not
/// migrate threads"* — a statement about scope, not about danger. Cauldron now
/// HOSTS threads, and the image it hosts is produced by a signing pipeline
/// that writes exactly here.
///
/// So the refusal is split rather than lifted, because the two halves protect
/// different things:
///
/// * **Writing** here is still refused everywhere [`refuse_protected`] is
///   called, which is every path cauldron creates: a build output directory, a
///   data root, a version directory. That is unchanged, and it is what stops
///   `cauldron build` ever producing bytes into a tree every agent on this
///   machine shares — the D-2 hazard, which is real and has fired (an agent's
///   test relinked `target/release/threads` on 2026-10-07, and that file is a
///   live daemon's launch path).
/// * **Reading** an image out of it is now permitted, by
///   [`refuse_as_image`] only. `cauldron build` with an `artifact` copies the
///   file into `data/shared/builds/<service>/<label>/` and the supervisor execs
///   the COPY, so permitting the read does not make the shared tree a launch
///   path — which is the thing the hazard was ever about.
///
/// RELATIVE to each of [`operator_homes`].
pub const PROTECTED_BUILD_TREES: &[(&str, &str)] = &[(
    "code/substrate-interface-layer/target",
    "the interface layer's build output — a tree every agent on this machine writes to. \
     Cauldron may READ an image out of it (see `refuse_as_image`) and may never write into it",
)];

/// Every prefix [`refuse_protected`] refuses. Both lists, so adding to either
/// one cannot forget to extend the write guard.
pub const PROTECTED: &[(&str, &str)] = &[
    PROTECTED_STORES[0],
    PROTECTED_STORES[1],
    PROTECTED_BUILD_TREES[0],
];

/// **The old repo prefix the NAME rules are scoped to.**
///
/// The name rules below ([`PROTECTED_NAMES`] and the `turns` rule) exist to
/// catch a copy of the store that was *moved or symlinked within the old
/// repository* — somewhere the two [`PROTECTED`] prefixes do not reach, such as
/// `lib/other/threads.db` or a sibling worktree's `lib/threads`. They are
/// scoped to this prefix, and that scope is load-bearing.
///
/// **Why it is a scope and not "wherever it appears".** Until 2026-10-07 the
/// operator's live store WAS this repo's `lib/threads/threads.db`, so matching
/// the bare filename anywhere cost nothing. On 2026-10-07 the live store moved
/// to `~/.cauldron/data/prod/threads/store/`, which is a cauldron data root —
/// the very place this guard tells a caller to put things. An unscoped name
/// rule therefore refused `~/.cauldron/data/prod/threads/store/threads.db`:
/// production's own new, correct, supervisor-blessed home. A guard that refuses
/// the thing it exists to protect is not protecting it, it is just an outage
/// with a good reason attached.
///
/// What still refuses the store at its NEW home is the containment half
/// ([`require_inside`]) plus the environment's own layout — not a filename.
///
/// RELATIVE to each of [`operator_homes`].
pub const PROTECTED_NAME_SCOPE: &str = "code/substrate-interface-layer";

/// File and directory names that are refused anywhere under
/// [`PROTECTED_NAME_SCOPE`], so that a copy of the store relocated or symlinked
/// *within the old repository* is refused too.
/// `tts.db` and its sidecars are here for the same reason and on the same
/// terms: `tts`'s store moved to `~/.cauldron/data/prod/tts/store/` on
/// 2026-10-08 and the copy left in `lib/tts` is the rollback — and, until the
/// daemon is restarted, still the live one. The prefix entry in [`PROTECTED`]
/// is the half that matters; this is the weaker half, which catches a copy
/// relocated or symlinked to some other path *within the old repository*.
/// It is scoped the same way, so production's own new home is not refused.
pub const PROTECTED_NAMES: &[&str] = &[
    "threads.db",
    "threads.db-wal",
    "threads.db-shm",
    "tts.db",
    "tts.db-wal",
    "tts.db-shm",
];

/// Lexically normalise: absolute, no `.`, no `..`, no trailing slash. We never
/// resolve symlinks, because a path that does not exist yet must be checkable
/// *before* it is created — which is the whole point of a guard.
pub fn normalize(p: &Path) -> Result<PathBuf> {
    if !p.is_absolute() {
        bail!("refusing a relative path `{}`: cauldron paths are always absolute", p.display());
    }
    let mut out = PathBuf::from("/");
    for c in p.components() {
        match c {
            Component::RootDir | Component::Prefix(_) => {}
            Component::CurDir => {}
            Component::ParentDir => {
                bail!(
                    "refusing `{}`: it contains `..`, and a path that can climb out of a data root \
                     cannot be guarded",
                    p.display()
                );
            }
            Component::Normal(s) => out.push(s),
        }
    }
    Ok(out)
}

/// The denylist half. Refuses `path` if it is, or is inside, a protected tree,
/// or if any component names a protected file.
pub fn refuse_protected(path: &Path) -> Result<PathBuf> {
    let p = normalize(path)?;
    for (prefix, why) in protected_prefixes() {
        let prefix = prefix.as_path();
        if p == prefix || p.starts_with(prefix) {
            bail!(
                "REFUSED: `{}` is inside the protected production path `{}` ({}). \
                 Cauldron never writes there. Pick a path under a cauldron data root instead.",
                p.display(),
                prefix.display(),
                why
            );
        }
    }
    // The name rules, scoped to the OLD repository. Outside it a file called
    // `threads.db` is somebody's ordinary file — including production's own,
    // which has lived at `~/.cauldron/data/prod/threads/store/threads.db` since
    // 2026-10-07. See [`PROTECTED_NAME_SCOPE`].
    for scope in under_homes(PROTECTED_NAME_SCOPE) {
        if !p.starts_with(&scope) {
            continue;
        }
        for c in p.components() {
            if let Component::Normal(s) = c {
                let name = s.to_string_lossy();
                if PROTECTED_NAMES.iter().any(|n| *n == name) {
                    bail!(
                        "REFUSED: `{}` names the protected production file `{}` inside the old \
                         threads repository `{}`. Cauldron never writes there.",
                        p.display(),
                        name,
                        scope.display()
                    );
                }
                if name == "turns" {
                    bail!(
                        "REFUSED: `{}` is inside the live threads turns store. \
                         Cauldron never writes there.",
                        p.display()
                    );
                }
            }
        }
    }
    Ok(p)
}

/// **MAY CAULDRON READ AN IMAGE OUT OF THIS PATH AND LAUNCH IT?**
///
/// Narrower than [`refuse_protected`] by exactly one list, and the narrowing
/// is the whole content of the function. It refuses [`PROTECTED_STORES`] — a
/// file inside the operator's data is DATA, and launching one as a binary is a
/// category error with a large sqlite store as its subject — and it permits
/// [`PROTECTED_BUILD_TREES`], because an image produced by the signing
/// pipeline in another repository is written exactly there and cauldron is now
/// the thing that hosts it.
///
/// **Why a separate function rather than an argument to the existing one.**
/// Because a boolean parameter named something like `for_read` is a thing a
/// caller gets wrong, and the caller that gets it wrong writes into the shared
/// build tree. There is one call site — `install_artifact`, which copies the
/// file OUT and never writes in — and giving it its own named door means
/// `refuse_protected`'s behaviour is untouched for every other caller and
/// cannot be relaxed by accident.
///
/// The name rules are applied too, so a store file relocated or symlinked
/// within the old repository is refused as an image for the same reason it is
/// refused as a write target.
pub fn refuse_as_image(path: &Path) -> Result<PathBuf> {
    let p = normalize(path)?;
    for (rel, why) in PROTECTED_STORES {
        for prefix in under_homes(rel) {
            let prefix = prefix.as_path();
            if p == prefix || p.starts_with(prefix) {
                bail!(
                    "REFUSED to treat `{}` as a service image: it is inside the protected store `{}` \
                     ({}). A file in the operator's data is DATA. Launching one as a binary is a \
                     category error, and the subject of this one is a live sqlite store.",
                    p.display(),
                    prefix.display(),
                    why
                );
            }
        }
    }
    for scope in under_homes(PROTECTED_NAME_SCOPE) {
        if !p.starts_with(&scope) {
            continue;
        }
        for c in p.components() {
            if let Component::Normal(s) = c {
                let name = s.to_string_lossy();
                if PROTECTED_NAMES.iter().any(|n| *n == name) || name == "turns" {
                    bail!(
                        "REFUSED to treat `{}` as a service image: it names the protected \
                         production file `{name}` inside the old threads repository `{}`.",
                        p.display(),
                        scope.display()
                    );
                }
            }
        }
    }
    Ok(p)
}

/// The allowlist half. Refuses `path` unless it sits inside `data_root`.
pub fn require_inside(data_root: &Path, path: &Path) -> Result<PathBuf> {
    let root = refuse_protected(data_root)?;
    let p = refuse_protected(path)?;
    if !p.starts_with(&root) {
        bail!(
            "REFUSED: `{}` is outside this environment's data root `{}`. \
             A service's data may only live inside the environment it was started in.",
            p.display(),
            root.display()
        );
    }
    Ok(p)
}
