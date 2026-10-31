//! Resolving the environment: the two knobs, and how they compose.
//!
//! ```text
//!   root       = $CAULDRON_HOME            if set and non-empty  (absolute)
//!              | $HOME/.cauldron           otherwise
//!   name       = `cauldron --env <name>`   if given on the command line
//!              | $CAULDRON_ENV             if set and non-empty
//!              | $ENV                      otherwise, if set and non-empty
//!              | "prod"                    otherwise
//!   data_root  = <root>/data/<name>
//!   svc data   = <root>/data/<name>/<service>/<kind>/
//! ```
//!
//! The last line is the DECIDED LAYOUT and it is what production occupies:
//! `~/.cauldron/data/prod/threads/store/` is service `threads`, kind `store`.
//! There is no `services/` level. `builds/` and `cache/` are the exception that
//! proves the rule — they are not per-environment, so they live under
//! `data/shared/` instead.
//!
//! **They compose by nesting and are orthogonal.** `HOME` (or
//! `CAULDRON_HOME`) relocates the whole cauldron root — builds, service
//! declarations, logs, every environment. `ENV` (or `CAULDRON_ENV`) segments
//! `data/` *within* whatever root was chosen. With both set,
//! `HOME=/tmp/x ENV=dev` gives `/tmp/x/.cauldron/data/dev`: the root knob
//! picks the house, the env knob picks the room, and neither overrides the
//! other.
//!
//! Why both are accepted, rather than one:
//!
//! * `HOME` is the operator's own suggestion and it is honoured *literally* —
//!   the root is `$HOME/.cauldron` with no special case, so `HOME=/tmp/x`
//!   relocates the root for free. The cost is that it also relocates `HOME`
//!   for every child process, which is sometimes exactly what you want (a
//!   service that keeps dotfiles) and sometimes not.
//! * `CAULDRON_HOME` relocates the cauldron root *only* and leaves the child's
//!   `HOME` alone. It wins over `HOME` when both are set, because it is the
//!   more specific statement.
//! * `ENV` is the operator's other suggestion, and it is the cheap knob: one
//!   root, many data roots, one build cache, one service catalog.
//!   `CAULDRON_ENV` wins over it, for two reasons. `ENV` is a *reserved*
//!   variable in POSIX sh and ksh — it names a file the shell sources at
//!   startup — so leaving `ENV=dev` exported in a shell makes every
//!   `sh -c` complain about a missing `dev`. And `ENV` is generic enough that
//!   some other tool may already own it in the operator's shell.
//!
//! For that reason the CLI exports **`CAULDRON_ENV`** to the services it
//! starts and deliberately does **not** export `ENV`. A service reads the
//! authoritative variable; the shell keeps its reserved one.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::guard;

/// Where a resolved value came from, so every command can print its banner and
/// a mistake is visible rather than silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Source {
    /// `cauldron --env <name>`: the command line, which wins over both
    /// variables because it is the most specific statement available.
    Flag,
    CauldronHome,
    Home,
    CauldronEnv,
    Env,
    Default,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Flag => "--env",
            Source::CauldronHome => "$CAULDRON_HOME",
            Source::Home => "$HOME/.cauldron",
            Source::CauldronEnv => "$CAULDRON_ENV",
            Source::Env => "$ENV",
            Source::Default => "default",
        }
    }
}

/// A resolved environment: the triple, plus where each half came from.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Environment {
    pub root: PathBuf,
    pub root_source: Source,
    pub name: String,
    pub name_source: Source,
    pub data_root: PathBuf,
}

