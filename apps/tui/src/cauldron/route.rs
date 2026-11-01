//! **Where a human action's value goes** — the routing adapter behind a
//! `field` step's Submit (R-V1 item 8: "a routing adapter that sends the
//! value to a graph node, a secret store (macOS keychain) or an env file";
//! product spec §9 supersedes 0217 B2, so this face now TAKES the value).
//!
//! A field names its destination in its fence's `route:` key ([`Route`]);
//! there is NO default destination — a secret field with none is refused
//! (`tickets::validate_action`). The three destinations:
//!
//! - **env** — [`write_env`], a port of the human-actions harness's
//!   `Router.write_value`: the environment's env file (`secrets.env_file`,
//!   an absolute path, per environment; unset refuses), `NAME=value`
//!   upserted, written to a temp file in the same directory and renamed
//!   over it, mode `0600`. Hard gate: the file must be `git check-ignore`d,
//!   or outside every git work tree. Unlike the harness it never adds a
//!   `.gitignore` line on his behalf: a trackable target is refused.
//! - **keychain** — [`Keychain::set`]: a generic password, service
//!   `cauldron.<env>`, account the field's name, updated in place. The
//!   value never travels on an argv (no `security -w`).
//! - **node** (non-secret only, v1) — a dump addressed to the action; that
//!   half lives in `App::ha_submit`, on the existing dump path.
//!
//! A value is a [`Secret`]: no `Display`, a redacted `Debug`, overwritten
//! when dropped. Nothing here puts a value in an error, a notice, a log or
//! an argv; every child process this module spawns is recorded
//! ([`spawned`]) so a test can prove it.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Where a field's value goes, and what an env variable may be called: the
/// soup crate's, so the face reads a fence's `route:` exactly as the soup's
/// write-time check does (V1.8.S2, "one validator, two callers").
pub use soup::actions::{env_name_ok, Route};

/// The keychain service a cauldron's values are kept under.
pub fn keychain_service(env: &str) -> String {
    format!("cauldron.{env}")
}

// ── a value ─────────────────────────────────────────────────────────────────

/// A value he typed: held in memory only, never printed. `Debug` says
/// `<redacted>`; there is no `Display`; dropping it overwrites its bytes.
#[derive(Default)]
pub struct Secret(String);

impl Secret {
    pub fn new() -> Secret {
        Secret(String::new())
    }
    pub fn push(&mut self, c: char) {
        self.0.push(c);
    }
    pub fn push_str(&mut self, s: &str) {
        self.0.push_str(s);
    }
    pub fn pop(&mut self) {
        self.0.pop();
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// How many characters it holds: what the masked input draws as `•`.
    pub fn chars(&self) -> usize {
        self.0.chars().count()
    }
    /// The value itself. For a destination's writer and a non-secret
    /// field's echo only.
    pub fn expose(&self) -> &str {
        &self.0
    }
    /// Overwrite, then empty.
    pub fn wipe(&mut self) {
        let n = self.0.len();
        self.0.replace_range(.., &"\0".repeat(n));
        self.0.clear();
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.wipe();
    }
}

// ── child processes, recorded ───────────────────────────────────────────────

static SPAWNED: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());

/// Every argv this module has spawned, in order (G5/G6: a test proves no
/// value ever rode on one).
pub fn spawned() -> Vec<Vec<String>> {
    SPAWNED.lock().map(|v| v.clone()).unwrap_or_default()
}

