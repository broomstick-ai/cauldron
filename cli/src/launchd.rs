//! **The bottom turtle: one launchd job per environment.**
//!
//! The operator's ruling on why it is per environment, verbatim: *"I was
//! thinking one per environment. cauldron sets it per-environment based on the
//! config. Maybe the envs can have their one process name, e.g. 'cauldron' for
//! prod or 'cauldron-<env>' for other envs. **That way each has its own signing
//! identity and I can selectively approve permissions per env.**"*
//!
//! That last sentence is the requirement and it is not an aesthetic one. macOS
//! grants permissions — keychain items, microphone, automation — against a
//! code identity, and the prompt names whatever binary happened to ask. He
//! denied a batch of keychain prompts earlier in this arc for exactly that
//! reason: the prompt did not tell him which thing was asking. One file per
//! environment, with its own name, means one signing identity per environment,
//! which means a prompt he can answer and an approval that does not silently
//! cover a test environment as well as production.
//!
//! So the install is not "write a plist". It is:
//!
//! ```text
//!   data/shared/builds/cauldron/<label>/cauldron   the version
//!            |                                     copy, atomic rename
//!            v
//!   ~/.cauldron/bin/cauldron        (prod)   <-- the LAUNCH PATH, a distinct
//!   ~/.cauldron/bin/cauldron-dev    (dev)        file with a distinct NAME
//!            |
//!            v
//!   ai.broomstick.cauldron         /  ai.broomstick.cauldron-dev
//!   (prefix: `[supervisor] label_prefix`, else an existing `com.broomstick.*`
//!    job keeps its name, else `ai.broomstick` — see `label_prefix`)
//! ```
//!
//! The name in `ps`, the name in a permission prompt and the name
//! `codesign` signs are all the launch path's file name, which is why the copy
//! is renamed rather than symlinked. DEC-3's no-symlink rule applies and has a
//! measured reason behind it: `current_exe()` on macOS returns a symlink
//! UNRESOLVED, and at least one daemon on this machine gates its update
//! protocol on the file's NAME, so a link named anything else silently disarms
//! it.
//!
//! # What the plist must do, and what it must not know
//!
//! Three things, and it says nothing else:
//!
//! 1. start one process at login (`RunAtLoad`);
//! 2. restart it whenever it exits, for any reason (`KeepAlive = true`,
//!    **unconditional**);
//! 3. guarantee one instance per label, which launchd does by construction.
//!
//! What it must NOT know: any service name, port, binary, health URL or data
//! path. One environment name and one program, and everything else is in
//! `env.toml` where the operator can read it.
//!
//! # Why `KeepAlive = true` and not `SuccessfulExit = false`
//!
//! Every existing `com.harness.*` job on this machine uses
//! `KeepAlive.SuccessfulExit = false`, and its own comment explains why: the
//! old supervisor had three honest reasons to exit 0 — a deliberate stop, a
//! lost bind race, and finding a duplicate — and restarting it in those cases
//! was how you got two supervisors.
//!
//! **All three are gone.** There is no second launcher to lose a bind race to;
//! a deliberate stop is a state the daemon holds rather than its exit; and a
//! duplicate is refused by a kernel lock with a NON-ZERO exit. So the exit code
//! of `cauldron daemon` carries no information launchd should act on, and
//! unconditional `KeepAlive` is what makes a dead holder a ten-second
//! condition instead of a sixteen-day one.
//!
//! That is the direct fix for 2026-09-22. `SuccessfulExit = false` was not the
//! cause of that failure — a stale pidfile and a reused pid were — but it was
//! what let the bug's clean exit *stick*. Both halves are removed: the hold
//! cannot answer wrongly, and the exit code cannot make launchd believe it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use env::decl::Manifest;
use env::{guard, Environment};

/// **The neutral default reverse-DNS prefix for a cauldron job** (REL-4.S3).
/// `broomstick-ai` is the organisation the product is published under, so a
/// stranger's install carries the product's namespace and nobody's personal
/// one. Overridden per environment by `[supervisor] label_prefix` in
/// `env.toml`; see [`label_prefix`] for the full resolution order.
pub const DEFAULT_LABEL_PREFIX: &str = "ai.broomstick";

/// **The prefix every cauldron job was installed under before REL-4.S3.**
/// The operator's running jobs carry it (`com.broomstick.cauldron`). It is
/// never chosen for a NEW install; it is kept only for an environment whose
/// job already exists under it, so that upgrading the binary never renames a
/// running job (a renamed job is a SECOND job holding the same environment).
pub const LEGACY_LABEL_PREFIX: &str = "com.broomstick";

/// The environment whose process is called plainly `cauldron`.
///
/// `prod` is not special anywhere else in this crate — the production guard is
/// structural, about a path on disk, not nominal — and it is not special here
/// either. This is a NAMING convention he chose, so that the thing he sees in
/// a permission prompt on the machine he actually uses is called `cauldron`.
pub const PLAIN_ENV: &str = "prod";

/// **`cauldron` in prod, `cauldron-<env>` everywhere else.**
///
/// One function, so the launch path, the plist label, the `ps` name and the
/// hold's advisory note cannot disagree about what this process is called.
pub fn process_name(env_name: &str) -> String {
    if env_name == PLAIN_ENV {
        "cauldron".to_string()
    } else {
        format!("cauldron-{env_name}")
    }
}

/// `<prefix>.cauldron` / `<prefix>.cauldron-<env>`. Pure: the prefix is the
/// caller's, already resolved.
pub fn label_with(prefix: &str, env_name: &str) -> String {
    format!("{prefix}.{}", process_name(env_name))
}

/// **Which prefix a job gets, as a pure decision** (REL-4.S3), so it is
/// unit-testable without a HOME or an `env.toml`:
///
/// 1. `configured` — `[supervisor] label_prefix` — wins whenever it is set.
///    That is how the operator's machine pins `com.broomstick`.
/// 2. Otherwise, if a job for this environment is already installed under
///    [`LEGACY_LABEL_PREFIX`], keep it: an upgrade must not rename a running
///    job.
/// 3. Otherwise [`DEFAULT_LABEL_PREFIX`].
pub fn resolve_label_prefix(configured: Option<&str>, legacy_installed: bool) -> String {
    match configured {
        Some(p) => p.to_string(),
        None if legacy_installed => LEGACY_LABEL_PREFIX.to_string(),
        None => DEFAULT_LABEL_PREFIX.to_string(),
    }
}

/// `$HOME/Library/LaunchAgents`, or `None` when `$HOME` is unset.
pub fn launch_agents_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(Path::new(&home).join("Library").join("LaunchAgents"))
}

/// **This environment's label prefix**, resolved by [`resolve_label_prefix`]
/// from its `env.toml` and from what is already in `~/Library/LaunchAgents`.
/// An unreadable `env.toml` counts as "not configured" here; `install` loads
/// it strictly and refuses before it ever writes a label.
pub fn label_prefix(env: &Environment) -> String {
    label_prefix_in(env, launch_agents_dir().as_deref())
}

/// [`label_prefix`] against an explicit LaunchAgents dir (`None`: no HOME),
/// so the legacy arm is testable without touching the real one.
pub fn label_prefix_in(env: &Environment, agents: Option<&Path>) -> String {
    let configured = Manifest::load(&env.manifest()).ok().and_then(|m| m.supervisor.label_prefix);
    let legacy_installed = agents
        .map(|d| d.join(format!("{}.plist", label_with(LEGACY_LABEL_PREFIX, &env.name))).exists())
        .unwrap_or(false);
    resolve_label_prefix(configured.as_deref(), legacy_installed)
}

/// **REL-4.S3b: write the resolved prefix into `env.toml` when none is set
/// there**, so the job's name stops depending on its plist continuing to
/// exist. Without this, the legacy arm of [`resolve_label_prefix`] is
/// evidence that `cauldron uninstall` deletes: uninstall, then install, and
/// `com.broomstick.cauldron` silently came back as `ai.broomstick.cauldron`.
///
/// Called by `install` (before it writes the plist) and by `uninstall`
/// (before it deletes one), through the ordinary locked env.toml writer
/// ([`Manifest::update`]). Returns the prefix it recorded, or `None` when
/// `label_prefix` was already set — then nothing is written at all.
pub fn persist_label_prefix_in(env: &Environment, agents: Option<&Path>) -> Result<Option<String>> {
    if Manifest::load(&env.manifest())?.supervisor.label_prefix.is_some() {
        return Ok(None);
    }
    let resolved = label_prefix_in(env, agents);
    Manifest::update(&env.manifest(), |m| {
        // Re-checked under the lock: a concurrent writer that set it wins.
        if m.supervisor.label_prefix.is_some() {
            return Ok(None);
        }
        m.supervisor.label_prefix = Some(resolved.clone());
        Ok(Some(resolved))
    })
}

/// **This environment's launchd label**: `<prefix>.cauldron` in prod,
/// `<prefix>.cauldron-<env>` elsewhere, with the prefix from [`label_prefix`].
pub fn label(env: &Environment) -> String {
    label_with(&label_prefix(env), &env.name)
}

/// **Plists in `agents` that would hold this environment under a label OTHER
/// than `chosen`** — one per known prefix (default and legacy). Two jobs for
/// one environment is a second holder: it loses the kernel lock, exits
/// non-zero, and launchd revives it every `ThrottleInterval` forever. So
/// `install` refuses when this is non-empty.
pub fn conflicting_plists(agents: &Path, env_name: &str, chosen: &str) -> Vec<PathBuf> {
    [DEFAULT_LABEL_PREFIX, LEGACY_LABEL_PREFIX]
        .iter()
        .map(|p| label_with(p, env_name))
        .filter(|l| l != chosen)
        .map(|l| agents.join(format!("{l}.plist")))
        .filter(|p| p.exists())
        .collect()
}

/// `~/.cauldron/bin` — where the launch paths live.
///
/// Under the cauldron root rather than beside the builds, because this is the
/// one directory whose contents are pointed at by something OUTSIDE cauldron
/// (launchd) and so must not move when a version does. A plist naming a
/// version directory would have to be rewritten on every promotion, and a
/// promotion that edits a launchd job is a promotion that can leave the job
/// pointing at a deleted file.
pub fn bin_dir(env: &Environment) -> PathBuf {
    env.root.join("bin")
}

pub fn launch_path(env: &Environment) -> PathBuf {
    bin_dir(env).join(process_name(&env.name))
}

