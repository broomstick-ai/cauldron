//! **Where the account helper scripts are**, for the login and rotate arms.
//!
//! The login flow drives `add-account` and rotation drives `claude-accounts`
//! rather than restating their credential guards in Rust (see
//! [`crate::login`]). Both are INSTALLED with cauldron, on the prebuilt path
//! and the from-source one alike, at
//! `<cauldron root>/share/cauldron/scripts/threads/<name>` ([`INSTALLED_DIR`]);
//! the daemon learns its root from `CAULDRON_HOME`, which the supervisor
//! hosting it sets, read ONCE at boot ([`adopt_cauldron_home_from_env`]) and
//! never from a path compiled into the binary.
//!
//! The older place, `<data dir>/scripts/<name>` (a hand-made link into a
//! source checkout), is still used when there is no installed copy, so a
//! development home keeps working.
//!
//! Whichever is found runs only if it is TRUSTED ([`check`]): an absolute path
//! to a regular, executable file owned by this user, not writable by group or
//! others, in directories each owned by this user or root and not writable by
//! group or others. So no other local user can plant a script the daemon
//! would run. A script that is missing, or not trusted, is refused with what
//! to do, before anything is spawned.
//!
//! The scripts are told which `threads` to run for their registry steps:
//! `CAULDRON_THREADS_BIN`, this daemon's own executable ([`threads_bin`]),
//! so they never fall back to building one from a source tree.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Where the helpers are installed, under the cauldron root.
pub const INSTALLED_DIR: &str = "share/cauldron/scripts/threads";

/// The environment variable that names the `threads` binary to a helper.
pub const THREADS_BIN_VAR: &str = "CAULDRON_THREADS_BIN";

/// The cauldron root, as adopted at boot. `None`: not hosted by cauldron (or
/// a test), so only the data dir's `scripts/` is looked in.
static CAULDRON_HOME: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Read `CAULDRON_HOME` (absolute, non-empty) into the daemon's state. Called
/// once by `run_daemon`; a library user (a test) that never calls it looks
/// only in the data dir.
pub fn adopt_cauldron_home_from_env() {
    let home = std::env::var_os("CAULDRON_HOME").map(PathBuf::from).filter(|p| p.is_absolute());
    set_cauldron_home(home);
}

/// Set (or clear) the adopted cauldron root.
pub fn set_cauldron_home(home: Option<PathBuf>) {
    *CAULDRON_HOME.write().unwrap_or_else(|e| e.into_inner()) = home;
}