/// **`--env <name>`, once per process.**
///
/// Set by the CLI's argument parser before any verb runs, and read by
/// [`Environment::resolve`] ahead of both variables. A `OnceLock` rather than
/// `set_var` alone, so the banner can say the name came from the FLAG and not
/// from a variable the operator never typed. The CLI ALSO exports it as
/// `CAULDRON_ENV` so a face it execs (the TUI) inherits the same environment —
/// see `cli/src/main.rs` `take_env_flag`.
static ENV_FLAG: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Record `--env <name>` for this process. Validated here, at the flag, so a
/// bad name is refused before any verb resolves anything. A second call is
/// refused rather than silently ignored: two `--env` flags are a mistake.
pub fn set_env_flag(name: &str) -> Result<()> {
    validate_name(name, Source::Flag)?;
    ENV_FLAG
        .set(name.to_string())
        .map_err(|_| anyhow::anyhow!("`--env` was given twice; give it once"))
}

fn var(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

/// Names under `data/` that are NOT environments, and may therefore never be
/// used as one.
///
/// `shared` is the whole list, and it is reserved because `data/shared/` already
/// exists and holds the things that are deliberately NOT per-environment — model
/// weights (`data/shared/weights/`), build output
/// (`data/shared/builds/<service>/<label>/`) and the cargo cache
/// (`data/shared/cache/target/`). The operator's split is *"builds and caches are
/// shared; data is per-environment"*, and `data/shared` is where the first half
/// lives.
///
/// So `CAULDRON_ENV=shared` would not create a new environment — it would resolve
/// `data_root` onto the shared tree and hand a service the machine's weights and
/// build cache as its own state directory. The first `rm -rf` of that
/// "environment" would take 1.5 GB of weights and every build with it. A name
/// collision between an environment and a reserved sibling is not a thing to
/// detect later; it is refused at resolution, which is the only moment both
/// halves are known.
pub const RESERVED_ENV_NAMES: &[&str] = &[SHARED_NAME];

/// **Names a SERVICE may not have**, because the environment's data root
/// already keeps something of that name directly inside it.
///
/// This list exists because the layout is `data/<env>/<service>/<kind>/` with
/// no `services/` level (see [`Environment::service_data`]). That flattening is
/// what puts a service's directory in the same namespace as the environment's
/// own `seams.json`, `env.toml`, `seams.lock` and `logs/`, so the collision has
/// to be refused at resolution rather than discovered when a service overwrites
/// the seam table it was supposed to register in.
///
/// **`cauldron` IS THE SECOND ENTRY, AND IT IS NOT A LAYOUT COLLISION LIKE THE
/// FIRST.** `logs` is reserved because `data/<env>/logs/` already exists.
/// `cauldron` is reserved because the SUPERVISOR registers a seam under that
/// name for itself — `cauldron daemon` is a producer like any other, under the
/// same compile-time warnings obligation it imposes, so that its presence is a
/// FACT IN THE TABLE a face can probe rather than something it has to be told.
/// A declared service called `cauldron` would overwrite the holder's own row,
/// and the row answering "is anything supervising this environment" is the one
/// row that must not be forgeable by dropping a file in `services/`.
///
/// **`logs` HAS THE ARGUMENT BELOW; `cauldron` does not need one.**
///
/// ---
///
/// **`logs` was the only entry, and that was not an oversight.** Four of those
/// siblings share the namespace, but only `logs` is a name a service can
/// actually be called. [`crate::decl::load`] accepts a service name of
/// `[A-Za-z0-9_-]` only, and it is the sole way a `Declaration` is ever
/// produced: it refuses any name containing `.` before a data root is
/// resolved, and it additionally refuses a declaration whose `name` key
/// disagrees with the stem it was loaded as, so `d.name` reaching
/// [`Environment::service_data`] is always a name that already passed that
/// filter. `seams.json`, `seams.lock` and `env.toml` therefore cannot reach
/// this check by any path — they were listed anyway, and three quarters of a
/// guard that cannot fire teaches the next reader that the dotted siblings are
/// what is being protected. They are protected by the character set, one layer
/// earlier. `logs` is the only sibling spelled in the accepted character set,
/// so `logs` is the only name this list has to refuse.
///
/// Adding an entry here is right only for a new data-root sibling whose name a
/// declaration could legally carry; a sibling with a `.` in it needs nothing.
///
/// Kept beside [`RESERVED_ENV_NAMES`] deliberately: both are "this name is
/// already taken by the layout", one rung apart.
pub const RESERVED_SERVICE_NAMES: &[&str] = &["logs", "cauldron"];

/// The one reserved name, spelled once.
///
/// [`Environment::shared_root`] and [`RESERVED_ENV_NAMES`] both read it, so the
/// directory that is created and the name that is refused cannot drift apart —
/// which is the only way this reservation could fail silently.
pub const SHARED_NAME: &str = "shared";

/// A name that is safe as one path component and readable in a banner.
fn validate_name(name: &str, source: Source) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name.chars().next().map(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) == Some(true)
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !ok {
        bail!(
            "REFUSED: environment name `{name}` (from {}) is not usable. \
             An environment name is 1-32 characters of [a-z0-9_-], starting with [a-z0-9]. \
             Nothing is guessed or sanitised, because a silently rewritten name is a silently \
             wrong data root.",
            source.as_str()
        );
    }
    // Checked AFTER the character rule, so a name that is both unusable and
    // reserved is reported as unusable — the more basic fault, and the one whose
    // message tells the reader the actual grammar.
    if RESERVED_ENV_NAMES.contains(&name) {
        bail!(
            "REFUSED: `{name}` (from {}) is a RESERVED name under `data/`, not an environment. \
             `data/{name}/` holds what is deliberately not per-environment — model weights, \
             builds and the build cache — so resolving an environment onto it would hand a \
             service the machine's shared state as its own data root, and deleting that \
             \"environment\" would delete the weights and every build with it. \
             Reserved names: {}. Pick any other name.",
            source.as_str(),
            RESERVED_ENV_NAMES.join(", ")
        );
    }
    Ok(())
}