pub fn plist_path(env: &Environment) -> Result<PathBuf> {
    let home = std::env::var("HOME").context(
        "$HOME is not set, so there is no LaunchAgents directory to install into",
    )?;
    Ok(Path::new(&home).join("Library").join("LaunchAgents").join(format!("{}.plist", label(env))))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `<data root>/logs/<process name>.launchd.log`: the file the plist names as
/// the holder's `StandardOutPath` and `StandardErrorPath`.
pub fn launchd_log_path(env: &Environment) -> PathBuf {
    env.data_root.join("logs").join(format!("{}.launchd.log", process_name(&env.name)))
}

/// **DG-2: rotate the launchd log, by rename, past 64 MiB (3 generations).**
/// cauldron never opens this file — launchd does, each time it spawns the job
/// — so no cauldron writer can rotate it at open. It is rotated from two
/// places:
///
/// - [`rotate_launchd_log_at_start`] (DG-2b), the holder's own startup. That
///   is the path every deploy runs: the holder swaps by its self-update exit
///   plus a KeepAlive revive, never by `cauldron install` (DEPLOY-9o/9p), and
///   a crash revive or a login start runs it too.
/// - [`reload_job`], inside `cauldron install`, AFTER the previous job has
///   been booted out and confirmed gone and BEFORE the new one is
///   bootstrapped.
pub fn rotate_launchd_log(env: &Environment) -> bool {
    crate::logrotate::rotate_quietly(&launchd_log_path(env))
}

/// Set by [`rotate_launchd_log_at_start`] on the image it re-execs, naming
/// the generation the old file went to. Read once and removed before the
/// hold, so no service ever inherits it — and its presence is what stops the
/// re-exec'd image from rotating again.
pub const LAUNCHD_LOG_ROTATED: &str = "CAULDRON_LAUNCHD_LOG_ROTATED";

/// **DG-2b — the holder rotates its own launchd log at start, on every spawn.**
///
/// WHO WRITES THE FILE. launchd opens `StandardOutPath`/`StandardErrorPath`
/// when it spawns the job and hands the holder that descriptor as fd 1 and
/// fd 2; the holder is then the file's writer for its whole life (measured on
/// prod 2026-10-27: `lsof` on `cauldron.launchd.log` lists only the holder,
/// pid 17359, `1u` and `2u`; hosted services write their own `<svc>.log` and
/// never inherit it). Whether launchd itself keeps a descriptor cannot be
/// seen from userland (`lsof -p 1` lists nothing without root), and launchd
/// writes nothing to a job's stdout, so the scheme below does not depend on
/// it: rotation is a RENAME, and any descriptor still on the old file keeps
/// writing into `.1`, losing nothing.
///
/// WHEN. Right after launchd has just opened the file for THIS spawn, before
/// the hold and before anything is spawned (single-threaded). If fd 1 or fd 2
/// IS the launchd log (same device and inode — a holder run by hand into a
/// terminal or another file is not this file's writer and rotates nothing)
/// and the file is past [`crate::logrotate::LOG_MAX_BYTES`], it is renamed
/// to `.1` (older generations shifted, at most 3 kept) and this image
/// RE-EXECS ITSELF with whichever of fd 1 / fd 2 were the log pointed at a
/// fresh file at the same path. The re-exec is the only way to move this
/// process's own stdio without `unsafe` (the crate forbids it); after it, no
/// cauldron process writes the rotated file. A failed re-exec is reported and
/// the holder carries on, writing into `.1`.
///
/// The one hole, stated: a duplicate holder started by hand with its stdio
/// redirected INTO the launchd log rotates it before its hold is refused; the
/// live holder then keeps writing, uninterrupted, into `.1`.
pub fn rotate_launchd_log_at_start(env: &Environment) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::process::CommandExt;
    let log = launchd_log_path(env);
    if let Some(gen) = std::env::var_os(LAUNCHD_LOG_ROTATED) {
        println!(
            "cauldron: rotated {} to {} at holder start (past 64 MiB); pid {} writes the fresh file",
            log.display(),
            Path::new(&gen).display(),
            std::process::id()
        );
        // Single-threaded here (before the hold), as RELAUNCHED_FROM's removal.
        std::env::remove_var(LAUNCHD_LOG_ROTATED);
        return Ok(());
    }
    let Ok(at) = std::fs::metadata(&log) else {
        return Ok(());
    };
    // Opening `/dev/fd/N` DUPLICATES descriptor N (it writes nothing), and
    // `metadata()` on the duplicate is an fstat of the file launchd opened. A
    // plain stat of `/dev/fd/N` will not do: on macOS it reports devfs's
    // device number (measured: st_dev 2130658334 against the file's 16777234,
    // same inode), so device + inode would never match.
    let is_log = |fd: u32| {
        std::fs::OpenOptions::new()
            .append(true)
            .open(format!("/dev/fd/{fd}"))
            .and_then(|f| f.metadata())
            .map(|m| m.dev() == at.dev() && m.ino() == at.ino())
            .unwrap_or(false)
    };
    let (out, err) = (is_log(1), is_log(2));
    if !(out || err) || !rotate_launchd_log(env) {
        return Ok(());
    }
    let gen = crate::logrotate::generation_path(&log, 1);
    let fresh = match std::fs::OpenOptions::new().create(true).append(true).open(&log) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "cauldron: rotated {} to {} but could not open a fresh one ({e}); this holder \
                 keeps writing to {}",
                log.display(),
                gen.display(),
                gen.display()
            );
            return Ok(());
        }
    };
    let me = std::env::current_exe().context("resolving this binary's own path")?;
    let mut cmd = std::process::Command::new(&me);
    cmd.args(std::env::args_os().skip(1)).env(LAUNCHD_LOG_ROTATED, &gen);
    if out {
        cmd.stdout(fresh.try_clone()?);
    }
    if err {
        cmd.stderr(fresh);
    }
    let e = cmd.exec();
    eprintln!(
        "cauldron: rotated {} to {} but could not re-exec {} onto the fresh file ({e}); this \
         holder keeps writing to {}",
        log.display(),
        gen.display(),
        me.display(),
        gen.display()
    );
    Ok(())
}

/// The launchctl half of `cauldron install`, as three calls, so the ORDER of
/// bootout → rotate → bootstrap is testable without launchctl (DG-2b).
pub(crate) trait JobControl {
    /// `launchctl print <target>` succeeds.
    fn loaded(&self) -> bool;
    fn bootout(&self);
    fn bootstrap(&self) -> Result<std::process::ExitStatus>;
}

/// **THE ONE MUTATING `launchctl`** (REL-6a5). `bootstrap` and `bootout`
/// act on his real launchd domain whatever `CAULDRON_HOME` says, so an
/// isolated process (`env::egress::process_isolation`) is refused them
/// outright. The read-only `print`/`list` queries are not daemon control.
fn launchctl_control(what: &str) -> Result<std::process::Command> {
    env::egress::admit_daemon_control(&format!("`launchctl {what}`"), None, env::egress::process_isolation().as_deref())
        .map_err(anyhow::Error::msg)?;
    Ok(std::process::Command::new("/bin/launchctl"))
}

struct Launchctl {
    dom: String,
    target: String,
    plist_file: PathBuf,
}