/// Run `argv` with no stdin, quietly; its exit code (`None`: did not run).
fn run(argv: &[&str]) -> Option<i32> {
    if let Ok(mut v) = SPAWNED.lock() {
        v.push(argv.iter().map(|s| s.to_string()).collect());
    }
    std::process::Command::new(argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .and_then(|s| s.code())
}

// ── env ─────────────────────────────────────────────────────────────────────

/// The nearest directory at or above `file`'s holding a `.git`: its work
/// tree, or none.
fn repo_root(file: &Path) -> Option<PathBuf> {
    file.parent()?.ancestors().find(|d| d.join(".git").exists()).map(Path::to_path_buf)
}

/// Whether `file` is safe to hold a value: `git check-ignore`d in its work
/// tree, or in no work tree at all.
pub fn safe_target(file: &Path) -> bool {
    match repo_root(file) {
        None => true,
        Some(root) => run(&["git", "-C", &root.to_string_lossy(), "check-ignore", "-q", "--", &file.to_string_lossy()]) == Some(0),
    }
}

/// `NAME=value`, quoted when the value needs it (the harness's
/// `_format_line`).
fn env_line(name: &str, value: &str) -> String {
    let quote = value.is_empty() || value.chars().any(char::is_whitespace) || value.contains(['=', '#', '"']);
    if quote {
        format!("{name}=\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        format!("{name}={value}")
    }
}

/// Whether `line` sets `name` (`NAME=` or `export NAME=`).
fn sets(line: &str, name: &str) -> bool {
    let l = line.strip_prefix("export").filter(|r| r.starts_with([' ', '\t'])).map_or(line, str::trim_start);
    l.strip_prefix(name).is_some_and(|r| r.starts_with('='))
}

/// Upsert `name=value` into the env file `file` (absolute): refused when
/// the file could be tracked by git. The error never holds the value.
pub fn write_env(file: &Path, name: &str, value: &Secret) -> Result<(), String> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if !file.is_absolute() {
        return Err(format!("secrets.env_file must be an absolute path, not {}", file.display()));
    }
    if !env_name_ok(name) {
        return Err(format!("`{name}` cannot be an env variable's name"));
    }
    if value.expose().contains(['\n', '\r']) {
        return Err(format!("{name}: a value is one line"));
    }
    if !safe_target(file) {
        return Err(format!("{} is NOT git-ignored — refusing to write a secret into anything that could become tracked", file.display()));
    }
    let dir = file.parent().ok_or_else(|| format!("{} has no directory", file.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let existing = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    let line = env_line(name, value.expose());
    let mut out: Vec<&str> = Vec::new();
    let mut done = false;
    for l in existing.lines() {
        if sets(l, name) {
            if !done {
                out.push(&line);
            }
            done = true;
            continue;
        }
        out.push(l);
    }
    if !done {
        out.push(&line);
    }
    let mut text = out.join("\n");
    text.push('\n');
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let tmp = dir.join(format!(".cauldron-env.{}.{nanos}", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, file)?;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))
    })();
    // The temp file's text is overwritten by `text` going out of scope only
    // as memory; on disk it is removed if the rename did not take it.
    let _ = std::fs::remove_file(&tmp);
    let mut text = text;
    let n = text.len();
    text.replace_range(.., &"\0".repeat(n));
    written.map_err(|e| format!("{}: {e}", file.display()))
}

// ── keychain ────────────────────────────────────────────────────────────────

/// A secret store a value can be put in. The face holds the system's;
/// a test injects its own (never the login keychain).
pub trait Keychain: Send + Sync {
    /// Store `value` as the generic password `service`/`account`, updating
    /// one that exists. The error never holds the value.
    fn set(&self, service: &str, account: &str, value: &Secret) -> Result<(), String>;
}

/// The machine's keychain, through Security.framework (no argv at all).
/// The face holds [`SystemKeychain::login`]; a test holds
/// [`SystemKeychain::in_file`] on a throwaway keychain file, so it drives
/// THIS code path — the same call, only a different keychain — and never
/// his login keychain (V1.8-I F4: a fake alone left the real path unproven).
#[derive(Default)]
pub struct SystemKeychain {
    #[cfg(target_os = "macos")]
    file: Option<security_framework::os::macos::keychain::SecKeychain>,
}

impl SystemKeychain {
    /// The user's default (login) keychain.
    pub fn login() -> SystemKeychain {
        SystemKeychain::default()
    }

    /// One keychain file the caller opened or created (tests).
    #[cfg(target_os = "macos")]
    pub fn in_file(kc: security_framework::os::macos::keychain::SecKeychain) -> SystemKeychain {
        SystemKeychain { file: Some(kc) }
    }
}