impl Environment {
    /// Resolve from the process environment.
    pub fn resolve() -> Result<Self> {
        let (root, root_source) = match var("CAULDRON_HOME") {
            Some(v) => (PathBuf::from(v), Source::CauldronHome),
            None => {
                let home = var("HOME").context(
                    "neither $CAULDRON_HOME nor $HOME is set, so there is no cauldron root to \
                     resolve. Nothing is defaulted: a guessed root is a guessed data root.",
                )?;
                (Path::new(&home).join(".cauldron"), Source::Home)
            }
        };
        let (name, name_source) = match (ENV_FLAG.get().cloned(), var("CAULDRON_ENV"), var("ENV")) {
            (Some(f), _, _) => (f, Source::Flag),
            (None, Some(v), _) => (v, Source::CauldronEnv),
            (None, None, Some(v)) => (v, Source::Env),
            (None, None, None) => ("prod".to_string(), Source::Default),
        };
        validate_name(&name, name_source)?;

        // The guard runs on the ROOT, before anything else, so that a root
        // pointed at production is refused at the very first step.
        let root = guard::refuse_protected(&root).with_context(|| {
            format!("the cauldron root came from {}", root_source.as_str())
        })?;
        let data_root = root.join("data").join(&name);
        guard::refuse_protected(&data_root)?;
        crate::alias::refuse_aliased_data_root(&root, &name, &data_root)?;
        Ok(Self { root, root_source, name, name_source, data_root })
    }

