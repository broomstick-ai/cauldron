//! # THE THROWAWAY DEV DAEMON — its ports, its guard, and its seeded scenarios
//!
//! The operator: *"You can maybe add an optional daemon path to do some testing
//! on the daemon with dummy threads before messing with our prod."*
//!
//! Until now the only `threads` daemon on this machine was the live one, holding
//! his real threads, his real logins and a 3.8 GB events table, and every
//! diagnosis had to be conducted against it. This module is the isolated
//! instance: different ports, a different data dir, synthetic accounts and
//! synthetic threads, and a guard that refuses to open the production store.
//!
//! ## What is HERE and what is in the binary
//!
//! This module is PURE. It computes paths, decides whether a directory may be
//! used, and writes rows into a [`Store`] it is handed. It sets no environment
//! variable, spawns nothing, reads no fixture and prints nothing.
//!
//! The process-level isolation — redirecting `THREADS_HOME`, `CAULDRON_HOME` and
//! `THREADS_CLAUDE_BIN`, disabling the self-update watcher, printing the banner,
//! and finally calling [`crate::run_daemon`] — is `src/bin/threads-dev.rs`.
//! The split is deliberate: the tests can exercise every decision in here
//! without a process, and the dangerous half (mutating the environment) lives in
//! a binary that cannot be linked into the production daemon.
//!
//! ## There is only ONE boot path
//!
//! `threads-dev` does not stand a server up. It prepares a [`Config`] and hands
//! it to [`crate::run_daemon`] — the same function `main.rs` calls, which means
//! the bind-before-anything-writes rule, `update.on_boot()`, `recover()`, both
//! servers, the WAL keeper and the drain-and-checkpoint shutdown are shared, not
//! reimplemented. A second boot path that drifts from the real one is worse than
//! none, so there is not one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::accounts::AccountState;
use crate::error::{Error, Result};
use crate::event::{SendIntent, StoredEvent, ThreadEvent};
use crate::settings::{AccountChoice, ThreadSettings};
use crate::store::{now_rfc3339, ParkedTurnRow, RetryRow, Store, ThreadRow};
use crate::Config;

// ─────────────────────────────────────────────────────────────────────────────
// 1. The ports, the home, and the marker
// ─────────────────────────────────────────────────────────────────────────────

/// **THE DEV DAEMON'S PORT — 13653, and not 3653.**
///
/// Production's port is [`crate::config::DEFAULT_CHASSIS_PORT`] (3653). The
/// dev port is that plus 10000, landing nowhere near 3649 (mesh), 3652 (tts) or
/// anything else `core/tools/promote` health-probes. It used to have a twin,
/// `DEV_PORT` (13651), for the legacy door; that door was retired at R2.S4 and
/// the dev daemon serves this one port, exactly as production does.
///
/// It is a CONSTANT and not a defaulted flag. A flag whose default is
/// prod-adjacent is a trap: the day somebody runs the dev daemon without the
/// flag it binds the operator's port and the supervised daemon loses a race it
/// never asked to be in. There is no argument to `threads-dev` that makes it
/// serve 3653.
pub const DEV_CHASSIS_PORT: u16 = 13653;

/// **THE MARKER FILE, AND IT IS THE LOAD-BEARING HALF OF THE GUARD.**
///
/// A directory may be used as the dev daemon's data dir only if it does not
/// exist yet, is empty, or already carries this file. The dev daemon writes it
/// on first use; nothing else on this machine writes it.
///
/// The denylist in [`production_objection`] is the *first* lock and names the
/// specific production locations, so the refusal can say which one it hit. But a
/// denylist can only refuse paths somebody thought of, and the production store
/// has now moved TWICE: it was never `~/.threads`, it was the repo's own
/// `lib/threads/` until 2026-10-07, and it is now
/// `~/.cauldron/data/prod/threads/store/` (still by way of `THREADS_HOME`).
/// Each move invalidated a sentence somewhere in this file, which is the
/// argument for this marker: it is the lock that does not depend on anyone's
/// imagination OR on anyone's path being current. A store the dev daemon did
/// not create is a store the dev daemon does not open.
pub const DEV_MARKER: &str = "THROWAWAY-DEV-DAEMON";

/// What the marker file says, so that anyone who finds it knows what it is and
/// that deleting the directory costs nothing.
pub const DEV_MARKER_TEXT: &str = "\
This directory is a THROWAWAY `threads` dev daemon's data dir.

Everything in it is synthetic: fake accounts, fake threads, fake messages. It is
written by `threads-dev` (lib/threads/src/bin/threads-dev.rs) and it is safe to
delete at any time — `threads-dev --teardown` does exactly that.

It is NOT the production daemon's state. Production lives wherever THREADS_HOME
points (on this machine, since 2026-10-07: ~/.cauldron/data/prod/threads/store),
holds the operator's real threads and real logins, and `threads-dev` refuses to
open it.
";

/// The environment variable that moves the dev daemon's data dir.
///
/// **It is `THREADS_DEV_HOME` and deliberately NOT `THREADS_HOME`.** On this
/// machine `THREADS_HOME` is how the launchd supervisor points the LIVE daemon
/// at the production store, so a dev daemon that honoured it would follow the
/// operator's own shell straight into production. [`dev_home`] never reads it,
/// and [`production_objection`] refuses its value outright.
pub const DEV_HOME_ENV: &str = "THREADS_DEV_HOME";