fn cauldron_home() -> Option<PathBuf> {
    CAULDRON_HOME.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The installed path of helper `name` under the cauldron root `root`.
pub fn installed(root: &Path, name: &str) -> PathBuf {
    root.join(INSTALLED_DIR).join(name)
}

/// **The helper `name` the daemon would run**: the installed copy under the
/// adopted cauldron root when it exists, else `<data_dir>/scripts/<name>`
/// when that exists, else the installed path (or, unhosted, the data dir's),
/// for [`check`] to refuse as missing.
pub fn locate(data_dir: &Path, name: &str) -> PathBuf {
    locate_in(cauldron_home().as_deref(), data_dir, name)
}

/// [`locate`] with the cauldron root handed in.
pub fn locate_in(root: Option<&Path>, data_dir: &Path, name: &str) -> PathBuf {
    let legacy = data_dir.join("scripts").join(name);
    match root.map(|r| installed(r, name)) {
        Some(p) if p.exists() => p,
        Some(p) if !legacy.exists() => p,
        _ => legacy,
    }
}

/// The helpers the daemon runs, in the order `/health` lists them.
pub const NAMES: &[&str] = &["add-account", "claude-accounts", "claude-login-browser"];

/// **Where this daemon finds each helper, and whether it would run it**
/// (#32): `/health` carries this under `helpers`, so an upgrade can be checked
/// without starting a login. `cauldron_home` is the root adopted at boot
/// (`null` when not hosted); each script names the path [`locate`] resolves
/// and `trusted`, with the refusal [`check`] would give when it is not.
/// Read-only: a few `stat`s, no script is run.
pub fn report(data_dir: &Path) -> serde_json::Value {
    report_in(cauldron_home().as_deref(), data_dir, current_uid())
}

/// [`report`] with the root and the uid handed in.
pub fn report_in(root: Option<&Path>, data_dir: &Path, uid: u32) -> serde_json::Value {
    let mut scripts = serde_json::Map::new();
    for name in NAMES {
        let path = locate_in(root, data_dir, name);
        let entry = match check_as(&path, name, uid) {
            Ok(()) => serde_json::json!({ "path": path, "trusted": true }),
            Err(why) => serde_json::json!({ "path": path, "trusted": false, "refused": why }),
        };
        scripts.insert((*name).to_string(), entry);
    }
    serde_json::json!({ "cauldron_home": root, "scripts": scripts })
}

/// **May `script` be run?** `Ok` when it is trusted (module docs); otherwise
/// the refusal, saying what to do. `what` names it in the message ("the
/// account login script").
pub fn check(script: &Path, what: &str) -> Result<(), String> {
    check_as(script, what, current_uid())
}

/// [`check`] for `uid`.
pub fn check_as(script: &Path, what: &str, uid: u32) -> Result<(), String> {
    if !script.exists() {
        return Err(format!(
            "{what} is not at {} — it is installed with cauldron, at <cauldron root>/{INSTALLED_DIR}/; \
             install or upgrade with `scripts/install --replace-running` (from a clone) or \
             the release bundle's `install --prebuilt <tarball> --replace-running`, which lays it down",
            script.display()
        ));
    }
    trusted(script, uid).map_err(|why| format!("{what} at {} is not run: {why}", script.display()))
}

fn trusted(path: &Path, uid: u32) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("it is not an absolute path".into());
    }
    let canonical = std::fs::canonicalize(path).map_err(|e| format!("it does not resolve ({e})"))?;
    let meta = std::fs::metadata(&canonical).map_err(|e| format!("it cannot be read ({e})"))?;
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file", canonical.display()));
    }
    if meta.uid() != uid {
        return Err(format!("{} is owned by uid {}, not by this user", canonical.display(), meta.uid()));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!("{} is writable by group or others (mode {:o})", canonical.display(), meta.mode() & 0o7777));
    }
    if meta.mode() & 0o100 == 0 {
        return Err(format!("{} is not executable", canonical.display()));
    }
    for dir in path.ancestors().skip(1).chain(canonical.ancestors().skip(1)) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let m = std::fs::metadata(dir).map_err(|e| format!("{} cannot be read ({e})", dir.display()))?;
        if m.uid() != uid && m.uid() != 0 {
            return Err(format!("{} is owned by uid {}, not by this user or root", dir.display(), m.uid()));
        }
        if m.mode() & 0o022 != 0 {
            return Err(format!(
                "{} is writable by group or others (mode {:o}), so someone else could replace what is in it",
                dir.display(),
                m.mode() & 0o7777
            ));
        }
    }
    Ok(())
}

fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