    /// `<data_root>/seams.json` — the seam table, which *is* the environment.
    pub fn seams(&self) -> PathBuf {
        self.data_root.join("seams.json")
    }
    /// `<data_root>/env.toml` — the per-environment version pins.
    pub fn manifest(&self) -> PathBuf {
        self.data_root.join("env.toml")
    }
    /// `<data_root>/<service>` — a service's own directory, inside which the
    /// service keeps one subdirectory per KIND of thing it holds:
    /// `data/<env>/<service>/<kind>/`.
    ///
    /// **This is the decided layout, and it is what production already
    /// occupies.** The live threads store moved to
    /// `~/.cauldron/data/prod/threads/store/` on 2026-10-07, with
    /// `.../threads/warnings/` and `.../threads/links/` beside it — service
    /// `threads`, kinds `store`, `warnings`, `links`. This function used to
    /// compute `data/<env>/services/<service>`, an extra `services/` level that
    /// nothing on disk had, so the layout the code described and the layout the
    /// machine ran were two different layouts. The `services/` level is gone
    /// rather than added to disk: production is the thing that is already
    /// right.
    ///
    /// The kinds are the SERVICE's to name — this function hands out the
    /// directory, never the kind. `cauldron start` exports this path as
    /// `$CAULDRON_SERVICE_DATA_DIR`, and a service joins its own kinds onto it
    /// (the warnings contract's durable record is
    /// `$CAULDRON_SERVICE_DATA_DIR/warnings/warnings.jsonl`, which is exactly
    /// `data/<env>/<service>/<kind>/` and needed no change).
    ///
    /// Still guarded into the data root, so a service cannot be handed another
    /// environment's state. And still refused if the service's name would
    /// collide with one of the data root's own files — see
    /// [`RESERVED_SERVICE_NAMES`], which is a NEW requirement created by
    /// flattening: with a `services/` level, a service called `logs` was
    /// `services/logs` and harmless; without one it is the environment's own
    /// `logs/` directory.
    pub fn service_data(&self, service: &str) -> Result<PathBuf> {
        if RESERVED_SERVICE_NAMES.contains(&service) {
            bail!(
                "REFUSED: `{service}` cannot be a service name. A service's data lives at \
                 `data/{}/<service>/`, directly inside the environment's data root, and \
                 `{service}` is already the name of something the environment keeps there. \
                 Reserved: {}. Rename the service.",
                self.name,
                RESERVED_SERVICE_NAMES.join(", ")
            );
        }
        let p = self.data_root.join(service);
        guard::require_inside(&self.data_root, &p)
    }
    /// **`<data_root>/cauldron` — THE SUPERVISOR'S OWN DIRECTORY.**
    ///
    /// Separate from [`Environment::service_data`] because that function
    /// REFUSES the name `cauldron`, and this is the function the reservation
    /// exists for. The name is reserved so that no declaration can claim it
    /// and overwrite the holder's row — the one row that answers "is anything
    /// supervising this environment", which must not be forgeable by dropping
    /// a file in `services/`. A reservation with no owner would just be a hole
    /// in the layout, so this is the owner.
    ///
    /// It holds what the supervisor keeps per environment: its warnings record
    /// at `cauldron/warnings/warnings.jsonl`, which is exactly the
    /// `data/<env>/<service>/<kind>/` shape every service uses, because the
    /// supervisor is a producer like any other.
    pub fn supervisor_data(&self) -> Result<PathBuf> {
        let p = self.data_root.join("cauldron");
        guard::require_inside(&self.data_root, &p)
    }