/// The dev data dir: `$THREADS_DEV_HOME`, else `~/.threads-dev`.
///
/// `~/.threads-dev` is one character off `~/.threads` on purpose — it reads as
/// obviously adjacent to the real thing, which is what you want in a log line —
/// but the guard does not rely on the name for anything.
pub fn dev_home() -> Result<PathBuf> {
    if let Some(h) = std::env::var_os(DEV_HOME_ENV) {
        if !h.is_empty() {
            return Ok(PathBuf::from(h));
        }
    }
    let home = std::env::var("HOME").map_err(|_| Error::Config("HOME is not set".into()))?;
    Ok(PathBuf::from(home).join(".threads-dev"))
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. THE PRODUCTION GUARD
// ─────────────────────────────────────────────────────────────────────────────

/// Normalise a path that may not exist yet.
///
/// `canonicalize` fails on a path whose leaf is absent, which is the normal case
/// for a dev dir on first run — so this canonicalises the deepest ancestor that
/// DOES exist and re-appends the rest. Without it `~/.threads-dev` and
/// `/Users/testuser/./.threads-dev` would compare unequal and the guard would be
/// decorative.
fn normalize(p: &Path) -> PathBuf {
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        if let Ok(real) = cur.canonicalize() {
            let mut out = real;
            for part in suffix.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (cur.file_name().map(|f| f.to_os_string()), cur.parent()) {
            (Some(name), Some(parent)) if !parent.as_os_str().is_empty() => {
                suffix.push(name);
                cur = parent.to_path_buf();
            }
            // Nothing above us resolves (a relative path with no existing root,
            // or `/` itself). Hand back what we were given rather than guess.
            _ => return p.to_path_buf(),
        }
    }
}

/// `inner` is `outer`, or lives underneath it.
fn within(inner: &Path, outer: &Path) -> bool {
    inner.starts_with(outer)
}

/// **Environment names under `<cauldron>/data/` a dev daemon may NOT use.**
///
/// `prod` is production — since 2026-10-07 `<cauldron>/data/prod/threads/store`
/// IS the operator's live store. `shared` is cauldron's reserved
/// non-environment sibling (`cauldron::env::RESERVED_ENV_NAMES`), holding model
/// weights, builds and the build cache; it is not an environment and resolving
/// one onto it would hand a daemon the machine's shared state.
///
/// Everything else under `data/` is a legitimate environment and is admitted,
/// because that is what the environment mechanism exists to provide. This list
/// is the whole of the exception to [`production_objection`]'s clause 3, which
/// is why it is spelled out here rather than inlined.
pub const CAULDRON_ENVS_REFUSED: &[&str] = &["prod", "shared"];

/// If `dir` is `<cauldron>/data/<env>` or inside it, the `<env>`.
///
/// `None` for anything else under the cauldron home — the home itself,
/// `data/` itself, `chassis/`, `projects.json`, `plugins/` — all of which stay
/// refused. Both paths are already [`normalize`]d by the caller.
fn cauldron_environment(dir: &Path, cauldron: &Path) -> Option<String> {
    let rest = dir.strip_prefix(cauldron).ok()?;
    let mut parts = rest.components();
    match parts.next()? {
        std::path::Component::Normal(s) if s == "data" => {}
        _ => return None,
    }
    match parts.next()? {
        std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
        _ => None,
    }
}

/// **LOCK ONE: is this a production location?**
///
/// `Some(reason)` means refuse, and the reason names the specific thing it hit so
/// the refusal is actionable rather than a shrug. `None` means this lock has no
/// objection — which is NOT the same as "this directory is usable"; that is
/// [`claim_dev_dir`]'s answer, and it applies the marker lock too.
///
/// Five objections, each with a reason it is not covered by the others:
///
/// 1. **`$THREADS_HOME`.** THE one that matters on this machine, and since
///    2026-10-07 it is the clause that actually protects production. The live
///    daemon is pointed at `~/.cauldron/data/prod/threads/store` by
///    `~/.threads-supervisor/paths.env` exporting this variable, so anybody who
///    has sourced that file — or who copied a line out of it — has the
///    production store one defaulted path away. This clause followed the move
///    for free, because it reads the pointer rather than restating the path.
/// 2. **`~/.threads`.** The crate's own built-in production state home
///    ([`crate::config::state_home`]'s fallback). It happens not to exist here,
///    which is exactly why it has to be named: a guard that only refuses what is
///    currently populated stops working the moment somebody populates it.
/// 3. **`$CAULDRON_HOME`, else `~/.cauldron` — EXCEPT a non-production
///    environment's own data root.** The cauldron home holds the chassis
///    consumer ledger shared by every producer on the machine, and the hard
///    don'ts name it explicitly. But since 2026-10-07 it also holds
///    `data/<env>/`, and segmenting state by environment is the entire purpose
///    of that mechanism: `data/dev/` is precisely where a throwaway dev
///    daemon's store BELONGS. Refusing the whole tree refused the one place the
///    machine was reorganised to provide. So this clause refuses the cauldron
///    home and everything in it except `data/<env>/…` for an `<env>` that is
///    not production or the shared sibling — see [`CAULDRON_ENVS_REFUSED`].
///    Production's own store inside that tree stays refused by this clause (it
///    is `data/prod/`), by clause 1 and by clause 5: three independent locks on
///    the one path that matters.
/// 4. **Anywhere inside a cargo source tree.** This is the structural rule and
///    its rationale is permanent: a worktree is cut from a commit, so state kept
///    here is work waiting to be lost. It is kept exactly as it was. What has
///    changed is only its SECONDARY justification — until 2026-10-07 this clause
///    also happened to catch the real production store, because that store was
///    the repo's own `lib/threads/` crate directory. It no longer is, and this
///    clause no longer carries production; clause 1 does. The structural rule
///    still stands on its own, and the rollback copy left in `lib/threads/`
///    still makes it true of this repo today.
/// 5. **A directory that already holds a `threads.db`.** Belt to the marker's
///    braces, and it is what produces a legible message when the first four miss.
pub fn production_objection(dir: &Path) -> Option<String> {
    let dir = normalize(dir);

    // 1. The production pointer, as this machine actually sets it.
    if let Some(h) = std::env::var_os("THREADS_HOME") {
        if !h.is_empty() {
            let prod = normalize(Path::new(&h));
            if within(&dir, &prod) {
                return Some(format!(
                    "{} is $THREADS_HOME ({}) or inside it. That variable is how the launchd \
                     supervisor points the LIVE daemon at the production store; a dev daemon \
                     does not follow it. Unset it, or pass --home <somewhere else>.",
                    dir.display(),
                    prod.display()
                ));
            }
        }
    }

    // 2. The crate's own built-in production state home.
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            let dot_threads = normalize(&PathBuf::from(&home).join(".threads"));
            if within(&dir, &dot_threads) {
                return Some(format!(
                    "{} is the production state home ({}) or inside it — that is where a \
                     default-config `threads` daemon keeps its store.",
                    dir.display(),
                    dot_threads.display()
                ));
            }
        }
    }

    // 3. The chassis consumer ledger's home — except a non-production
    //    environment's own data root, which is what that tree exists to hand out.
    if let Some(cauldron) = crate::consumers::cauldron_home() {
        let cauldron = normalize(&cauldron);
        if within(&dir, &cauldron) {
            match cauldron_environment(&dir, &cauldron) {
                Some(env) if !CAULDRON_ENVS_REFUSED.contains(&env.as_str()) => {
                    // `<cauldron>/data/<env>/…` for a usable `<env>`. Admitted by
                    // THIS clause; the marker lock in `claim_dev_dir` still applies,
                    // so a populated directory in there is still not the dev
                    // daemon's to open.
                }
                Some(env) => {
                    return Some(format!(
                        "{} is inside the cauldron `{}` environment ({}/data/{}). `{}` is \
                         not a usable environment for a throwaway dev daemon: {}. Use \
                         another environment name — {}/data/dev is the one this tool is \
                         for.",
                        dir.display(),
                        env,
                        cauldron.display(),
                        env,
                        env,
                        if env == "prod" {
                            "it is PRODUCTION, and since 2026-10-07 it is where the \
                             operator's real 3.8 GB store, his real logins and his real \
                             threads actually live"
                        } else {
                            "it is the reserved non-environment sibling that holds what is \
                             deliberately shared across every environment — model weights, \
                             builds and the build cache"
                        },
                        cauldron.display()
                    ));
                }
                None => {
                    return Some(format!(
                        "{} is the cauldron home ({}) or inside it, but not inside any \
                         environment's data root. That directory holds the chassis consumer \
                         ledger shared by every producer on this machine and is not any one \
                         daemon's to write synthetic data into. A dev daemon's store goes in \
                         an environment: {}/data/dev.",
                        dir.display(),
                        cauldron.display(),
                        cauldron.display()
                    ));
                }
            }
        }
    }

    // 4. THE STRUCTURAL RULE. A data dir inside a source tree is a defect even
    //    when it is not production — and on this machine it IS production.
    let mut probe: Option<&Path> = Some(dir.as_path());
    while let Some(p) = probe {
        if p.join("Cargo.toml").is_file() {
            return Some(format!(
                "{} is inside a cargo source tree ({} holds a Cargo.toml). A daemon's data \
                 dir never belongs in source: a git worktree is cut from a commit, so state \
                 kept here is work waiting to be lost. (Until 2026-10-07 this was also \
                 where the live store itself lived, by way of $THREADS_HOME; it is \
                 ~/.cauldron/data/prod/threads/store now, and the rollback copy left in \
                 lib/threads/ is still refused here.)",
                dir.display(),
                p.display()
            ));
        }
        probe = p.parent();
    }

    // 5. Something already keeps a thread store here.
    if dir.join("threads.db").exists() && !dir.join(DEV_MARKER).exists() {
        return Some(format!(
            "{} already holds a threads.db that this dev daemon did not create (no {} \
             marker beside it). Refusing to open somebody else's thread store.",
            dir.display(),
            DEV_MARKER
        ));
    }

    None
}