impl JobControl for Launchctl {
    fn loaded(&self) -> bool {
        std::process::Command::new("/bin/launchctl")
            .args(["print", &self.target])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    fn bootout(&self) {
        let Ok(mut c) = launchctl_control("bootout") else { return };
        let _ = c
            .args(["bootout", &self.target])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    fn bootstrap(&self) -> Result<std::process::ExitStatus> {
        launchctl_control("bootstrap")?
            .args(["bootstrap", &self.dom, &self.plist_file.display().to_string()])
            .status()
            .context("running launchctl bootstrap")
    }
}

/// **Bootout, wait for it, rotate the launchd log, bootstrap, then verify.**
/// `verb_install`'s launchd half; `wait` bounds each of the two waits.
///
/// `launchctl bootout` is ASYNCHRONOUS. A `bootstrap` issued immediately
/// after it races the teardown, and the race is not reported: bootstrap
/// exits 0 and the job is not there. Measured on this machine — an install
/// printed "bootstrapped com.broomstick.cauldron into gui/501" and
/// `launchctl list` showed nothing, so the environment was unheld while the
/// install said it had succeeded.
///
/// That is the 2026-09-22 shape of mistake in a new place: a step that
/// reports success without checking. So this waits for the label to GO, and
/// then waits for it to ARRIVE, and the exit code is not the evidence —
/// `launchctl print` is.
pub(crate) fn reload_job(
    env: &Environment,
    jobs: &dyn JobControl,
    dom: &str,
    plist_file: &Path,
    wait: std::time::Duration,
) -> Result<()> {
    let lab = label(env);
    let target = format!("{dom}/{lab}");
    if jobs.loaded() {
        jobs.bootout();
        let deadline = std::time::Instant::now() + wait;
        while jobs.loaded() {
            if std::time::Instant::now() > deadline {
                bail!(
                    "`launchctl bootout {target}` was issued {}s ago and the job is still \
                     loaded. Nothing was bootstrapped, because bootstrapping over a job that is \
                     still tearing down is how you get a reported success and an unheld \
                     environment. The plist and the binary ARE installed; run \
                     `launchctl bootout {target}` by hand and then `cauldron install` again.",
                    wait.as_secs()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        println!("cauldron: booted out the previous {lab} and waited for it to go");
    }
    // DG-2: nothing holds the launchd log now (any previous job is booted out
    // and confirmed gone), and launchd opens it afresh when it spawns the job.
    if rotate_launchd_log(env) {
        println!("cauldron: rotated {} to .1 (past 64 MiB)", launchd_log_path(env).display());
    }
    let st = jobs.bootstrap()?;
    if !st.success() {
        bail!(
            "`launchctl bootstrap {dom} {}` failed with {st}. The plist is written and the \
             binary is installed; nothing is loaded.",
            plist_file.display()
        );
    }
    // VERIFIED, not assumed.
    let deadline = std::time::Instant::now() + wait;
    while !jobs.loaded() {
        if std::time::Instant::now() > deadline {
            bail!(
                "`launchctl bootstrap` reported success and {target} is NOT loaded {}s later. \
                 The exit code is not the evidence and this is why it is checked: the plist and \
                 the binary are installed, and the environment is NOT held.",
                wait.as_secs()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    println!("cauldron: bootstrapped {lab} into {dom}, and launchd confirms it is loaded");
    Ok(())
}

/// The system directories every macOS user has, in launchd's own order.
const SYSTEM_PATH: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// **The plist's `PATH` when the installing shell has none** (REL-4.S3).
///
/// It used to be a literal naming the operator's home directory, so a
/// stranger's job would have looked for cargo in his. Now it is DERIVED:
/// cargo's bin dir from `$CARGO_HOME` (rustup's own override), else
/// `$HOME/.cargo/bin`, else nothing; then the system directories. Pure, so the
/// test can plant a HOME and prove no other user's path is in the output.
pub fn fallback_path(home: Option<&str>, cargo_home: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    match (cargo_home, home) {
        (Some(c), _) => parts.push(format!("{}/bin", c.trim_end_matches('/'))),
        (None, Some(h)) => parts.push(format!("{}/.cargo/bin", h.trim_end_matches('/'))),
        (None, None) => {}
    }
    parts.extend(SYSTEM_PATH.iter().map(|s| s.to_string()));
    parts.join(":")
}

/// **The plist, generated from the environment's own policy.**
///
/// `RunAtLoad` and `KeepAlive` both come from `[supervisor] auto_restart`,
/// which is the operator's sentence — *"prod should auto-restart on computer
/// restart or failure"* — turned into the two keys that mean it. An
/// environment with `auto_restart = false` still gets a job, so that
/// `launchctl kickstart` works and so that the job is visible beside the
/// others, but launchd will neither start it at login nor revive it.
///
/// The label is passed in already resolved (`install` resolves it against an
/// injected LaunchAgents dir, not `$HOME`'s — REL-4.S3c).
pub fn plist_labelled(env: &Environment, m: &Manifest, lab: &str) -> Result<String> {
    let p = &m.supervisor;
    let prog = launch_path(env);
    let log = launchd_log_path(env);
    let home = std::env::var("HOME").unwrap_or_default();
    let path_var = std::env::var("PATH").ok().filter(|p| !p.is_empty()).unwrap_or_else(|| {
        fallback_path(
            Some(home.as_str()).filter(|h| !h.is_empty()),
            std::env::var("CARGO_HOME").ok().as_deref().filter(|c| !c.is_empty()),
        )
    });
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <!-- GENERATED BY `cauldron install`. Do not hand-edit: the environment's
         own `env.toml` is the source of truth for every value below that is
         not a path, and a hand edit here is a second place the policy lives.
         Re-run `cauldron install` after changing [supervisor] in
         {manifest}. -->

    <key>Label</key>
    <string>{lab}</string>

    <!-- ONE PROCESS PER ENVIRONMENT, WITH ITS OWN NAME. The file at this path
         is called `{pname}`, so that is what `ps` shows, what `codesign`
         signs, and what a macOS permission prompt names. That is the whole
         reason the install copies and renames rather than symlinking: a
         permission he approves for this environment does not silently cover
         another one. -->
    <key>ProgramArguments</key>
    <array>
        <string>{prog}</string>
        <string>daemon</string>
    </array>

    <!-- START AT LOGIN: {auto_restart}, from `[supervisor] auto_restart` in
         {manifest}. -->
    <key>RunAtLoad</key>
    <{auto_restart}/>

    <!-- RESTART ON ANY EXIT, UNCONDITIONALLY — not `SuccessfulExit = false`,
         which is what every com.harness.* job on this machine uses.
         That exception existed because the old supervisor had three honest
         reasons to exit 0 (a deliberate stop, a lost bind race, a duplicate)
         and reviving it in those cases produced two supervisors. All three are
         gone: there is no second launcher, a stop is a state the daemon holds,
         and a duplicate is refused by a KERNEL LOCK with exit {exit_held}.
         The exit code therefore carries nothing launchd should act on.

         This is the half of 2026-09-22 that launchd owned. A stale pidfile and
         a reused pid made the supervisor say "another supervisor is already
         running" and exit 0; `SuccessfulExit = false` is what made that
         refusal STICK, and tts went unsupervised for sixteen days. -->
    <key>KeepAlive</key>
    <{auto_restart}/>

    <!-- If the holder is crash-looping, do not spin. -->
    <key>ThrottleInterval</key>
    <integer>{throttle}</integer>

    <!-- The environment, and NOTHING ELSE about what is hosted. launchd does
         not know a service name, a port, a binary, a health URL or a data
         path: all of that is in `env.toml` and `services/*.toml`, where he can
         read it. CAULDRON_ENV and not ENV — ENV is reserved by POSIX sh. -->
    <key>EnvironmentVariables</key>
    <dict>
        <key>CAULDRON_ENV</key>
        <string>{envname}</string>
        <key>CAULDRON_HOME</key>
        <string>{root}</string>
        <key>HOME</key>
        <string>{home}</string>
        <key>PATH</key>
        <string>{path_var}</string>
    </dict>

    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        lab = xml_escape(&lab),
        pname = xml_escape(&process_name(&env.name)),
        prog = xml_escape(&prog.display().to_string()),
        auto_restart = p.auto_restart,
        throttle = p.throttle_secs,
        envname = xml_escape(&env.name),
        root = xml_escape(&env.root.display().to_string()),
        home = xml_escape(&home),
        path_var = xml_escape(&path_var),
        log = xml_escape(&log.display().to_string()),
        manifest = xml_escape(&env.manifest().display().to_string()),
        exit_held = crate::daemon::EXIT_HELD,
    ))
}

/// **Read-only launchd queries for the RUNNING-JOB GUARD** (REL-4.S3c): the
/// two calls `scripts/install` step 2b makes, behind a trait so the guard is
/// testable with a stub and no real `launchctl`.
pub(crate) trait LaunchdQuery {
    /// `launchctl print <target>`: `None` when nothing is loaded under it,
    /// else `Some(pid)` (`"-"` when loaded with no running process).
    fn print_pid(&self, target: &str) -> Option<String>;
    /// `launchctl list` stdout ("PID Status Label" per loaded job), or why it
    /// could not be read.
    fn list(&self) -> Result<String>;
}

/// The real [`LaunchdQuery`]: `/bin/launchctl print|list`, nothing else.
struct LaunchctlQuery;

impl LaunchdQuery for LaunchctlQuery {
    fn print_pid(&self, target: &str) -> Option<String> {
        let o = std::process::Command::new("/bin/launchctl")
            .args(["print", target])
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if !o.status.success() {
            return None;
        }
        Some(parse_print_pid(&String::from_utf8_lossy(&o.stdout)).unwrap_or_else(|| "-".to_string()))
    }
    fn list(&self) -> Result<String> {
        let o = std::process::Command::new("/bin/launchctl")
            .arg("list")
            .stderr(std::process::Stdio::null())
            .output()
            .context("running `launchctl list`")?;
        if !o.status.success() {
            bail!("`launchctl list` exited with {}", o.status);
        }
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    }
}

/// The `pid = N` line of `launchctl print` output.
fn parse_print_pid(out: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        match (w.next(), w.next(), w.next()) {
            (Some("pid"), Some("="), Some(pid)) => Some(pid.to_string()),
            _ => None,
        }
    })
}

/// What [`loaded_jobs_for_env`] found: `(label, pid)` per loaded job, and
/// why the any-prefix scan could not run, if it could not.
#[derive(Debug, Default)]
pub(crate) struct LoadedJobs {
    pub jobs: Vec<(String, String)>,
    pub list_failed: Option<String>,
}

/// **Every cauldron job for `env_name` loaded in `dom`, under ANY label** —
/// the same detection as `scripts/install` step 2b: `print` under the
/// configured, default and legacy prefixes, then a `list` scan for any
/// `<prefix>.<process name>`. A failed `list` is REPORTED in `list_failed`,
/// never silently treated as "nothing there"; the named prefixes are still
/// checked.
pub(crate) fn loaded_jobs_for_env(
    q: &dyn LaunchdQuery,
    dom: &str,
    env_name: &str,
    configured: Option<&str>,
) -> LoadedJobs {
    let pname = process_name(env_name);
    let mut found = LoadedJobs::default();
    let note = |lab: &str, pid: &str, jobs: &mut Vec<(String, String)>| {
        if !jobs.iter().any(|(l, _)| l == lab) {
            jobs.push((lab.to_string(), pid.to_string()));
        }
    };
    for prefix in configured.into_iter().chain([DEFAULT_LABEL_PREFIX, LEGACY_LABEL_PREFIX]) {
        let lab = format!("{prefix}.{pname}");
        if let Some(pid) = q.print_pid(&format!("{dom}/{lab}")) {
            note(&lab, &pid, &mut found.jobs);
        }
    }
    let suffix = format!(".{pname}");
    match q.list() {
        Ok(out) => {
            for line in out.lines() {
                let mut w = line.split_whitespace();
                if let (Some(pid), Some(_), Some(lab)) = (w.next(), w.next(), w.next()) {
                    if lab.len() > suffix.len() && lab.ends_with(&suffix) {
                        note(lab, pid, &mut found.jobs);
                    }
                }
            }
        }
        Err(e) => found.list_failed = Some(format!("{e:#}")),
    }
    found
}

/// Single-quote `s` for a shell when it needs it.
fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// **The running-job guard of `cauldron install`** (REL-4.S3c): `true` to
/// proceed. An install boots the job out and bootstraps it again, i.e. it
/// RESTARTS the running holder, so a loaded job for this environment refuses
/// the install unless `--replace-running`. The refusal names what it found
/// and the exact command to proceed (absolute path to this binary).
pub(crate) fn running_job_guard(
    q: &dyn LaunchdQuery,
    dom: &str,
    env: &Environment,
    configured: Option<&str>,
    opts: InstallOpts,
    me: &Path,
) -> bool {
    let found = loaded_jobs_for_env(q, dom, &env.name, configured);
    if let Some(why) = &found.list_failed {
        eprintln!(
            "cauldron: WARNING: `launchctl list` could not be read ({why}), so a job for '{}'              under an UNKNOWN label prefix cannot be detected. Only the configured, default and              legacy labels were checked.",
            env.name
        );
    }
    if found.jobs.is_empty() {
        println!("cauldron: no cauldron job for '{}' is loaded in {dom}", env.name);
        return true;
    }
    for (lab, pid) in &found.jobs {
        println!("cauldron: LOADED     {lab} (pid {pid}) in {dom}");
    }
    if !opts.replace_running {
        let mut cmd = format!(
            "{} --env {} install --replace-running",
            sh_quote(&me.display().to_string()),
            sh_quote(&env.name)
        );
        if opts.dry {
            cmd.push_str(" --dry-run");
        }
        eprintln!(
            "cauldron: REFUSED: a cauldron job for environment '{}' is already loaded (listed              above). Installing over it boots it out and bootstraps it again, which RESTARTS              that running holder. Nothing was written or loaded.\n\
             cauldron: To replace it and restart it, run exactly:\n\
             cauldron:   {cmd}",
            env.name
        );
        return false;
    }
    for (lab, pid) in &found.jobs {
        println!("cauldron: WILL RESTART {lab} (pid {pid}): it is booted out and bootstrapped again (--replace-running)");
    }
    true
}

/// What `cauldron install` was asked for.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct InstallOpts {
    pub dry: bool,
    pub replace_running: bool,
}

/// **`cauldron install` — put this environment's holder under launchd.**
pub fn verb_install(args: &[String]) -> Result<i32> {
    let mut opts = InstallOpts::default();
    for a in args {
        match a.as_str() {
            "--dry-run" => opts.dry = true,
            "--replace-running" => opts.replace_running = true,
            other => bail!(
                "unknown option `{other}` for `cauldron install` (only `--dry-run`, `--replace-running`)"
            ),
        }
    }
    let env = Environment::resolve()?;
    let me = std::env::current_exe().context("resolving this binary's own path")?;
    let agents = launch_agents_dir().context(
        "$HOME is not set, so there is no LaunchAgents directory to install into",
    )?;
    let dom = format!("gui/{}", unsafe_uid());
    let lab = label_with(&label_prefix_in(&env, Some(&agents)), &env.name);
    let jobs = Launchctl {
        dom: dom.clone(),
        target: format!("{dom}/{lab}"),
        plist_file: agents.join(format!("{lab}.plist")),
    };
    install_with(&env, opts, &me, &agents, &dom, &LaunchctlQuery, &jobs)
}

/// [`verb_install`] with every outside thing injected — the binary being
/// installed, the LaunchAgents dir, the domain, and both launchd interfaces —
/// so the whole verb, guard included, runs in a test against stubs.
pub(crate) fn install_with(
    env: &Environment,
    opts: InstallOpts,
    me: &Path,
    agents: &Path,
    dom: &str,
    query: &dyn LaunchdQuery,
    jobs: &dyn JobControl,
) -> Result<i32> {
    let dry = opts.dry;
    env.ensure_dirs()?;
    println!("{}", env.banner());
    let m = Manifest::load(&env.manifest())?;

    let dest = launch_path(env);
    guard::refuse_protected(&dest)?;
    let lab = label_with(&label_prefix_in(env, Some(agents)), &env.name);
    let plist_file = agents.join(format!("{lab}.plist"));
    let body = plist_labelled(env, &m, &lab)?;
    let why = match (&m.supervisor.label_prefix, lab.starts_with(&format!("{LEGACY_LABEL_PREFIX}."))) {
        (Some(_), _) => format!("`[supervisor] label_prefix` in {}", env.manifest().display()),
        (None, true) => "a job is already installed under the pre-release prefix, so it keeps it".to_string(),
        (None, false) => "the default".to_string(),
    };
    {
        let clash = conflicting_plists(agents, &env.name, &lab);
        if !clash.is_empty() {
            bail!(
                "REFUSED: this environment's label resolves to `{lab}` ({why}), but a job for the \
                 SAME environment is already installed as {}. Two jobs would be two holders \
                 of one environment. Either set `[supervisor] label_prefix` in {} to that job's \
                 prefix, or remove the old job first (`launchctl bootout gui/<uid>/<label>` and \
                 delete its plist). Nothing was written.",
                clash.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "),
                env.manifest().display()
            );
        }
    }

    println!("cauldron: process name  {}", process_name(&env.name));
    println!("cauldron: launch path   {}", dest.display());
    println!("cauldron: launchd label {lab}  (prefix from {why})");
    println!("cauldron: plist         {}", plist_file.display());
    println!(
        "cauldron: RunAtLoad/KeepAlive {} (from `[supervisor] auto_restart` in {})",
        m.supervisor.auto_restart,
        env.manifest().display()
    );
    // ── THE RUNNING-JOB GUARD (REL-4.S3c) ──────────────────────────────
    // Before the dry-run return and before anything is written, so a dry run
    // shows the same refusal a real one would hit.
    if !running_job_guard(query, dom, env, m.supervisor.label_prefix.as_deref(), opts, me) {
        return Ok(2);
    }

    if dry {
        if m.supervisor.label_prefix.is_none() {
            println!(
                "cauldron: a real install records `label_prefix = {:?}` under [supervisor] in {}",
                label_prefix_in(env, Some(agents)),
                env.manifest().display()
            );
        }
        println!("\n--- {} ---\n{body}", plist_file.display());
        println!("cauldron: --dry-run, so nothing was written and nothing was bootstrapped.");
        return Ok(0);
    }

    // ── REMEMBER THE PREFIX (REL-4.S3b) ─────────────────────────────────
    // Before anything else is written, so the plist below and every later
    // resolution agree on a prefix that no longer depends on a plist file.
    if let Some(p) = persist_label_prefix_in(env, Some(agents))? {
        println!(
            "cauldron: recorded `label_prefix = {p:?}` under [supervisor] in {}, so this job \
             keeps the name `{lab}` even after its plist is removed",
            env.manifest().display()
        );
    }

    // ── INSTALL THE BINARY BY ATOMIC RENAME ─────────────────────────────
    //
    // Never a symlink: `current_exe()` on macOS does not resolve one, a
    // permission prompt would name the link, and at least one daemon here
    // gates behaviour on its own file NAME. Never a copy written in place
    // either — the file may be running, and a rename is the only way to
    // replace a running image's path without ever presenting a half-written
    // one.
    let (_, tui) = place_install_files(env, me)?;
    println!("cauldron: installed {} -> {}", me.display(), dest.display());
    if let Some(t) = tui {
        println!("cauldron: installed the TUI beside it -> {}", t.display());
    }

    std::fs::create_dir_all(env.data_root.join("logs"))?;
    if let Some(p) = plist_file.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::write(&plist_file, &body)
        .with_context(|| format!("writing {}", plist_file.display()))?;
    println!("cauldron: wrote {}", plist_file.display());

    // ── BOOTOUT, WAIT FOR IT, ROTATE, BOOTSTRAP, THEN VERIFY ────────────
    // (`reload_job` says why each wait is there.)
    reload_job(env, jobs, dom, &plist_file, std::time::Duration::from_secs(20))?;
    if !m.supervisor.auto_restart {
        println!(
            "cauldron: NOTE — `auto_restart = false` in this environment, so launchd will not \
             start this job at login and will not revive it. Start it by hand with \
             `launchctl kickstart {dom}/{}`.",
            lab
        );
    } else {
        println!(
            "cauldron: launchd will start it at login and revive it on ANY exit. \
             `launchctl kickstart -k {dom}/{}` restarts it now.",
            lab
        );
    }
    println!(
        "cauldron: SIGNING is a separate, deliberate act and is NOT done here. The launch path \
         is a distinct file with a distinct name, which is what makes a per-environment identity \
         possible; signing it is `codesign -s <identity> {}`. Per the ruling, a signature is \
         IDENTITY ONLY and never a gate: it says who shipped an image, and cauldron reads the \
         COMMIT to know what the image is.",
        dest.display()
    );
    Ok(0)
}

/// What `uninstall` managed to record about the label prefix (REL-4.S3c).
#[derive(Debug, PartialEq)]
pub(crate) enum UninstallPrefix {
    Recorded(String),
    AlreadySet,
    /// Not recorded, and why; the uninstall goes on regardless.
    Unrecorded(String),
}

/// [`persist_label_prefix_in`], best-effort: an error becomes
/// [`UninstallPrefix::Unrecorded`], never a refusal to uninstall.
pub(crate) fn record_prefix_for_uninstall(env: &Environment, agents: Option<&Path>) -> UninstallPrefix {
    match persist_label_prefix_in(env, agents) {
        Ok(Some(p)) => UninstallPrefix::Recorded(p),
        Ok(None) => UninstallPrefix::AlreadySet,
        Err(e) => UninstallPrefix::Unrecorded(format!("{e:#}")),
    }
}

/// **`cauldron uninstall` — take this environment's holder out from under
/// launchd.**
///
/// It does NOT stop the services. That is deliberate and it is the same
/// property re-attach rests on: the children are in their own process groups,
/// so removing the job leaves them running and a later `cauldron install`
/// finds them again through the registry. An uninstall that killed the voice
/// daemon as a side effect of editing a launchd job would be a surprise worth
/// avoiding.
pub fn verb_uninstall(args: &[String]) -> Result<i32> {
    if let Some(a) = args.first() {
        bail!("`cauldron uninstall` takes no arguments (got `{a}`)");
    }
    let env = Environment::resolve()?;
    println!("{}", env.banner());
    let plist_file = plist_path(&env)?;
    // REL-4.S3b: the plist about to be deleted may be the only evidence of
    // this job's prefix (the legacy arm). Record it first, so a later
    // `install` re-creates the SAME label rather than the default one.
    //
    // REL-4.S3c: BEST-EFFORT, with a warning, and still BEFORE the plist is
    // deleted. Before, because the plist is the evidence being recorded;
    // best-effort, because an uninstall is how a user gets OUT of a broken
    // state, and an unparseable env.toml must not leave the job loaded with
    // no way to remove it short of hand-run launchctl.
    if plist_file.exists() {
        match record_prefix_for_uninstall(&env, launch_agents_dir().as_deref()) {
            UninstallPrefix::Recorded(p) => println!(
                "cauldron: recorded `label_prefix = {p:?}` under [supervisor] in {}, so a later \
                 `cauldron install` keeps the label {}",
                env.manifest().display(),
                label(&env)
            ),
            UninstallPrefix::AlreadySet => {}
            UninstallPrefix::Unrecorded(why) => eprintln!(
                "cauldron: WARNING: could not record this job's label prefix in {} ({why}). \
                 Uninstalling anyway, as {}. A later `cauldron install` may give it a different \
                 label unless you set `label_prefix = \"{}\"` under [supervisor] once that file \
                 is repaired.",
                env.manifest().display(),
                label(&env),
                label_prefix(&env)
            ),
        }
    }
    let dom = format!("gui/{}", unsafe_uid());
    let st = launchctl_control("bootout")?
        .args(["bootout", &format!("{dom}/{}", label(&env))])
        .status();
    match st {
        Ok(s) if s.success() => println!("cauldron: booted out {}", label(&env)),
        _ => println!("cauldron: {} was not loaded", label(&env)),
    }
    if plist_file.exists() {
        std::fs::remove_file(&plist_file)?;
        println!("cauldron: removed {}", plist_file.display());
    }
    println!(
        "cauldron: the hosted services were NOT stopped and are still running — they are in \
         their own process groups, so they were never in launchd's job. `cauldron status` shows \
         them; `cauldron stop <service>` stops one."
    );
    println!(
        "cauldron: the launch path {} was left in place, because deleting the binary a loaded \
         job might still be asked to start is how a job becomes unstartable.",
        launch_path(&env).display()
    );
    Ok(0)
}

/// **Put `src` at this environment's launch path**, by copy to a dot-file
/// beside it and an atomic rename (R6.S6, factored out of `install`).
///
/// A BYTE COPY, so an embedded code signature travels with it unchanged: the
/// designated requirement names an identifier and a certificate leaf, never a
/// path, so `codesign -v --strict` and `codesign -d -r-` read the same on the
/// copy as on the source. Never a symlink, for the reasons in the module doc.
pub fn place_image(env: &Environment, src: &Path) -> Result<PathBuf> {
    let dest = launch_path(env);
    guard::refuse_protected(&dest)?;
    std::fs::create_dir_all(bin_dir(env))?;
    let tmp = bin_dir(env).join(format!(".{}.installing.{}", process_name(&env.name), std::process::id()));
    std::fs::copy(src, &tmp).with_context(|| format!("copying {} -> {}", src.display(), tmp.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&tmp)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&tmp, perm)?;
    }
    replace_keeping_prev(&tmp, &dest)?;
    Ok(dest)
}

/// **ONE `.prev` PER BINARY, AND IT IS ALWAYS AN EARLIER BUILD** (DG-3).
///
/// Moves the staged `tmp` onto `dest` by atomic rename. When `dest` already
/// holds a DIFFERENT build, that build is kept first as `<dest>.prev` — a HARD
/// LINK to the old inode, renamed over any older `.prev`, so `dest` is never
/// absent for an instant and the rollback copy is byte-for-byte the build that
/// was running.
///
/// **A re-install of the SAME build** (`cauldron install` run from the launch
/// path itself, or a retried install; `tmp` and `dest` equal byte for byte)
/// leaves `.prev` alone: overwriting it would make the rollback target the
/// current image and destroy the only earlier build (DG-3 inspect A1).
///
/// **Pruning `<name>.<hex hash>` copies** (the hand-made deploy backups the
/// CAULDRON-DATA census counted, which DEPLOY rollback lines name) happens
/// ONLY after an install that produced a distinct `.prev`, and even then the
/// NEWEST hash copy (by mtime) is kept, so the most recent recorded rollback
/// line still resolves (DG-3 inspect A2). Only `dest`'s own directory is
/// touched, and no other name.
pub fn replace_keeping_prev(tmp: &Path, dest: &Path) -> Result<()> {
    let dir = dest.parent().context("an install destination has a directory")?;
    let name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .context("an install destination has a UTF-8 file name")?
        .to_string();
    let mut kept_prev = false;
    if dest.is_file() && !same_bytes(tmp, dest) {
        let prev = dir.join(format!("{name}.prev"));
        let staged = dir.join(format!(".{name}.prev.installing.{}", std::process::id()));
        let _ = std::fs::remove_file(&staged);
        if std::fs::hard_link(dest, &staged).is_err() {
            std::fs::copy(dest, &staged)
                .with_context(|| format!("keeping {} as {}", dest.display(), prev.display()))?;
        }
        std::fs::rename(&staged, &prev).with_context(|| format!("renaming into {}", prev.display()))?;
        kept_prev = true;
    }
    std::fs::rename(tmp, dest).with_context(|| format!("renaming into {}", dest.display()))?;
    if kept_prev {
        prune_hash_copies(dir, &name);
    }
    Ok(())
}

/// Do these two files hold the same bytes? An unreadable side is "not the
/// same", which keeps the earlier build as `.prev` — the safe direction.
fn same_bytes(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) if x.len() == y.len() => {}
        _ => return false,
    }
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// `<name>.<hash>`: the name, a dot, then 7 to 40 hex digits and nothing else.
/// `cauldron.facae2dfa8f9` is one; `cauldron.prev`, `tui.pre-r24` and
/// `cauldron-stage` are not.
pub fn is_hash_copy(file: &str, name: &str) -> bool {
    file.strip_prefix(name)
        .and_then(|r| r.strip_prefix('.'))
        .is_some_and(|h| (7..=40).contains(&h.len()) && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Remove every `<name>.<hash>` file in `dir` EXCEPT the newest by mtime
/// (ties broken by file name, so the choice is deterministic).
fn prune_hash_copies(dir: &Path, name: &str) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut copies: Vec<(std::time::SystemTime, String, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| {
            let f = e.file_name().into_string().ok()?;
            if !is_hash_copy(&f, name) {
                return None;
            }
            let at = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            Some((at, f, e.path()))
        })
        .collect();
    copies.sort();
    copies.pop(); // the newest is kept
    for (_, _, path) in copies {
        match std::fs::remove_file(&path) {
            Ok(()) => println!("cauldron: pruned the old copy {}", path.display()),
            Err(err) => eprintln!("cauldron: could not prune {}: {err}", path.display()),
        }
    }
}

/// **The files `install` writes before it touches launchd**: this binary at
/// the launch path, and the `tui` beside it (if `me`'s directory has one).
///
/// The TUI goes beside the launch path because `cauldron <app>` looks for an
/// app BESIDE its own binary and a bare `cauldron` reaches the TUI; installing
/// only the router would give him a launch path whose default verb fails. It
/// goes under the environment's OWN name (`tui` in prod, `tui-<env>`
/// elsewhere), because the root's `bin/` is shared and prod's face is
/// `bin/tui` (R6.S8). Factored out of `verb_install` so the placement can be
/// tested without the launchd half.
pub fn place_install_files(env: &Environment, me: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    let dest = place_image(env, me)?;
    let tui = match me.parent() {
        Some(dir) => place_tui(env, dir)?,
        None => None,
    };
    Ok((dest, tui))
}

/// **`tui` in prod, `tui-<env>` everywhere else** (R6.S8) — the file name an
/// app is installed under for this environment, so a non-prod install never
/// lands on prod's face. The same convention as [`process_name`], and the one
/// `cauldron <app>` looks up first (`main.rs`, `app`).
pub fn app_file_name(app: &str, env_name: &str) -> String {
    if env_name == PLAIN_ENV {
        app.to_string()
    } else {
        format!("{app}-{env_name}")
    }
}

/// **Put the `tui` found in `src_dir` beside this environment's launch path**,
/// under [`app_file_name`], by copy to a dot-file and an atomic rename.
/// Returns where it went, or `None` when `src_dir` holds no `tui`.
///
/// Before R6.S8 this wrote `<root>/bin/tui` for EVERY environment, and the
/// root is shared, so `cauldron --env stage install` replaced prod's face.
pub fn place_tui(env: &Environment, src_dir: &Path) -> Result<Option<PathBuf>> {
    let tui = src_dir.join("tui");
    if !tui.exists() {
        return Ok(None);
    }
    std::fs::create_dir_all(bin_dir(env))?;
    let t = bin_dir(env).join(app_file_name("tui", &env.name));
    guard::refuse_protected(&t)?;
    let ttmp = bin_dir(env).join(format!(".{}.installing.{}", app_file_name("tui", &env.name), std::process::id()));
    std::fs::copy(&tui, &ttmp).with_context(|| format!("copying {} -> {}", tui.display(), ttmp.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&ttmp)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&ttmp, perm)?;
    }
    replace_keeping_prev(&ttmp, &t)?;
    Ok(Some(t))
}

/// Set on the re-exec below, so the exec'd process can tell a relaunch that
/// did not land on its own launch path (which would loop) from a first call.
const RELAUNCHED_FROM: &str = "CAULDRON_RELAUNCHED_FROM";

fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// **A non-prod holder runs from its OWN launch path** (R6.S6, H179-1; the
/// Deploy-3 finding, ledger E10-5).
///
/// The holder's self-update watches the file it was launched from
/// (`daemon.rs`, `supervise`). Before this, `cauldron --env stage daemon` typed
/// at `~/.cauldron/bin/cauldron` ran prod's FILE, so every prod holder deploy
/// made the stage holder exit too, and nothing restarted it. So for any
/// environment but `prod`, `daemon` makes sure it is executing
/// `<root>/bin/cauldron-<env>` — the same file `install` already points that
/// environment's launchd job at — and if it is not, it EXECS that file with
/// the same arguments before it takes the hold (so there is no window with the
/// hold or a port taken). If the file does not exist yet, this binary places
/// it there first (`place_image`). After that it changes only by
/// `cauldron --env <e> promote` or `cauldron --env <e> install`, so the holder
/// self-updates only when ITS OWN launch path changes.
///
/// `prod` is untouched: its holder runs from wherever it was started, exactly
/// as before.
///
/// Returns only when no exec is needed; a successful exec does not return.
pub fn hold_from_own_launch_path(env: &Environment) -> Result<()> {
    if env.name == PLAIN_ENV {
        return Ok(());
    }
    let me = std::env::current_exe().context("resolving this binary's own path")?;
    let own = launch_path(env);
    if same_file(&me, &own) {
        // R6.S6b: said only by the image that the exec below landed on, so a
        // reader (and `own_launch_path.rs`) can tell an exec that happened
        // from one that was skipped — the "OWN launch path" line is printed
        // BEFORE the exec and cannot.
        if let Some(from) = std::env::var_os(RELAUNCHED_FROM) {
            println!(
                "cauldron: `{}` exec'd onto its own launch path {} (pid {}, relaunched from {})",
                env.name,
                me.display(),
                std::process::id(),
                Path::new(&from).display()
            );
        }
        // Single-threaded here (nothing is spawned before the hold), so this is
        // the one safe point to drop the marker; services never see it.
        std::env::remove_var(RELAUNCHED_FROM);
        return Ok(());
    }
    if let Some(from) = std::env::var_os(RELAUNCHED_FROM) {
        bail!(
            "REFUSED: `{}` was exec'd as this environment's own launch path {} (from {}), and \
             the running image {} is not that file. Not re-exec'ing again, which would loop.",
            env.name,
            own.display(),
            Path::new(&from).display(),
            me.display()
        );
    }
    if !own.exists() {
        place_image(env, &me)?;
        println!(
            "cauldron: `{}` had no launch path of its own; placed {} -> {}",
            env.name,
            me.display(),
            own.display()
        );
    }
    println!(
        "cauldron: `{}` holds from its OWN launch path {}, not {}, so a deploy of another \
         environment's file cannot stop it. Replace it with `cauldron --env {} promote`.",
        env.name,
        own.display(),
        me.display(),
        env.name
    );
    // REL-6a6: this forwards every verb to another binary; under isolation
    // only to one in a scratch world, never his installed CLI.
    env::egress::admit_daemon_path("re-exec'ing an environment's own launch path", &own, env::egress::process_isolation().as_deref())
        .map_err(anyhow::Error::msg)?;
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(&own)
        .args(std::env::args_os().skip(1))
        .env(RELAUNCHED_FROM, &me)
        .exec();
    Err(err).with_context(|| format!("exec'ing this environment's own launch path {}", own.display()))
}

/// **`cauldron --env <e> promote` — place THIS binary at a non-prod
/// environment's own launch path** (R6.S6).
///
/// That file is what the environment's holder runs from (`daemon` and
/// `install` both launch it), so promoting is how a non-prod environment takes
/// a new cauldron. A holder running from it sees the file change and exits on
/// its next pass (`exit`/`self_update`); under launchd it is revived on the new
/// bytes, a hand-run holder is restarted with `cauldron --env <e> daemon`.
///
/// Refuses `prod`: prod's launch path is placed by the signed deploy (or
/// `install`), and nothing about it changes here.
pub fn verb_promote(args: &[String]) -> Result<i32> {
    if let Some(a) = args.first() {
        bail!("`cauldron promote` takes no arguments (got `{a}`); it places THIS binary");
    }
    let env = Environment::resolve()?;
    println!("{}", env.banner());
    if env.name == PLAIN_ENV {
        bail!(
            "REFUSED: `promote` is for a non-prod environment's own launch path. `prod`'s \
             launch path {} is placed by the signed deploy (or `cauldron install`), and this \
             verb does not touch it.",
            launch_path(&env).display()
        );
    }
    if !env.manifest().exists() {
        bail!("REFUSED: no environment `{}` here (no {}); `cauldron env create {}` first", env.name, env.manifest().display(), env.name);
    }
    let me = std::env::current_exe().context("resolving this binary's own path")?;
    let own = launch_path(&env);
    if same_file(&me, &own) {
        println!("cauldron: {} IS this environment's launch path; nothing to promote", own.display());
        return Ok(0);
    }
    place_image(&env, &me)?;
    println!("cauldron: promoted {} -> {} (commit {})", me.display(), own.display(), env!("CAULDRON_SELF_COMMIT"));
    println!(
        "cauldron: a holder running from it exits on its next pass (self_update). Under launchd \
         it comes back on these bytes; a hand-run holder needs `cauldron --env {} daemon` again. \
         The copy is byte-identical, so a signature on the source is on it too: check with \
         `codesign -d -r- {}`.",
        env.name,
        own.display()
    );
    Ok(0)
}

/// The real uid, for the launchd domain. `id -u` rather than a libc call,
/// matching this crate's rule of no `unsafe` and no `libc`; the name is a joke
/// about what it is not.
fn unsafe_uid() -> String {
    std::process::Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "501".to_string())
}