    /// `<data_root>/logs/<service>.log`
    pub fn log(&self, service: &str) -> Result<PathBuf> {
        let p = self.data_root.join("logs").join(format!("{service}.log"));
        guard::require_inside(&self.data_root, &p)
    }
    /// `<root>/data/shared` — the RESERVED, non-environment sibling of every
    /// `data/<env>`, holding what is deliberately shared across all of them.
    ///
    /// The operator's split is *"builds and caches are shared; data is
    /// per-environment"*, and this is where the first half lives. It sits under
    /// `data/` rather than beside it so that ONE directory — `data/` — holds
    /// every byte of state the machine keeps, and `shared` is refused as an
    /// environment name ([`RESERVED_ENV_NAMES`]) precisely so nothing can
    /// resolve an environment onto it.
    pub fn shared_root(&self) -> PathBuf {
        self.root.join("data").join(SHARED_NAME)
    }
    /// `<root>/data/shared/weights` — MODEL WEIGHTS ONLY.
    ///
    /// The operator's ruling is that this holds weights and is *"not a cache for
    /// application data"*: anything a program could regenerate or re-derive
    /// belongs in `cache/`, and anything belonging to one environment belongs in
    /// that environment. Weights are shared because they are enormous, immutable
    /// and identical for every environment — the exact case `shared` exists for.
    pub fn weights(&self) -> PathBuf {
        self.shared_root().join("weights")
    }
    /// `<root>/data/shared/builds/<service>/<label>` — builds are SHARED, not per
    /// environment, so one build can be pinned by `dev` while `prod` stays on
    /// the previous one. That split is what makes a version bump per seam
    /// cheap: build once, pin per environment.
    ///
    /// Moved under `data/shared/` on 2026-10-07 to match the agreed layout. The
    /// rationale is unchanged and the property it buys is unchanged — `shared` is
    /// not an environment, so "per root, not per environment" still describes it
    /// exactly; only the location moved, so that `data/` is the one tree holding
    /// state.
    pub fn build_dir(&self, service: &str, label: &str) -> PathBuf {
        self.shared_root().join("builds").join(service).join(label)
    }
    pub fn builds_for(&self, service: &str) -> PathBuf {
        self.shared_root().join("builds").join(service)
    }
    /// `<root>/data/shared/cache/target` — the cargo target dir the `build` verb
    /// uses, so intermediates are inside the cauldron root and deletable with it.
    ///
    /// Under `cache/` and NOT under `weights/`: this is the regenerable half, and
    /// the operator's ruling that weights are "not a cache for application data"
    /// cuts in both directions — a build cache must not be filed with weights
    /// either.
    pub fn cargo_target(&self) -> PathBuf {
        self.shared_root().join("cache").join("target")
    }
    /// `<root>/services` — the operator's declaration overrides.
    pub fn service_dir(&self) -> PathBuf {
        self.root.join("services")
    }

    /// One line, printed by every verb, so a typo'd `ENV` is visible instead
    /// of silently creating an empty environment.
    pub fn banner(&self) -> String {
        format!(
            "env {} (from {})  root {} (from {})  data {}",
            self.name,
            self.name_source.as_str(),
            self.root.display(),
            self.root_source.as_str(),
            self.data_root.display()
        )
    }

    /// The ROOT-level directories only — builds and declarations — without
    /// creating this environment's data root. For verbs that act on the root
    /// (`build`), so naming an environment never creates it as a side effect.
    pub fn ensure_shared_dirs(&self) -> Result<()> {
        for d in [self.shared_root().join("builds"), self.service_dir()] {
            guard::refuse_protected(&d)?;
            std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
        }
        Ok(())
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        for d in [
            self.data_root.clone(),
            // No `services/` level: a service's data dir is `data/<env>/<service>/`
            // and `cauldron start` creates it. See `service_data`.
            self.data_root.join("logs"),
            self.shared_root().join("builds"),
            self.service_dir(),
        ] {
            guard::refuse_protected(&d)?;
            std::fs::create_dir_all(&d)
                .with_context(|| format!("creating {}", d.display()))?;
        }
        Ok(())
    }
}

// ═══════════════════════════════════════════════ ENVIRONMENTS, IN THE PLURAL

impl Environment {
    /// **Resolve a NAMED environment inside an already-resolved root.**
    ///
    /// [`Environment::resolve`] reads the process environment and can only ever
    /// produce the ONE environment this process was launched into. That is
    /// correct for a service and for every verb that acts on an environment,
    /// and it is exactly wrong for the one question the operator asked next:
    /// *which environments are there, and what is running in each?*
    ///
    /// A verb cannot answer that by re-reading `$CAULDRON_ENV`, and it must not
    /// answer it by `setenv`-ing in a loop — mutating the process environment to
    /// look at a second environment is how a reader ends up writing into one.
    /// So the second environment is constructed, not switched to: the root and
    /// its provenance are carried over unchanged, the name is validated and
    /// guarded exactly as `resolve` validates and guards it, and the result is
    /// an ordinary [`Environment`] that every existing function already works
    /// on.
    ///
    /// The name's `Source` is recorded as [`Source::Default`] because it came
    /// from neither variable — it came from a directory listing or an argument,
    /// and the banner should not claim an environment variable set it.
    pub fn for_name(&self, name: &str) -> Result<Self> {
        validate_name(name, Source::Default)?;
        let data_root = self.root.join("data").join(name);
        guard::refuse_protected(&data_root)?;
        crate::alias::refuse_aliased_data_root(&self.root, name, &data_root)?;
        Ok(Self {
            root: self.root.clone(),
            root_source: self.root_source,
            name: name.to_string(),
            name_source: Source::Default,
            data_root,
        })
    }