/// **CLAIM `dir` AS A THROWAWAY DEV DATA DIR, OR REFUSE.** Both locks, in order.
///
/// On success the directory exists and carries the [`DEV_MARKER`]. Idempotent:
/// claiming a directory this already claimed is a no-op, which is what makes
/// running the dev daemon twice safe.
///
/// Lock one is [`production_objection`] and is UNCONDITIONAL — a marker cannot
/// buy a way past it, which is the point of checking it first. Lock two is the
/// marker: a directory that exists, is not empty, and carries no marker is
/// refused, because the dev daemon did not make it.
pub fn claim_dev_dir(dir: &Path) -> Result<()> {
    if let Some(why) = production_objection(dir) {
        return Err(Error::Config(format!(
            "REFUSING TO START: {why}\n\nThe dev daemon only ever opens a store it created \
             itself. Nothing was read and nothing was written."
        )));
    }

    let marker = dir.join(DEV_MARKER);
    if marker.exists() {
        return Ok(());
    }

    if dir.exists() {
        let mut entries = std::fs::read_dir(dir)
            .map_err(|e| Error::Config(format!("reading {}: {e}", dir.display())))?;
        if entries.next().is_some() {
            return Err(Error::Config(format!(
                "REFUSING TO START: {} already exists, is not empty, and carries no {} \
                 marker. The dev daemon only opens a directory it created itself, so that \
                 pointing it at somebody else's state cannot work by accident. Delete the \
                 directory, or pass --home <somewhere else>.",
                dir.display(),
                DEV_MARKER
            )));
        }
    } else {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::Config(format!("creating {}: {e}", dir.display())))?;
    }

    std::fs::write(&marker, DEV_MARKER_TEXT)
        .map_err(|e| Error::Config(format!("writing {}: {e}", marker.display())))?;
    Ok(())
}