#[cfg(test)]
mod install_tui_tests {
    //! R6.S8: a non-prod `install` must not overwrite prod's `<root>/bin/tui`.
    //! Each case runs `place_install_files` (exactly what `verb_install` runs
    //! before its launchd half) in a throwaway root, with a sentinel already
    //! sitting at prod's face. Stage leaves it alone; prod replaces it — so a
    //! placement that always skips, or always writes `bin/tui`, fails one.
    use super::*;
    use env::paths::Source;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const SENTINEL: &[u8] = b"PROD-TUI-SENTINEL-r6s8\n";
    const NEW_TUI: &[u8] = b"#!/bin/sh\necho NEW-TUI\n";

    /// A throwaway root with prod's `bin/tui` already holding `SENTINEL`, and
    /// a build dir holding a `cauldron` and a different `tui`.
    fn setup(tag: &str) -> (Tmp, PathBuf, PathBuf) {
        let t = std::env::temp_dir().join(format!("cauldron-r6s8-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let root = t.join("home").join(".cauldron");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin").join("tui"), SENTINEL).unwrap();
        let build = t.join("build");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(build.join("cauldron"), b"router").unwrap();
        std::fs::write(build.join("tui"), NEW_TUI).unwrap();
        // Two different things are compared below; prove they differ.
        assert_ne!(SENTINEL, NEW_TUI);
        (Tmp(t), root, build.join("cauldron"))
    }