/// Variables a helper never inherits: each one makes a shell source a file,
/// a lookup start somewhere else, or an interpreter load code from a path
/// the caller chose. (The helpers also run every python with `-I`, which
/// ignores the `PYTHON*` ones; they are removed here as well.)
pub const SCRUBBED_VARS: &[&str] = &[
    "BASH_ENV", "ENV", "IFS", "CDPATH", "GLOBIGNORE", "SHELLOPTS", "BASHOPTS", "PS4", "PROMPT_COMMAND",
    "PERL5LIB", "PERL5OPT", "PERLLIB", "RUBYOPT", "RUBYLIB", "NODE_OPTIONS", "NODE_PATH",
];
/// Prefixes of variables a helper never inherits: exported bash functions,
/// every `PYTHON*` setting, and dynamic-loader injection.
pub const SCRUBBED_PREFIXES: &[&str] = &["BASH_FUNC_", "PYTHON", "DYLD_", "LD_"];
/// The PATH a helper gets when the caller's has no absolute entry.
pub const FALLBACK_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Which of `names` a helper must not inherit ([`SCRUBBED_VARS`],
/// [`SCRUBBED_PREFIXES`]).
pub fn scrubbed_names<I: IntoIterator<Item = std::ffi::OsString>>(names: I) -> Vec<std::ffi::OsString> {
    names
        .into_iter()
        .filter(|n| {
            let s = n.to_string_lossy();
            SCRUBBED_VARS.contains(&s.as_ref()) || SCRUBBED_PREFIXES.iter().any(|p| s.starts_with(p))
        })
        .collect()
}

/// `path` with only its absolute entries: an empty entry or `.` would find a
/// program in the helper's working directory. [`FALLBACK_PATH`] when none is
/// left.
pub fn absolute_path_entries(path: Option<&std::ffi::OsStr>) -> std::ffi::OsString {
    let kept: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(p).filter(|d| d.is_absolute()).collect())
        .unwrap_or_default();
    if kept.is_empty() {
        return FALLBACK_PATH.into();
    }
    std::env::join_paths(kept).unwrap_or_else(|_| FALLBACK_PATH.into())
}

/// **How every helper is started**: from `dir` (the threads home, never the
/// directory the daemon was started from), with the variables of
/// [`scrubbed_names`] removed and a PATH of absolute entries only. The
/// helper itself is a path [`check`] has admitted.
pub fn harden(cmd: &mut tokio::process::Command, dir: &Path) {
    cmd.current_dir(dir);
    for name in scrubbed_names(std::env::vars_os().map(|(k, _)| k)) {
        cmd.env_remove(name);
    }
    cmd.env("PATH", absolute_path_entries(std::env::var_os("PATH").as_deref()));
}