    /// **Every environment that EXISTS in this root**, sorted, with the
    /// reserved names and anything unusable left out.
    ///
    /// A directory under `data/` is an environment if and only if its name is a
    /// legal environment name and is not reserved. That is the same predicate
    /// [`Environment::resolve`] applies, read in the other direction, so a
    /// directory this cannot name is a directory no `CAULDRON_ENV` could have
    /// created.
    ///
    /// It is deliberately a LISTING and not a registry file. There is nothing
    /// to keep in sync and nothing to go stale: an environment exists because
    /// its data root exists, which is the same fact that makes its seam table
    /// findable. `data/shared/` is skipped by [`RESERVED_ENV_NAMES`] rather
    /// than by a special case here, so the skip and the refusal are one rule.
    ///
    /// A missing `data/` is an empty list, not an error — "no environment has
    /// been used in this root yet" is a true statement.
    pub fn environments(&self) -> Result<Vec<String>> {
        let data = self.root.join("data");
        let rd = match std::fs::read_dir(&data) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("listing {}", data.display())),
        };
        let mut out = Vec::new();
        for e in rd.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let Some(name) = e.file_name().to_str().map(str::to_string) else { continue };
            if validate_name(&name, Source::Default).is_err() {
                continue;
            }
            // R9.C7f7 (inspect F2): what a failed `env create` leaves is not
            // an environment — the same predicate `env create` uses.
            if vacant_data_root(&e.path()) {
                continue;
            }
            out.push(name);
        }
        out.sort();
        Ok(out)
    }

    /// The environments that exist, PLUS this one, so the environment the
    /// caller is standing in is never missing from a listing of environments.
    ///
    /// An environment that has been resolved but never written to has no
    /// directory yet — `cauldron envs` in a fresh root would otherwise print a
    /// list that does not contain the environment whose banner it just printed.
    pub fn environments_including_self(&self) -> Result<Vec<String>> {
        let mut out = self.environments()?;
        if !out.iter().any(|n| n == &self.name) {
            out.push(self.name.clone());
            out.sort();
        }
        Ok(out)
    }
}

/// **A data root that is not an environment (R9.C7f7).** What a failed
/// `cauldron env create` leaves: the directory holding nothing but the
/// env.toml writers' lock file (`.env.toml.lock`, never removed — removing it
/// could hand a queued writer a lock on an orphan inode) and an empty `logs/`.
/// The ONE definition both `env create` (which may create over it) and
/// [`Environment::environments`] (which does not list it) use. Anything else
/// in the directory — env.toml, a holder's `supervisor.lock`, service data —
/// makes it an environment that exists, and so does an EMPTY directory (no
/// lock file). An unreadable directory is not vacant.
pub fn vacant_data_root(dir: &Path) -> bool {
    // The lock file must BE there: it is the mark a failed create leaves. An
    // empty directory is not that shape, and stays what it always was (an
    // environment, listed, and refused by `env create`).
    if !dir.join(".env.toml.lock").is_file() {
        return false;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    for entry in rd {
        let Ok(entry) = entry else { return false };
        let name = entry.file_name();
        if name == ".env.toml.lock" {
            continue;
        }
        if name == "logs" && std::fs::read_dir(entry.path()).map(|mut r| r.next().is_none()).unwrap_or(false) {
            continue;
        }
        return false;
    }
    true
}