    fn env_at(root: &Path, name: &str) -> Environment {
        Environment {
            root: root.to_path_buf(),
            root_source: Source::CauldronHome,
            name: name.to_string(),
            name_source: Source::Flag,
            data_root: root.join("data").join(name),
        }
    }

    #[test]
    fn a_stage_install_leaves_prods_tui_and_places_its_own_tui_stage() {
        let (_t, root, me) = setup("stage");
        let (dest, tui) = place_install_files(&env_at(&root, "stage"), &me).unwrap();
        assert_eq!(dest, root.join("bin").join("cauldron-stage"));
        assert_eq!(
            std::fs::read(root.join("bin").join("tui")).unwrap(),
            SENTINEL,
            "`--env stage install` overwrote prod's bin/tui"
        );
        assert_eq!(tui, Some(root.join("bin").join("tui-stage")));
        assert_eq!(std::fs::read(root.join("bin").join("tui-stage")).unwrap(), NEW_TUI);
    }

    #[test]
    fn a_prod_install_still_replaces_prods_tui() {
        let (_t, root, me) = setup("prod");
        let (dest, tui) = place_install_files(&env_at(&root, "prod"), &me).unwrap();
        assert_eq!(dest, root.join("bin").join("cauldron"));
        assert_eq!(tui, Some(root.join("bin").join("tui")));
        assert_eq!(std::fs::read(root.join("bin").join("tui")).unwrap(), NEW_TUI);
        assert!(!root.join("bin").join("tui-prod").exists());
    }