/// This daemon's own executable, every link resolved: the `threads` a helper
/// runs for its registry steps (`CAULDRON_THREADS_BIN`).
pub fn threads_bin() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&exe).unwrap_or(exe))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(at: &Path) {
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A tempdir is 0700 and its parents are the system's: trusted.
    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn health_reports_each_helper_with_its_path_and_whether_it_is_trusted() {
        let t = tmp();
        let (root, data) = (t.path().join("root"), t.path().join("data"));
        std::fs::create_dir_all(&data).unwrap();
        script(&installed(&root, "add-account"));
        let r = report_in(Some(&root), &data, current_uid());
        assert_eq!(r["cauldron_home"], serde_json::json!(root));
        let a = &r["scripts"]["add-account"];
        assert_eq!(a["path"], serde_json::json!(installed(&root, "add-account")));
        assert_eq!(a["trusted"], true, "{r}");
        assert!(a.get("refused").is_none(), "{r}");
        let c = &r["scripts"]["claude-accounts"];
        assert_eq!(c["trusted"], false, "{r}");
        assert!(c["refused"].as_str().unwrap().contains("is not at"), "{r}");
        assert_eq!(r["scripts"].as_object().unwrap().len(), NAMES.len());
        // Unhosted: no root, the data dir's scripts/ is what is named.
        let u = report_in(None, &data, current_uid());
        assert!(u["cauldron_home"].is_null());
        assert_eq!(u["scripts"]["add-account"]["path"], serde_json::json!(data.join("scripts/add-account")));
    }

    #[test]
    fn the_installed_copy_is_found_first_then_the_data_dir_then_refused_as_missing() {
        let t = tmp();
        let (root, data) = (t.path().join("root"), t.path().join("data"));
        std::fs::create_dir_all(&data).unwrap();
        // Neither: the installed path, refused as missing with what to do.
        let p = locate_in(Some(&root), &data, "add-account");
        assert_eq!(p, root.join("share/cauldron/scripts/threads/add-account"));
        let e = check(&p, "the account login script").unwrap_err();
        assert!(e.contains("is not at") && e.contains("--replace-running"), "{e}");
        // Only the data dir's (a development home): that one.
        script(&data.join("scripts/add-account"));
        assert_eq!(locate_in(Some(&root), &data, "add-account"), data.join("scripts/add-account"));
        // Both: the installed copy wins.
        script(&installed(&root, "add-account"));
        let p = locate_in(Some(&root), &data, "add-account");
        assert_eq!(p, installed(&root, "add-account"));
        check(&p, "the account login script").unwrap();
        // Not hosted: only the data dir.
        assert_eq!(locate_in(None, &data, "add-account"), data.join("scripts/add-account"));
    }

    #[test]
    fn a_helper_starts_in_the_named_directory_with_a_scrubbed_environment() {
        let names: Vec<std::ffi::OsString> =
            ["PYTHONPATH", "PYTHONSTARTUP", "PYTHONHOME", "BASH_ENV", "ENV", "IFS", "BASH_FUNC_x%%", "DYLD_INSERT_LIBRARIES", "HOME", "CLAUDE_ACCOUNTS_HOME", "PATH"]
                .iter()
                .map(|s| s.into())
                .collect();
        let gone = scrubbed_names(names);
        for n in ["PYTHONPATH", "PYTHONSTARTUP", "PYTHONHOME", "BASH_ENV", "ENV", "IFS", "BASH_FUNC_x%%", "DYLD_INSERT_LIBRARIES"] {
            assert!(gone.iter().any(|g| g == n), "{n} is inherited");
        }
        for n in ["HOME", "CLAUDE_ACCOUNTS_HOME", "PATH"] {
            assert!(!gone.iter().any(|g| g == n), "{n} is removed");
        }
        assert_eq!(absolute_path_entries(Some(".:/x/bin::rel/bin:/usr/bin".as_ref())), "/x/bin:/usr/bin");
        assert_eq!(absolute_path_entries(Some(".:".as_ref())), FALLBACK_PATH);
        assert_eq!(absolute_path_entries(None), FALLBACK_PATH);
        let t = tmp();
        let mut cmd = tokio::process::Command::new("/bin/true");
        harden(&mut cmd, t.path());
        assert_eq!(cmd.as_std().get_current_dir(), Some(t.path()));
        let path = cmd.as_std().get_envs().find(|(k, _)| *k == "PATH").and_then(|(_, v)| v).unwrap();
        assert!(std::env::split_paths(path).all(|d| d.is_absolute()));
    }

    #[test]
    fn a_script_someone_else_could_replace_is_refused() {
        let t = tmp();
        let root = t.path().join("root");
        let p = installed(&root, "claude-accounts");
        script(&p);
        check(&p, "x").unwrap();
        // Group-writable file.
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(check(&p, "x").unwrap_err().contains("writable by group or others"));
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A world-writable directory above it.
        let dir = root.join("share/cauldron/scripts");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(check(&p, "x").unwrap_err().contains("someone else could replace"), "dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Not executable.
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(check(&p, "x").unwrap_err().contains("not executable"));
        // Owned by someone else (judged as another uid).
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(check_as(&p, "x", current_uid() + 1).unwrap_err().contains("owned by uid"));
        // A link to a file elsewhere is judged at its target too.
        let other = t.path().join("elsewhere/script");
        script(&other);
        std::fs::set_permissions(other.parent().unwrap(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let link = root.join("share/cauldron/scripts/threads/linked");
        std::os::unix::fs::symlink(&other, &link).unwrap();
        assert!(check(&link, "x").is_err(), "the target's directory is world-writable");
        std::fs::set_permissions(other.parent().unwrap(), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}