/// **TEAR THE DEV DAEMON'S STATE DOWN — one call, and it cannot reach
/// production.**
///
/// It re-runs the SAME two locks `claim_dev_dir` runs before deleting anything,
/// so `--teardown` pointed at the production store refuses for the identical
/// reason starting would. A teardown that trusted its argument would be a
/// `rm -rf` with the guard on the wrong side of the door.
///
/// `Ok(false)` means there was nothing there.
pub fn teardown_dev_dir(dir: &Path) -> Result<bool> {
    if let Some(why) = production_objection(dir) {
        return Err(Error::Config(format!(
            "REFUSING TO DELETE: {why}\n\nNothing was removed."
        )));
    }
    if !dir.exists() {
        return Ok(false);
    }
    if !dir.join(DEV_MARKER).exists() {
        return Err(Error::Config(format!(
            "REFUSING TO DELETE: {} carries no {} marker, so it was not created by the dev \
             daemon. Nothing was removed.",
            dir.display(),
            DEV_MARKER
        )));
    }
    std::fs::remove_dir_all(dir)
        .map_err(|e| Error::Config(format!("removing {}: {e}", dir.display())))?;
    Ok(true)
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. The config
// ─────────────────────────────────────────────────────────────────────────────

/// The healthy, placeable synthetic account.
pub const ACCOUNT_HEALTHY: &str = "dev-healthy";
/// The synthetic account sitting inside a rate-limit window.
pub const ACCOUNT_LIMITED: &str = "dev-ratelimited";
/// The synthetic account that is both DISABLED (the operator took it out of the
/// pool) and DEAD (its login expired). Two different mechanisms —
/// `account_disabled` row presence and `accounts.dead_since` — and seeding both
/// on one slot means whichever one a diagnosis is looking at, it finds it.
pub const ACCOUNT_DEAD: &str = "dev-dead-login";

/// Every synthetic account, in the order they are seeded.
pub const ACCOUNTS: [&str; 3] = [ACCOUNT_HEALTHY, ACCOUNT_LIMITED, ACCOUNT_DEAD];

/// The dev daemon's [`Config`]: dev ports, the dev data dir, and every periodic
/// `claude` invocation switched off.
///
/// **The gauge legs are all zero and `calibrate`/`edge_poll` are false.** Those
/// legs shell out to `claude -p "/usage"` on a cadence. Against synthetic config
/// dirs that is pure noise, and against a real one it would be unrequested
/// traffic on the operator's logins from a daemon he started to look at a bug.
pub fn dev_config(data_dir: &Path) -> Config {
    let mut accounts = BTreeMap::new();
    for name in ACCOUNTS {
        // A real directory per account, inside the dev dir: `pool::Pool`'s
        // derivation drops any registry row whose `config_dir` is empty (that is
        // how the machine default is spelled, and it is unplaceable), so an
        // account with no dir would never reach the pool at all.
        accounts.insert(
            name.to_string(),
            data_dir
                .join("accounts")
                .join(name)
                .to_string_lossy()
                .to_string(),
        );
    }
    Config {
        chassis_port: DEV_CHASSIS_PORT,
        host: "127.0.0.1".to_string(),
        data_dir: Some(data_dir.to_string_lossy().to_string()),
        accounts,
        // No periodic `claude` process spawns, on any leg.
        probe_interval_secs: 0,
        probe_interval_busy_secs: 0,
        calibrate: false,
        edge_poll: false,
        ..Config::default()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. THE SEED — a function per scenario
// ─────────────────────────────────────────────────────────────────────────────

/// How much synthetic data to make.
///
/// Enough to reproduce the bugs, not a clone of production: the default is 11
/// threads and a couple of dozen events, against a live store with 2.46M ledger
/// rows. Every count is a field so a scenario can be widened without editing the
/// seeding code — `Scale::times(20)` is the whole knob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scale {
    /// Threads parked on the unavailable account.
    pub parked: usize,
    /// Threads with a waiting queue sitting behind a held lock.
    pub with_queue: usize,
    /// Threads that have never been spoken in at all.
    pub silent: usize,
    /// Plain idle threads with a little history.
    pub idle: usize,
    /// Waiting queue rows per queue-bearing thread.
    pub waiting_per_thread: usize,
    /// Failed queue rows per queue-bearing thread.
    pub failed_per_thread: usize,
    /// User messages on a thread that has been spoken in.
    pub messages_per_thread: usize,
}

impl Default for Scale {
    /// The bug-reproducing minimum. Note `parked: 3` and `with_queue: 2`: a
    /// mechanism that arbitrates — placement across accounts, a queue, a lock —
    /// is not exercised by its empty case or its single case, so no count that
    /// matters is 1.
    fn default() -> Self {
        Self {
            parked: 3,
            with_queue: 2,
            silent: 2,
            idle: 4,
            waiting_per_thread: 3,
            failed_per_thread: 2,
            messages_per_thread: 4,
        }
    }
}

impl Scale {
    /// Multiply every count by `n` (clamped at 1). The scale parameter.
    pub fn times(n: usize) -> Self {
        let n = n.max(1);
        let d = Self::default();
        Self {
            parked: d.parked * n,
            with_queue: d.with_queue * n,
            silent: d.silent * n,
            idle: d.idle * n,
            waiting_per_thread: d.waiting_per_thread * n,
            failed_per_thread: d.failed_per_thread * n,
            messages_per_thread: d.messages_per_thread * n,
        }
    }

    /// Total threads this scale creates.
    pub fn threads(&self) -> usize {
        self.parked + self.with_queue + self.silent + self.idle
    }
}

/// What one seeding run did. Returned rather than logged so the binary can print
/// it and a test can assert on it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeedReport {
    pub accounts: usize,
    pub threads_created: usize,
    pub threads_already_present: usize,
    pub events: usize,
    pub queue_waiting: usize,
    pub queue_failed: usize,
    pub parked: usize,
    pub retrying: usize,
    pub locked: usize,
}

/// Every synthetic thread id starts with this, so a human reading `threads/list`
/// — or a stray row in a store — can tell at a glance that nothing here is real.
pub const THREAD_PREFIX: &str = "dev-";

/// The id of the nth thread in a scenario. DETERMINISTIC, which is the whole of
/// how re-seeding stays idempotent: the second run upserts the same rows rather
/// than minting a parallel set beside them.
fn thread_id(scenario: &str, n: usize) -> String {
    format!("{THREAD_PREFIX}{scenario}-{n:02}")
}

/// **SEED EVERYTHING.** The one entry point; the per-scenario functions below are
/// public so a scenario can be re-run or widened on its own.
///
/// Idempotent by construction — run it twice and the second run reports
/// `threads_already_present` and adds nothing. See [`ensure_queue`] for the one
/// place that needed real work to get there.
pub fn seed(store: &Store, scale: Scale) -> Result<SeedReport> {
    let mut r = SeedReport::default();
    seed_accounts(store, &mut r)?;
    seed_parked_on_unavailable_account(store, scale, &mut r)?;
    seed_queue_behind_a_held_lock(store, scale, &mut r)?;
    seed_threads_never_spoken_in(store, scale, &mut r)?;
    seed_idle_threads(store, scale, &mut r)?;
    if let Some(n) = std::env::var("THREADS_DEV_BIG_EVENTS").ok().and_then(|v| v.parse().ok()) {
        seed_big_thread(store, n, &mut r)?;
    }
    Ok(r)
}

/// **SCENARIO: ONE LONG THREAD** (`THREADS_DEV_BIG_EVENTS=<n>`), for measuring
/// what opening a thread costs (`docs/THREAD-LOAD-LATENCY.md`). Every fifth
/// reply is over the 1 KiB inline ceiling, so the window carries elided rows
/// the face must fetch through the resource door, as a real thread does.
pub fn seed_big_thread(store: &Store, n: usize, r: &mut SeedReport) -> Result<()> {
    let id = thread_id("big", 0);
    if !ensure_thread(store, &id, ACCOUNT_HEALTHY, r)? {
        return Ok(());
    }
    let mut seq = store.max_seq(&id)?;
    let mut mid = store.max_message_id(&id)?;
    let long = "lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(60);
    for m in 0..n / 2 {
        mid += 1;
        seq += 1;
        append(
            store,
            &id,
            seq,
            Some(mid),
            ThreadEvent::UserMessage {
                text: format!("big fixture: message {m}"),
                intent: SendIntent::Immediate,
                metadata: serde_json::Value::Null,
            },
            r,
        )?;
        seq += 1;
        let text = if m % 5 == 0 {
            format!("reply {m}: {long}")
        } else {
            format!("big fixture: reply {m} with **markdown** and `code`")
        };
        append(
            store,
            &id,
            seq,
            None,
            ThreadEvent::AssistantText { text, parent_tool_use_id: None, api_message_id: None },
            r,
        )?;
    }
    store.update_thread_counters(&id, mid + 1)?;
    Ok(())
}

/// **SCENARIO: three accounts — one healthy, one rate-limited, one
/// disabled-and-dead.**
///
/// The three unavailability mechanisms in this daemon are genuinely different and
/// a diagnosis that conflates them goes wrong:
///
/// * **rate-limited** is `accounts.limit_until` in the future, honoured only
///   while `limit_identity` still matches `identity` (`limit_is_stale`) — so both
///   are written, or the window would be ignored as stale and the account would
///   read healthy.
/// * **dead** is `accounts.dead_since` / `dead_reason`: the login itself expired.
/// * **disabled** is not a column at all — it is the PRESENCE of a row in
///   `account_disabled`, written by the operator taking the slot out of the pool.
///
/// `ACCOUNT_DEAD` carries the last two together, because in practice an expired
/// login is what makes somebody disable a slot.
pub fn seed_accounts(store: &Store, r: &mut SeedReport) -> Result<()> {
    let now = now_rfc3339();

    // The healthy one. An `identity` is written so it is not indistinguishable
    // from an account nothing has ever been observed about.
    store.upsert_account_state(&AccountState {
        name: ACCOUNT_HEALTHY.to_string(),
        identity: Some("dev-identity-healthy".into()),
        updated_at: Some(now.clone()),
        ..AccountState::default()
    })?;

    // Rate-limited: a window two hours out. `limit_identity` MUST equal
    // `identity` or `limit_is_stale()` retires the window and the account reads
    // available again — which is the failure mode this scenario exists to show.
    let until = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
    store.upsert_account_state(&AccountState {
        name: ACCOUNT_LIMITED.to_string(),
        identity: Some("dev-identity-limited".into()),
        updated_at: Some(now.clone()),
        ..AccountState::default()
    })?;
    store.set_account_limit(
        ACCOUNT_LIMITED,
        Some((
            &until,
            Some("Claude usage limit reached. Your limit will reset at 2am (dev fixture)."),
            Some("weekly_all"),
            Some("dev-identity-limited"),
        )),
    )?;

    // Dead AND disabled — two mechanisms, deliberately both.
    store.upsert_account_state(&AccountState {
        name: ACCOUNT_DEAD.to_string(),
        identity: Some("dev-identity-dead".into()),
        updated_at: Some(now.clone()),
        ..AccountState::default()
    })?;
    store.set_account_health(
        ACCOUNT_DEAD,
        Some((&now, "OAuth token expired and could not be refreshed (dev fixture)")),
    )?;
    store.set_account_disabled(
        ACCOUNT_DEAD,
        true,
        Some("dev fixture: taken out of the pool because its login is dead"),
    )?;

    r.accounts = ACCOUNTS.len();
    Ok(())
}

/// **SCENARIO: threads PARKED on an unavailable account.**
///
/// The durable artifact is a `parked_turns` row, and it is the only kind of park
/// that survives a restart — the admission gate's park is an in-memory latch on
/// `ThreadState`, so a fresh daemon reading a seeded store would see nothing at
/// all if this wrote only the latch's shape.
///
/// Each thread also gets the `turn_parked` ledger event a real park emits, with
/// `cause: "unavailable"` — the string [`crate::budgets::ParkCause::Unavailable`]
/// produces — so the history reads the way a real one does and not merely the
/// status.
///
/// The prompt is held verbatim, which is CON-4: a park never drops what the
/// operator said.
pub fn seed_parked_on_unavailable_account(
    store: &Store,
    scale: Scale,
    r: &mut SeedReport,
) -> Result<()> {
    for n in 0..scale.parked {
        let id = thread_id("parked", n);
        let fresh = ensure_thread(store, &id, ACCOUNT_LIMITED, r)?;
        let mut seq = store.max_seq(&id)?;
        let mut mid = store.max_message_id(&id)?;

        if fresh {
            mid += 1;
            seq += 1;
            append(
                store,
                &id,
                seq,
                Some(mid),
                ThreadEvent::UserMessage {
                    text: format!("dev fixture: parked message {n}, held and never delivered"),
                    intent: SendIntent::Immediate,
                    metadata: serde_json::Value::Null,
                },
                r,
            )?;
            seq += 1;
            append(
                store,
                &id,
                seq,
                None,
                ThreadEvent::TurnParked {
                    cause: "unavailable".into(),
                    reason: format!(
                        "{ACCOUNT_LIMITED} is rate-limited and this thread is pinned to it"
                    ),
                    budget_key: Some(crate::budgets::DEFAULT_BUDGET_KEY.into()),
                    message_ids: vec![mid],
                },
                r,
            )?;
            store.update_thread_counters(&id, mid + 1)?;
        }

        store.upsert_parked_turn(&ParkedTurnRow {
            thread_id: id.clone(),
            prompt: format!("dev fixture: parked message {n}, held and never delivered"),
            message_ids: vec![mid.max(1)],
            attempt: 1,
            reason: format!("{ACCOUNT_LIMITED} is rate-limited and this thread is pinned to it"),
            budget_key: Some(crate::budgets::DEFAULT_BUDGET_KEY.into()),
            parked_at: now_rfc3339(),
        })?;
        r.parked += 1;

        // ── THE RETRY FLOOR, AND IT IS NOT DECORATION ────────────────────
        //
        // **MEASURED, NOT ASSUMED.** The first run of this fixture without these
        // rows did the following at boot, in the daemon's own log:
        //
        // ```
        // WARN RUNNING ANYWAY: this thread is ungated and the gate had nowhere
        //      better to put it thread=dev-parked-01 phase=unavailable
        // INFO a turn COMPLETED on this login, so the recorded usage-limit window
        //      was wrong — cleared on every label of it  account=dev-ratelimited
        // ```
        //
        // `recover()` → `resume_sweep()` calls `maybe_dispatch` on every thread
        // with a `parked_turns` row; `admit_or_park` ADMITTED, because an ungated
        // thread runs anyway when the gate has nowhere better to put it; the
        // fake-claude turn completed; and `note_account_served` then cleared
        // `accounts.limit_until` as a window that must have been wrong. The
        // fixture destroyed two of its own scenarios — the park AND the rate
        // limit — inside 200 ms of boot, before anybody could connect.
        //
        // A `retries` row is the fix, and it is the right one rather than a
        // convenient one. `maybe_dispatch`'s FIRST store read is
        // `if matches!(self.store.get_retry(&state.id), Ok(Some(_))) { return; }`
        // — ahead of the parked branch — so a thread holding a parked prompt is
        // never dispatched and that prompt is never cleared. And it is what the
        // real thing looks like: a thread parked on a rate-limited login is a
        // thread with a prompt waiting in `retries` and an account the gate will
        // refuse. Nothing fires during a session because the ACCOUNT is barred for
        // two hours, which is the fixture's real mechanism — the row itself carries
        // no instant to wait on, by design (see `store::DROPPED_COLUMNS`).
        //
        // `thread_states`' precedence is executing → parked → retrying → queued →
        // idle, so these threads still read `parked`, which is the state the
        // scenario is named for.
        store.upsert_retry(&RetryRow {
            thread_id: id.clone(),
            prompt: format!("dev fixture: parked message {n}, held and never delivered"),
            message_ids: vec![mid.max(1)],
            attempt: 2,
            kind: "rate_limit".into(),
            reason: format!("{ACCOUNT_LIMITED} answered 429"),
            account: Some(ACCOUNT_LIMITED.to_string()),
            created_at: String::new(),
        })?;
        r.retrying += 1;
    }
    Ok(())
}

/// **SCENARIO: a queue with items WAITING and items FAILED, behind a held
/// lock.**
///
/// Two things here needed thought, and both are findings about the daemon rather
/// than choices about the fixture.
///
/// **Why the lock.** `run_daemon` → `recover()` → `resume_sweep()` calls
/// `maybe_dispatch` on every thread with a waiting queue. Seeded waiting rows on
/// a dispatchable thread would therefore be CONSUMED during boot — the fixture
/// would destroy itself before anyone could look at it. A `thread_locks` row held
/// by a *different* daemon (a foreign `boot_id`, and `daemon_pid: 1`, which is
/// launchd and is always alive) makes `lock_turn` refuse, so the rows stay
/// waiting. `recover`'s own lock sweep leaves a live holder strictly alone and
/// says so at WARN; that line is expected in the dev daemon's log.
///
/// **Why `failed` has to be reached through `delivered`.**
/// `Store::fail_deliveries` is guarded on `state='delivered'`, so a row cannot be
/// failed from `waiting`. The seed walks the real transitions —
/// enqueue → `mark_delivered` → `fail_deliveries` — rather than writing the
/// terminal state directly, which means the fixture exercises the same
/// state machine the daemon does.
///
/// The derived state of these threads is `active` with a non-zero `queued` count,
/// which is what a real busy thread with work behind it looks like. See the
/// module-level note in the report about why a bare `queued` state word is
/// transient by construction.
pub fn seed_queue_behind_a_held_lock(
    store: &Store,
    scale: Scale,
    r: &mut SeedReport,
) -> Result<()> {
    for n in 0..scale.with_queue {
        let id = thread_id("queued", n);
        ensure_thread(store, &id, ACCOUNT_HEALTHY, r)?;

        // The foreign lock. `boot_id` is a literal and emphatically not a uuid
        // this process could ever mint, so `lock_is_live` takes the
        // other-daemon arm and judges it by `pid_alive(1)`.
        match store.try_claim_thread(&id, "dev-fixture-foreign-boot", 1, "dispatch") {
            Ok(_) => r.locked += 1,
            Err(e) => {
                tracing::warn!(thread = %id, error = %e, "could not seed the held lock");
            }
        }

        ensure_queue(
            store,
            &id,
            scale.waiting_per_thread,
            scale.failed_per_thread,
            r,
        )?;
    }
    Ok(())
}

/// **SCENARIO: threads with NO USER MESSAGES AT ALL.**
///
/// This is the one the operator called out, and it is the subtlest. HEAD changed
/// `threads/recent-updates`: the index's order comes from
/// `threads.last_user_input_at`, which `Store::bump_last_seq` only ever sets when
/// the appended event was a `user_message`. A thread that has never been spoken
/// in has NULL there, `Store::recent_threads` filters it out, and the shape
/// cannot mint a position for it — so the thread is **absent from the index
/// entirely**, not merely last in it.
///
/// So these threads get a `thread_started` and an `assistant_text` and no
/// `UserMessage`, and `bump_last_seq` is called with `user_input_at: None`. The
/// assistant line matters: without it the thread would have no events at all and
/// a reader could not tell "never spoken in" from "empty".
pub fn seed_threads_never_spoken_in(store: &Store, scale: Scale, r: &mut SeedReport) -> Result<()> {
    for n in 0..scale.silent {
        let id = thread_id("silent", n);
        let fresh = ensure_thread(store, &id, ACCOUNT_HEALTHY, r)?;
        if !fresh {
            continue;
        }
        let mut seq = store.max_seq(&id)?;
        seq += 1;
        append(
            store,
            &id,
            seq,
            None,
            ThreadEvent::AssistantText {
                text: "dev fixture: this thread has events but has NEVER been spoken in, so it \
                       carries no last_user_input_at and is absent from threads/recent-updates"
                    .into(),
                parent_tool_use_id: None,
                api_message_id: None,
            },
            r,
        )?;
    }
    Ok(())
}

/// **SCENARIO: ordinary idle threads with a little history.**
///
/// The control arm. Without threads that are simply fine, every query against the
/// dev daemon returns a pathology and there is nothing to compare against — and
/// `threads/recent-updates` needs threads that DO have a position before "absent
/// from the index" means anything.
pub fn seed_idle_threads(store: &Store, scale: Scale, r: &mut SeedReport) -> Result<()> {
    for n in 0..scale.idle {
        let id = thread_id("idle", n);
        // Spread them across the healthy and the limited account so placement has
        // something to reason about. Not the dead one: it is disabled, and a
        // thread there would be parked rather than idle.
        let account = if n % 2 == 0 {
            ACCOUNT_HEALTHY
        } else {
            ACCOUNT_LIMITED
        };
        let fresh = ensure_thread(store, &id, account, r)?;
        if !fresh {
            continue;
        }
        let mut seq = store.max_seq(&id)?;
        let mut mid = store.max_message_id(&id)?;
        for m in 0..scale.messages_per_thread {
            mid += 1;
            seq += 1;
            append(
                store,
                &id,
                seq,
                Some(mid),
                ThreadEvent::UserMessage {
                    text: format!("dev fixture: message {m} on {id}"),
                    intent: SendIntent::Immediate,
                    metadata: serde_json::Value::Null,
                },
                r,
            )?;
            seq += 1;
            append(
                store,
                &id,
                seq,
                None,
                ThreadEvent::AssistantText {
                    text: format!("dev fixture: reply {m} on {id}"),
                    parent_tool_use_id: None,
                    api_message_id: None,
                },
                r,
            )?;
        }
        store.update_thread_counters(&id, mid + 1)?;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. The small primitives the scenarios share
// ─────────────────────────────────────────────────────────────────────────────

/// Create the thread row if it is not already there. `true` when it was created
/// by this call — which is what every scenario keys its event-writing off, so
/// re-seeding does not pile a second copy of the history onto an existing thread.
///
/// `Store::upsert_thread` and not `ThreadManager::create_thread`: the latter
/// ERRORS on an id that already exists, which would make the second run of a
/// seed fail rather than be a no-op.
fn ensure_thread(store: &Store, id: &str, account: &str, r: &mut SeedReport) -> Result<bool> {
    if store.get_thread(id)?.is_some() {
        r.threads_already_present += 1;
        return Ok(false);
    }
    let now = now_rfc3339();
    let mut settings = ThreadSettings::new("/tmp", "sonnet");
    // PINNED, so the account in the fixture is the account the daemon reasons
    // about. `AccountChoice::Auto` would re-resolve to whatever is selectable,
    // which for the limited and dead accounts is the opposite of the point.
    settings.account = AccountChoice::Named(account.to_string());
    // No injected preamble: the seeded ledger should read as what was written,
    // not as what a prompt builder would have added to it.
    settings.injection = crate::settings::Injection::NOTHING;
    let settings_json = serde_json::to_string(&settings)
        .map_err(|e| Error::Config(format!("serialising dev thread settings: {e}")))?;

    store.upsert_thread(&ThreadRow {
        id: id.to_string(),
        account: Some(account.to_string()),
        cwd: Some("/tmp".into()),
        model: Some("sonnet".into()),
        settings_json,
        last_seq: 0,
        next_message_id: 1,
        last_compaction_message_id: 0,
        needs_restate: false,
        created_at: now.clone(),
        updated_at: now,
    })?;
    // The conversation-history row: this is what "the thread has an established
    // conversation on this login" means, and the admission gate's continuity park
    // reads it. A thread with no history row is a different scenario.
    store.record_thread_account(id, account)?;

    // `seq` 1: the first event on any thread stream, as `ThreadEvent::ThreadStarted`
    // documents. Every scenario resumes from `store.max_seq` after this, so there
    // is no shared counter to keep in step.
    append(
        store,
        id,
        1,
        None,
        ThreadEvent::ThreadStarted {
            thread_id: id.to_string(),
            account: account.to_string(),
            cwd: "/tmp".into(),
            model: "sonnet".into(),
        },
        r,
    )?;
    r.threads_created += 1;
    Ok(true)
}

/// Append one event AND advance `threads.last_seq`.
///
/// **The second half is the easy thing to forget.** `Store::append_event` writes
/// the `events` row and nothing else; `last_seq` and `last_user_input_at` are
/// moved by `bump_last_seq`, which the daemon's own private `emit_resolving`
/// calls right after. Skip it and `recent_threads`, the unread counts and the
/// hydrate bound are all wrong — the fixture would be lying in exactly the area
/// the `recent-updates` bug lives in.
///
/// `user_input_at` is `Some(ts)` for a user message and `None` otherwise, which
/// is `ThreadEvent::is_user_message()`'s one job and is the whole mechanism
/// behind a never-spoken-in thread being absent from the index.
fn append(
    store: &Store,
    id: &str,
    seq: u64,
    message_id: Option<u64>,
    event: ThreadEvent,
    r: &mut SeedReport,
) -> Result<()> {
    let ts = now_rfc3339();
    let is_user = event.is_user_message();
    store.append_event(&StoredEvent {
        thread_id: id.to_string(),
        seq,
        message_id,
        ts: ts.clone(),
        event,
    })?;
    store.bump_last_seq(id, seq, if is_user { Some(&ts) } else { None })?;
    r.events += 1;
    Ok(())
}

/// Bring a thread's queue up to `waiting` waiting rows and `failed` failed rows,
/// adding only the shortfall.
///
/// **This is the one place idempotency took real work.** `Store::enqueue` is a
/// plain INSERT against an AUTOINCREMENT id, so it is the single non-idempotent
/// write in the seeding path: calling it again would stack a second set of rows
/// beside the first and "running it twice must not accumulate garbage" would be
/// false. Counting first and topping up makes re-seeding a no-op, and makes
/// RAISING the scale do the obvious thing instead of doubling.
///
/// A `failed` row is reached the way the daemon reaches it —
/// enqueue → `mark_delivered` → `fail_deliveries` — because
/// `Store::fail_deliveries` is guarded on `state='delivered'` and will not move a
/// `waiting` row.
pub fn ensure_queue(
    store: &Store,
    id: &str,
    waiting: usize,
    failed: usize,
    r: &mut SeedReport,
) -> Result<()> {
    let have_waiting = store.waiting_queue(id)?.len();
    let have_failed = store.failed_deliveries(id)?.len();
    let mut mid = store.max_message_id(id)?;
    let mut seq = store.max_seq(id)?;

    // The FAILED ones first, so they are the older rows — a failure is something
    // that already happened, and the waiting rows are what is still owed.
    for n in have_failed..failed {
        mid += 1;
        seq += 1;
        append(
            store,
            id,
            seq,
            Some(mid),
            ThreadEvent::UserMessage {
                text: format!("dev fixture: FAILED delivery {n} on {id}"),
                intent: SendIntent::Immediate,
                metadata: serde_json::Value::Null,
            },
            r,
        )?;
        let row = store.enqueue(
            id,
            mid,
            &format!("dev fixture: FAILED delivery {n} on {id}"),
            "immediate",
            &serde_json::Value::Null,
            &[],
        )?;
        store.mark_delivered(&[row])?;
        store.fail_deliveries(id, &[mid])?;
        r.queue_failed += 1;
    }

    for n in have_waiting..waiting {
        mid += 1;
        seq += 1;
        append(
            store,
            id,
            seq,
            Some(mid),
            ThreadEvent::UserMessage {
                text: format!("dev fixture: WAITING delivery {n} on {id}"),
                intent: SendIntent::Immediate,
                metadata: serde_json::Value::Null,
            },
            r,
        )?;
        store.enqueue(
            id,
            mid,
            &format!("dev fixture: WAITING delivery {n} on {id}"),
            "immediate",
            &serde_json::Value::Null,
            &[],
        )?;
        r.queue_waiting += 1;
    }

    store.update_thread_counters(id, mid + 1)?;
    Ok(())
}