    /// **DG-3: two consecutive installs leave exactly ONE `.prev`, and it is
    /// the PREVIOUS build** — for the router and the TUI alike, and with the
    /// `<bin>.<hash>` copies pruned from the install dir only. Three distinct
    /// builds (A, B, C) so "the previous build" (B) is distinguishable from
    /// both the current one (C) and the oldest (A).
    #[test]
    fn two_installs_leave_exactly_one_prev_and_it_is_the_previous_build() {
        let (_t, root, me) = setup("dg3");
        let bin = root.join("bin");
        let build = me.parent().unwrap().to_path_buf();
        // Hand-made deploy backups in the install dir (two per binary, the
        // NEWER of each to be kept), and one OUTSIDE it.
        let aged = |p: &Path, secs_ago: u64| {
            std::fs::write(p, b"old").unwrap();
            let f = std::fs::File::options().write(true).open(p).unwrap();
            f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago)).unwrap();
        };
        aged(&bin.join("cauldron.facae2dfa8f9"), 3600);
        aged(&bin.join("cauldron.00000000aaaa"), 7200);
        aged(&bin.join("tui.0123abcd"), 7200);
        aged(&bin.join("tui.2e2a543906e5"), 3600);
        std::fs::write(bin.join("tui.pre-r24"), b"kept").unwrap();
        std::fs::write(build.join("cauldron.facae2dfa8f9"), b"elsewhere").unwrap();
        let env = env_at(&root, "prod");
        for gen in [b"A".as_slice(), b"B", b"C"] {
            std::fs::write(&me, [b"router-".as_slice(), gen].concat()).unwrap();
            std::fs::write(build.join("tui"), [b"tui-".as_slice(), gen].concat()).unwrap();
            place_install_files(&env, &me).unwrap();
        }
        assert_eq!(std::fs::read(bin.join("cauldron")).unwrap(), b"router-C");
        assert_eq!(std::fs::read(bin.join("cauldron.prev")).unwrap(), b"router-B", "the .prev is the PREVIOUS build");
        assert_eq!(std::fs::read(bin.join("tui")).unwrap(), b"tui-C");
        assert_eq!(std::fs::read(bin.join("tui.prev")).unwrap(), b"tui-B", "the TUI's .prev is the previous build");
        let mut names: Vec<String> =
            std::fs::read_dir(&bin).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(
            names,
            ["cauldron", "cauldron.facae2dfa8f9", "cauldron.prev", "tui", "tui.2e2a543906e5", "tui.pre-r24", "tui.prev"],
            "exactly one .prev per binary, only the NEWEST hash copy kept, no staging files; tui.pre-r24 is not a hash copy"
        );
        assert!(build.join("cauldron.facae2dfa8f9").exists(), "nothing outside the install dir is pruned");
    }

    /// **DG-3 inspect A1: re-installing the SAME build keeps the earlier
    /// build as `.prev` and prunes nothing.** Install A, then B (so `.prev` is
    /// A), then B again from the launch path itself — the
    /// `~/.cauldron/bin/cauldron install` case. `.prev` must still be A, and
    /// the hash copies must all survive. Opposed by
    /// `two_installs_leave_exactly_one_prev_and_it_is_the_previous_build`,
    /// where distinct builds DO move `.prev` and DO prune.
    #[test]
    fn reinstalling_the_same_build_keeps_the_earlier_prev_and_prunes_nothing() {
        let (_t, root, me) = setup("dg3-same");
        let bin = root.join("bin");
        let build = me.parent().unwrap().to_path_buf();
        let env = env_at(&root, "prod");
        for gen in [b"A".as_slice(), b"B"] {
            std::fs::write(&me, [b"router-".as_slice(), gen].concat()).unwrap();
            std::fs::write(build.join("tui"), [b"tui-".as_slice(), gen].concat()).unwrap();
            place_install_files(&env, &me).unwrap();
        }
        assert_eq!(std::fs::read(bin.join("cauldron.prev")).unwrap(), b"router-A");
        std::fs::write(bin.join("cauldron.facae2dfa8f9"), b"old1").unwrap();
        std::fs::write(bin.join("cauldron.00000000aaaa"), b"old2").unwrap();
        // Re-install FROM THE LAUNCH PATH: `me` is the installed file itself.
        place_install_files(&env, &bin.join("cauldron")).unwrap();
        place_tui(&env, &bin).unwrap();
        assert_eq!(std::fs::read(bin.join("cauldron")).unwrap(), b"router-B");
        assert_eq!(
            std::fs::read(bin.join("cauldron.prev")).unwrap(),
            b"router-A",
            "a same-build reinstall must not make .prev the current image"
        );
        assert_eq!(std::fs::read(bin.join("tui.prev")).unwrap(), b"tui-A", "nor the TUI's");
        assert!(
            bin.join("cauldron.facae2dfa8f9").exists() && bin.join("cauldron.00000000aaaa").exists(),
            "a reinstall that kept no new .prev prunes no hash copy"
        );
    }

    #[test]
    fn a_first_install_leaves_no_prev() {
        let (_t, root, me) = setup("dg3-first");
        let env = env_at(&root, "stage");
        place_install_files(&env, &me).unwrap();
        assert!(root.join("bin").join("cauldron-stage").exists());
        assert!(!root.join("bin").join("cauldron-stage.prev").exists(), "nothing was there to keep");
    }

    #[test]
    fn a_hash_copy_is_name_dot_hex_and_nothing_else() {
        assert!(is_hash_copy("cauldron.facae2dfa8f9", "cauldron"));
        assert!(is_hash_copy("tui.2e2a5439", "tui"));
        assert!(!is_hash_copy("cauldron.prev", "cauldron"));
        assert!(!is_hash_copy("tui.pre-r24", "tui"));
        assert!(!is_hash_copy("cauldron-stage.facae2dfa8f9", "cauldron"));
        assert!(!is_hash_copy("tui-stage.facae2dfa8f9", "tui"));
        assert!(!is_hash_copy("cauldron.abc", "cauldron"), "too short to be a hash");
        assert!(!is_hash_copy("cauldron", "cauldron"));
    }

    #[test]
    fn app_file_name_is_plain_in_prod_and_suffixed_elsewhere() {
        assert_eq!(app_file_name("tui", "prod"), "tui");
        assert_eq!(app_file_name("tui", "stage"), "tui-stage");
    }

    /// DG-2: the file `rotate_launchd_log` rotates is the file the plist hands
    /// launchd (both `StandardOutPath` and `StandardErrorPath`), and it rotates
    /// to `.1` only past 64 MiB. Forced red with the rotation disabled; the
    /// under-bound half opposes a rotation that always fires. (Renamed by
    /// DG-2b: it never ran `install`; `dg2b_install_*` below does guard that
    /// wiring.)
    #[test]
    fn dg2_the_launchd_log_the_plist_names_rotates_only_past_the_bound() {
        let (_t, root, _) = setup("dg2launchd");
        let env = env_at(&root, "stage");
        std::fs::create_dir_all(env.data_root.join("logs")).unwrap();
        std::fs::write(env.data_root.join("env.toml"), "[supervisor]\n").unwrap();
        let m = Manifest::load(&env.data_root.join("env.toml")).unwrap();
        let body = plist_labelled(&env, &m, &label(&env)).unwrap();
        let log = launchd_log_path(&env);
        for key in ["StandardOutPath", "StandardErrorPath"] {
            let after = body.split(&format!("<key>{key}</key>")).nth(1).unwrap_or_else(|| panic!("no {key}"));
            let named = after.split("<string>").nth(1).and_then(|r| r.split("</string>").next()).unwrap();
            assert_eq!(named, xml_escape(&log.display().to_string()), "{key} is not the rotated path");
        }
        let one = crate::logrotate::generation_path(&log, 1);
        let f = std::fs::File::create(&log).unwrap();
        f.set_len(crate::logrotate::LOG_MAX_BYTES).unwrap();
        drop(f);
        assert!(!rotate_launchd_log(&env), "rotated at exactly the bound");
        assert!(!one.exists());
        std::fs::OpenOptions::new().append(true).open(&log).unwrap().set_len(crate::logrotate::LOG_MAX_BYTES + 1).unwrap();
        assert!(rotate_launchd_log(&env), "did not rotate one byte past the bound");
        assert_eq!(std::fs::metadata(&one).unwrap().len(), crate::logrotate::LOG_MAX_BYTES + 1);
        assert!(!log.exists(), "the launchd log was not moved (launchd recreates it at spawn)");
    }

    /// launchctl as three calls that record what the launchd log looked like
    /// at each one. `loaded` starts true (a previous job is there).
    struct FakeJobs {
        log: PathBuf,
        loaded: std::cell::Cell<bool>,
        events: std::cell::RefCell<Vec<String>>,
    }

    impl FakeJobs {
        fn seen(&self, what: &str) {
            let one = crate::logrotate::generation_path(&self.log, 1);
            self.events.borrow_mut().push(format!("{what} live={} dot1={}", self.log.exists(), one.exists()));
        }
    }

    impl JobControl for FakeJobs {
        fn loaded(&self) -> bool {
            self.loaded.get()
        }
        fn bootout(&self) {
            self.seen("bootout");
            self.loaded.set(false);
        }
        fn bootstrap(&self) -> Result<std::process::ExitStatus> {
            use std::os::unix::process::ExitStatusExt;
            self.seen("bootstrap");
            self.loaded.set(true);
            Ok(std::process::ExitStatus::from_raw(0))
        }
    }

    fn reload_with(tag: &str, size: u64) -> (Tmp, PathBuf, Vec<String>) {
        let (t, root, _) = setup(tag);
        let env = env_at(&root, "stage");
        std::fs::create_dir_all(env.data_root.join("logs")).unwrap();
        let log = launchd_log_path(&env);
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&log).unwrap();
            f.write_all(b"OLD-LAUNCHD-LOG").unwrap();
            f.set_len(size).unwrap();
        }
        let jobs = FakeJobs { log: log.clone(), loaded: true.into(), events: Default::default() };
        let plist_file = root.join("fake.plist");
        reload_job(&env, &jobs, "gui/0", &plist_file, std::time::Duration::from_secs(2)).unwrap();
        let ev = jobs.events.borrow().clone();
        (t, log, ev)
    }

    /// DG-2b item 3: `cauldron install`'s launchd half (`reload_job`, the
    /// code `verb_install` runs) rotates a launchd log past the bound AFTER
    /// the old job is booted out and BEFORE the new one is bootstrapped.
    /// Forced red by removing the rotate call from `reload_job`, and by moving
    /// it ahead of the bootout. Opposed by the under-bound test below.
    #[test]
    fn dg2b_install_rotates_an_oversized_launchd_log_between_bootout_and_bootstrap() {
        let (_t, log, ev) = reload_with("dg2b-install-over", crate::logrotate::LOG_MAX_BYTES + 1);
        assert_eq!(
            ev,
            vec!["bootout live=true dot1=false".to_string(), "bootstrap live=false dot1=true".to_string()],
            "the launchd log was not rotated between bootout and bootstrap"
        );
        let one = crate::logrotate::generation_path(&log, 1);
        assert_eq!(std::fs::metadata(&one).unwrap().len(), crate::logrotate::LOG_MAX_BYTES + 1);
    }

    /// Opposition: the same install path leaves a launchd log under the bound
    /// exactly where it is.
    #[test]
    fn dg2b_install_leaves_an_undersized_launchd_log_alone() {
        let (_t, log, ev) = reload_with("dg2b-install-under", 100);
        assert_eq!(
            ev,
            vec!["bootout live=true dot1=false".to_string(), "bootstrap live=true dot1=false".to_string()],
            "an under-bound launchd log was rotated"
        );
        assert_eq!(std::fs::metadata(&log).unwrap().len(), 100);
    }
}