impl Keychain for SystemKeychain {
    #[cfg(target_os = "macos")]
    fn set(&self, service: &str, account: &str, value: &Secret) -> Result<(), String> {
        use security_framework::os::macos::keychain::SecKeychain;
        let kc = match &self.file {
            Some(kc) => kc.clone(),
            None => SecKeychain::default().map_err(|e| format!("the keychain is unavailable for {service}/{account}: {e}"))?,
        };
        // Updates the item in place when it exists, adds it otherwise.
        kc.set_generic_password(service, account, value.expose().as_bytes())
            .map_err(|e| format!("the keychain refused {service}/{account}: {e}"))
    }
    #[cfg(not(target_os = "macos"))]
    fn set(&self, service: &str, account: &str, _value: &Secret) -> Result<(), String> {
        Err(format!("{service}/{account}: there is no keychain on this platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENTINEL: &str = concat!("SENTINEL", "-7f3a");

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("route-unit-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn secret(s: &str) -> Secret {
        let mut v = Secret::new();
        v.push_str(s);
        v
    }

    #[test]
    fn a_route_is_read_from_its_word_and_an_unknown_one_is_kept_as_written() {
        assert_eq!(Route::parse(None, "K"), Route::Unset);
        assert_eq!(Route::parse(Some(""), "K"), Route::Unset);
        assert_eq!(Route::parse(Some("env"), "pay_key"), Route::Env("pay_key".into()));
        assert_eq!(Route::parse(Some("env:RENAMED"), "pay_key"), Route::Env("RENAMED".into()));
        assert_eq!(Route::parse(Some("keychain"), "K"), Route::Keychain);
        assert_eq!(Route::parse(Some("node"), "K"), Route::Node);
        assert_eq!(Route::parse(Some("vault"), "K"), Route::Unknown("vault".into()));
        assert_eq!(Route::parse(Some("env:1BAD"), "K"), Route::Unknown("env:1BAD".into()));
    }

    #[test]
    fn a_secret_debugs_as_redacted_and_wipes() {
        let mut s = secret(SENTINEL);
        assert_eq!(s.chars(), 13);
        let d = format!("{s:?}");
        assert!(!d.contains("7f3a") && d.contains("redacted"), "{d}");
        s.wipe();
        assert!(s.is_empty());
    }

    #[test]
    fn env_upserts_one_line_and_keeps_the_rest() {
        let d = scratch();
        let f = d.join("vals.env");
        std::fs::write(&f, "A=1\nexport K=old\n# note\nK=dup\n").unwrap();
        write_env(&f, "K", &secret("new")).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "A=1\nK=new\n# note\n");
        write_env(&f, "B", &secret("two words")).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "A=1\nK=new\n# note\nB=\"two words\"\n");
        // No temp file left beside it.
        let left: Vec<_> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "{left:?}");
    }

    #[test]
    fn env_refuses_a_relative_path_a_bad_name_and_a_two_line_value_without_saying_the_value() {
        let d = scratch();
        let e = write_env(Path::new("rel.env"), "K", &secret(SENTINEL)).unwrap_err();
        assert!(e.contains("absolute") && !e.contains("7f3a"), "{e}");
        let e = write_env(&d.join("x.env"), "BAD NAME", &secret(SENTINEL)).unwrap_err();
        assert!(!e.contains("7f3a"), "{e}");
        let e = write_env(&d.join("x.env"), "K", &secret(&format!("{SENTINEL}\nX=1"))).unwrap_err();
        assert!(e.contains("one line") && !e.contains("7f3a"), "{e}");
        assert!(!d.join("x.env").exists());
    }

    #[test]
    fn sets_matches_the_name_exactly() {
        assert!(sets("K=1", "K"));
        assert!(sets("export K=1", "K"));
        assert!(!sets("KK=1", "K"));
        assert!(!sets("exportK=1", "K"));
        assert!(!sets("# K=1", "K"));
    }

    /// G5 at the REAL keychain path (V1.8-I F4): `SystemKeychain::set`, the
    /// code the face runs, stores the item — and updates it in place — in a
    /// throwaway keychain file, and no argv this module spawned holds either
    /// value. Sentinel first: the recorder demonstrably sees a spawned
    /// argv, so "no argv holds it" is a measurement and not a blank.
    #[cfg(target_os = "macos")]
    #[test]
    fn g5_the_system_keychain_stores_in_place_and_no_argv_holds_the_value() {
        use security_framework::os::macos::keychain::CreateOptions;
        let d = scratch();
        let kc = CreateOptions::new().password("throwaway-test-keychain").prompt_user(false).create(d.join("t.keychain")).expect("create a throwaway keychain");
        let marker = format!("argv-recorder-marker-{}", std::process::id());
        let _ = run(&["/usr/bin/true", &marker]);
        assert!(spawned().iter().any(|a| a.iter().any(|x| x == &marker)), "the argv recorder saw nothing: this check would measure nothing");
        let (v1, v2) = (format!("{SENTINEL}-one"), format!("{SENTINEL}-two"));
        let k = SystemKeychain::in_file(kc.clone());
        k.set("cauldron.unit", "K", &secret(&v1)).expect("first set");
        assert_eq!(&*kc.find_generic_password("cauldron.unit", "K").expect("stored").0, v1.as_bytes());
        k.set("cauldron.unit", "K", &secret(&v2)).expect("second set updates in place");
        assert_eq!(&*kc.find_generic_password("cauldron.unit", "K").expect("still stored").0, v2.as_bytes());
        for argv in spawned() {
            assert!(!argv.iter().any(|x| x.contains("7f3a")), "a spawned argv holds the value: {argv:?}");
        }
        drop((k, kc));
        let _ = std::fs::remove_dir_all(&d);
    }
}