#[cfg(test)]
mod rel6a5_launchctl {
    /// A cargo test harness is isolated, so the mutating launchctl is
    /// refused before a `Command` exists — nothing here can run it.
    #[test]
    fn rel6a5_mutating_launchctl_is_refused_in_an_isolated_process() {
        assert!(env::egress::process_isolation().is_some(), "sentinel: this test binary is isolated");
        for verb in ["bootstrap", "bootout"] {
            let e = super::launchctl_control(verb).expect_err("a launchctl Command was handed out under isolation");
            assert!(e.to_string().contains("refused"), "{e}");
        }
    }
}

#[cfg(test)]
mod rel4_install_tests {
    //! REL-4.S3: an install carries no personal path and no personal label.
    //! Each guard is paired with its opposite (default vs override, legacy
    //! kept vs not, HOME planted vs absent), so a constant answer fails one.
    use super::*;
    use env::paths::Source;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tmp(tag: &str) -> Tmp {
        let t = std::env::temp_dir().join(format!("cauldron-rel4s3-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        Tmp(t)
    }
    fn env_at(root: &Path, name: &str) -> Environment {
        Environment {
            root: root.to_path_buf(),
            root_source: Source::CauldronHome,
            name: name.to_string(),
            name_source: Source::Flag,
            data_root: root.join("data").join(name),
        }
    }

    #[test]
    fn rel4_label_default_is_neutral_and_config_reproduces_the_operators_labels() {
        // Default: the product's namespace, nobody's personal one.
        let d = resolve_label_prefix(None, false);
        assert_eq!(d, "ai.broomstick");
        assert_eq!(label_with(&d, "prod"), "ai.broomstick.cauldron");
        assert_eq!(label_with(&d, "dev"), "ai.broomstick.cauldron-dev");
        // Derived, never a literal: the building user's own login name.
        let user = std::env::var("USER").unwrap_or_default();
        for personal in [user.as_str(), "com.broomstick"].into_iter().filter(|s| s.len() > 2) {
            assert!(!label_with(&d, "prod").contains(personal), "default label carries `{personal}`");
        }
        // Override reproduces the operator's live label exactly…
        let his = resolve_label_prefix(Some("com.broomstick"), false);
        assert_eq!(label_with(&his, "prod"), "com.broomstick.cauldron");
        assert_eq!(label_with(&his, "stage"), "com.broomstick.cauldron-stage");
        // …and any other prefix he names, e.g. a net.example.* family.
        assert_eq!(label_with(&resolve_label_prefix(Some("net.example"), false), "prod"), "net.example.cauldron");
        // An existing legacy job keeps its name with NO config at all…
        assert_eq!(resolve_label_prefix(None, true), LEGACY_LABEL_PREFIX);
        // …and explicit config still beats it.
        assert_eq!(resolve_label_prefix(Some("org.example"), true), "org.example");
    }

    #[test]
    fn rel4_label_reads_label_prefix_from_the_environments_own_env_toml() {
        let t = tmp("cfg");
        let root = t.0.join(".cauldron");
        // A name no real LaunchAgents dir holds, so the legacy arm cannot fire.
        let name = format!("rel4s3-{}", std::process::id());
        let e = env_at(&root, &name);
        std::fs::create_dir_all(&e.data_root).unwrap();
        assert_eq!(label(&e), format!("ai.broomstick.cauldron-{name}"), "no env.toml: the default");
        std::fs::write(e.manifest(), "[supervisor]\nlabel_prefix = \"com.broomstick\"\n").unwrap();
        assert_eq!(label(&e), format!("com.broomstick.cauldron-{name}"), "env.toml's prefix was not used");
        // A save (a rollback re-pin) keeps the key; an unset key is not invented.
        let m = Manifest::load(&e.manifest()).unwrap();
        m.save(&e.manifest()).unwrap();
        assert!(std::fs::read_to_string(e.manifest()).unwrap().contains("label_prefix = \"com.broomstick\""));
        Manifest::default().save(&e.manifest()).unwrap();
        assert!(!std::fs::read_to_string(e.manifest()).unwrap().contains("label_prefix"));
    }

    #[test]
    fn rel4_label_prefix_that_is_not_reverse_dns_is_refused_and_one_that_is_loads() {
        let t = tmp("bad");
        let p = t.0.join("env.toml");
        for bad in ["", "com..broomstick", "com.broom stick", "com/broomstick", ".com", "com.\"x\""] {
            std::fs::write(&p, format!("[supervisor]\nlabel_prefix = {bad:?}\n")).unwrap();
            let err = Manifest::load(&p).err().unwrap_or_else(|| panic!("`{bad}` was accepted"));
            assert!(format!("{err:#}").contains("label_prefix"), "{err:#}");
        }
        for good in ["ai.broomstick", "com.broomstick", "net.example", "a-b.c1"] {
            std::fs::write(&p, format!("[supervisor]\nlabel_prefix = {good:?}\n")).unwrap();
            assert_eq!(Manifest::load(&p).unwrap().supervisor.label_prefix.as_deref(), Some(good));
        }
    }

    #[test]
    fn rel4_install_refuses_a_second_job_for_the_same_environment_and_allows_its_own() {
        let t = tmp("clash");
        let agents = t.0.join("LaunchAgents");
        std::fs::create_dir_all(&agents).unwrap();
        assert!(conflicting_plists(&agents, "prod", "ai.broomstick.cauldron").is_empty(), "empty dir");
        std::fs::write(agents.join("com.broomstick.cauldron.plist"), "<plist/>").unwrap();
        assert_eq!(
            conflicting_plists(&agents, "prod", "ai.broomstick.cauldron"),
            vec![agents.join("com.broomstick.cauldron.plist")],
            "a legacy job under the default label was not seen"
        );
        assert!(conflicting_plists(&agents, "prod", "com.broomstick.cauldron").is_empty(), "its OWN job is not a clash");
        assert!(conflicting_plists(&agents, "dev", "ai.broomstick.cauldron-dev").is_empty(), "another env's job is not a clash");
    }

    /// REL-4.S3b: the legacy label survives its plist. Paired: legacy plist
    /// present (records the legacy prefix), absent (records the default),
    /// already configured (writes nothing) — so neither a constant prefix nor
    /// a no-op passes all three.
    #[test]
    fn rel4b_the_resolved_prefix_is_persisted_so_the_legacy_label_survives_its_plist() {
        let t = tmp("persist");
        let agents = t.0.join("LaunchAgents");
        std::fs::create_dir_all(&agents).unwrap();
        let root = t.0.join(".cauldron");

        // 1. His case: a legacy job, no key. Uninstall persists, then deletes the plist.
        let e = env_at(&root, "prod");
        std::fs::create_dir_all(&e.data_root).unwrap();
        let legacy = agents.join("com.broomstick.cauldron.plist");
        std::fs::write(&legacy, "<plist/>").unwrap();
        assert_eq!(label_prefix_in(&e, Some(&agents)), "com.broomstick", "legacy arm did not fire");
        assert_eq!(persist_label_prefix_in(&e, Some(&agents)).unwrap().as_deref(), Some("com.broomstick"));
        std::fs::remove_file(&legacy).unwrap();
        assert_eq!(
            label_with(&label_prefix_in(&e, Some(&agents)), "prod"),
            "com.broomstick.cauldron",
            "with its plist gone the job was renamed"
        );
        assert_eq!(
            Manifest::load(&e.manifest()).unwrap().supervisor.label_prefix.as_deref(),
            Some("com.broomstick")
        );

        // 2. No legacy job: the default is recorded (not the legacy one).
        let d = env_at(&root, "dev");
        std::fs::create_dir_all(&d.data_root).unwrap();
        assert_eq!(persist_label_prefix_in(&d, Some(&agents)).unwrap().as_deref(), Some("ai.broomstick"));
        assert_eq!(label_prefix_in(&d, Some(&agents)), "ai.broomstick");

        // 3. Already configured: nothing is written, the file is byte-identical.
        let c = env_at(&root, "stage");
        std::fs::create_dir_all(&c.data_root).unwrap();
        std::fs::write(c.manifest(), "[supervisor]\nlabel_prefix = \"org.example\"\n").unwrap();
        std::fs::write(agents.join("com.broomstick.cauldron-stage.plist"), "<plist/>").unwrap();
        let before = std::fs::read(c.manifest()).unwrap();
        assert_eq!(persist_label_prefix_in(&c, Some(&agents)).unwrap(), None);
        assert_eq!(std::fs::read(c.manifest()).unwrap(), before, "a configured env.toml was rewritten");
    }

    /// The home of whoever runs the test, DERIVED (never a literal): the
    /// fallback built for another user must not leak the real HOME into it.
    fn operators_home() -> String {
        std::env::var("HOME").ok().filter(|h| h.len() > 1 && h != "/Users/alice").unwrap_or_else(|| "/nonexistent-home".into())
    }

    #[test]
    fn rel4_path_fallback_is_derived_from_home_and_names_no_other_user() {
        let alice = fallback_path(Some("/Users/alice"), None);
        assert!(alice.starts_with("/Users/alice/.cargo/bin:"), "{alice}");
        assert!(!alice.contains(&operators_home()), "a literal user path is in the fallback: {alice}");
        assert_eq!(alice.matches("/Users/").count(), 1, "a user path other than HOME's: {alice}");
        // No HOME: no user directory of anybody's.
        let none = fallback_path(None, None);
        assert!(!none.contains("/Users/"), "a user path with no HOME: {none}");
        assert!(none.split(':').any(|d| d == "/usr/bin"), "{none}");
        // CARGO_HOME (rustup's own override) wins over HOME.
        let ch = fallback_path(Some("/Users/alice"), Some("/opt/rust/cargo"));
        assert!(ch.starts_with("/opt/rust/cargo/bin:"), "{ch}");
        assert!(!ch.contains("/Users/"), "{ch}");
    }

    // ── REL-4.S3c: `cauldron install`'s running-job guard ─────────────────

    /// A stub launchd: `loaded` answers `print` (by label) and `list`;
    /// `in_list` are jobs only `list` shows; `list_fails` makes `list` error.
    struct StubQuery {
        loaded: Vec<(&'static str, &'static str)>,
        in_list: Vec<(&'static str, &'static str)>,
        list_fails: bool,
    }
    impl LaunchdQuery for StubQuery {
        fn print_pid(&self, target: &str) -> Option<String> {
            let lab = target.rsplit('/').next().unwrap();
            self.loaded.iter().find(|(l, _)| *l == lab).map(|(_, p)| p.to_string())
        }
        fn list(&self) -> Result<String> {
            if self.list_fails {
                bail!("stub: launchctl list failed");
            }
            let mut out = "PID\tStatus\tLabel\n1\t0\tcom.apple.x\n2\t0\torg.example.notcauldron\n3\t0\tx.cauldron-stagex\n4\t0\tx.cauldron\n".to_string();
            for (l, p) in self.loaded.iter().chain(&self.in_list) {
                out.push_str(&format!("{p}\t0\t{l}\n"));
            }
            Ok(out)
        }
    }

    /// A stub `JobControl` that records every call; a real launchctl is
    /// never reached from these tests.
    struct RecJobs {
        loaded: std::cell::Cell<bool>,
        calls: std::cell::RefCell<Vec<&'static str>>,
    }
    impl JobControl for RecJobs {
        fn loaded(&self) -> bool {
            self.loaded.get()
        }
        fn bootout(&self) {
            self.calls.borrow_mut().push("bootout");
            self.loaded.set(false);
        }
        fn bootstrap(&self) -> Result<std::process::ExitStatus> {
            use std::os::unix::process::ExitStatusExt;
            self.calls.borrow_mut().push("bootstrap");
            self.loaded.set(true);
            Ok(std::process::ExitStatus::from_raw(0))
        }
    }

    struct Run {
        rc: i32,
        plist_written: bool,
        binary_written: bool,
        manifest_written: bool,
        calls: Vec<&'static str>,
    }

    /// `install_with` in a throwaway root + LaunchAgents dir, env `stage`.
    fn install_run(tag: &str, q: &StubQuery, opts: InstallOpts) -> Run {
        let t = tmp(tag);
        let root = t.0.join("home").join(".cauldron");
        let agents = t.0.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        let build = t.0.join("build");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(build.join("cauldron"), b"router").unwrap();
        let env = env_at(&root, "stage");
        // The stub job target is loaded iff a job for `stage` is in the stub.
        let mine = q.loaded.iter().chain(&q.in_list).any(|(l, _)| l.ends_with(".cauldron-stage"));
        let jobs = RecJobs { loaded: mine.into(), calls: Default::default() };
        let rc = install_with(&env, opts, &build.join("cauldron"), &agents, "gui/0", q, &jobs).unwrap();
        let plist_written = std::fs::read_dir(&agents).unwrap().next().is_some();
        let run = Run {
            rc,
            plist_written,
            binary_written: launch_path(&env).exists(),
            manifest_written: env.manifest().exists(),
            calls: jobs.calls.borrow().clone(),
        };
        drop(t);
        run
    }

    const REPLACE: InstallOpts = InstallOpts { dry: false, replace_running: true };
    const PLAIN: InstallOpts = InstallOpts { dry: false, replace_running: false };

    /// REL-4.S3c item 3: a REAL `cauldron install` over a loaded job for the
    /// same environment refuses (rc 2) before it writes the env.toml, the
    /// binary or the plist, and before any launchd call. Forced red by
    /// removing the guard call from `install_with`. Opposed by the two
    /// proceeding arms below, so an always-refusing guard fails too.
    #[test]
    fn rel4c_install_refuses_a_loaded_job_for_its_env_before_writing_anything() {
        let q = StubQuery { loaded: vec![("ai.broomstick.cauldron-stage", "4242")], in_list: vec![], list_fails: false };
        let r = install_run("c-refuse", &q, PLAIN);
        assert_eq!(r.rc, 2, "install over a loaded job was not refused");
        assert!(!r.plist_written, "a plist was written over a loaded job");
        assert!(!r.binary_written, "the launch path was written over a loaded job");
        assert!(!r.manifest_written, "env.toml was written over a loaded job");
        assert!(r.calls.is_empty(), "launchd was touched: {:?}", r.calls);
        // A job under ANY other prefix, visible only to `list`, refuses too.
        let q = StubQuery { loaded: vec![], in_list: vec![("org.example.cauldron-stage", "77")], list_fails: false };
        let r = install_run("c-refuse-list", &q, PLAIN);
        assert_eq!(r.rc, 2, "a job under another prefix was not caught");
        assert!(r.calls.is_empty() && !r.plist_written);
    }

    /// Opposition: `--replace-running` over the same loaded job proceeds,
    /// writes, and boots out + bootstraps (the restart it was warned of).
    #[test]
    fn rel4c_install_with_replace_running_proceeds_over_a_loaded_job() {
        let q = StubQuery { loaded: vec![("ai.broomstick.cauldron-stage", "4242")], in_list: vec![], list_fails: false };
        let r = install_run("c-replace", &q, REPLACE);
        assert_eq!(r.rc, 0);
        assert!(r.plist_written && r.binary_written && r.manifest_written);
        assert_eq!(r.calls, vec!["bootout", "bootstrap"]);
    }

    /// Opposition: nothing for THIS env loaded (decoys: prod's `x.cauldron`,
    /// `x.cauldron-stagex`, another env's) — proceeds without the flag.
    #[test]
    fn rel4c_install_proceeds_when_no_job_for_its_env_is_loaded() {
        let q = StubQuery { loaded: vec![("ai.broomstick.cauldron", "5")], in_list: vec![("org.example.cauldron-dev", "6")], list_fails: false };
        let r = install_run("c-none", &q, PLAIN);
        assert_eq!(r.rc, 0, "an install with no job for its env was refused");
        assert!(r.plist_written && r.binary_written);
        assert_eq!(r.calls, vec!["bootstrap"]);
    }

    /// Item 2 (Rust side): a failed `launchctl list` is reported, and the
    /// named prefixes are still checked by `print`.
    #[test]
    fn rel4c_a_failed_list_is_reported_and_named_prefixes_still_checked() {
        let q = StubQuery { loaded: vec![("com.broomstick.cauldron-stage", "9")], in_list: vec![], list_fails: true };
        let f = loaded_jobs_for_env(&q, "gui/0", "stage", None);
        assert!(f.list_failed.as_deref().unwrap_or("").contains("stub: launchctl list failed"), "{f:?}");
        assert_eq!(f.jobs, vec![("com.broomstick.cauldron-stage".to_string(), "9".to_string())]);
        let ok = StubQuery { loaded: vec![], in_list: vec![], list_fails: false };
        let f = loaded_jobs_for_env(&ok, "gui/0", "stage", None);
        assert!(f.list_failed.is_none() && f.jobs.is_empty(), "{f:?}");
        assert_eq!(parse_print_pid("x = {\n\tstate = running\n\tpid = 312\n}\n").as_deref(), Some("312"));
    }

    /// Item 4: `uninstall`'s prefix save is best-effort. An unparseable
    /// env.toml gives `Unrecorded` (and is left byte-identical) instead of an
    /// error that stops the uninstall; a good one is still recorded.
    #[test]
    fn rel4c_uninstall_prefix_save_is_best_effort_on_a_corrupt_env_toml() {
        let t = tmp("c-uninstall");
        let agents = t.0.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("com.broomstick.cauldron-stage.plist"), "<plist/>").unwrap();
        let e = env_at(&t.0.join("root"), "stage");
        std::fs::create_dir_all(&e.data_root).unwrap();
        std::fs::write(e.manifest(), "[supervisor\nthis is not toml").unwrap();
        let before = std::fs::read(e.manifest()).unwrap();
        match record_prefix_for_uninstall(&e, Some(&agents)) {
            UninstallPrefix::Unrecorded(why) => assert!(why.contains("parsing"), "{why}"),
            other => panic!("a corrupt env.toml gave {other:?}"),
        }
        assert_eq!(std::fs::read(e.manifest()).unwrap(), before);
        std::fs::remove_file(e.manifest()).unwrap();
        assert_eq!(
            record_prefix_for_uninstall(&e, Some(&agents)),
            UninstallPrefix::Recorded("com.broomstick".to_string())
        );
    }
}
