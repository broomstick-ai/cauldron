//! **THE ONE WAY A RUST TEST STARTS THE BUILT FACE** (REL-1.S1c).
//!
//! `drive_isolation` checks every shell and python launch of the face, but
//! Rust integration tests that spawned the built binary were outside every
//! isolation check (REL-1.S1b report §7): each was isolated only as far as
//! its author remembered to be — one launched the chat face with `AUI_V2_URL`
//! unset, i.e. at the live ui-server on 9010.
//!
//! So every test that runs the binary goes through [`Face`], and
//! `drive_isolation::every_rust_test_starts_the_face_through_the_helper`
//! fails if any file under `tests/` names the binary anywhere else.
//!
//! [`Face`] starts from the whole isolation set — a scratch HOME and
//! CAULDRON_HOME, opener `none`, TYPE_ONE `off`, port 1 for both threads
//! levers, a dead AUI port, a harmless CAULDRON_BIN, the test-only marker
//! CAULDRON_ISOLATED=1 (REL-6a2), no TMUX, no retired
//! TUI_THREADS_URL — and lets a test override any of it. It does not trust the
//! overrides: [`Face::check`] judges the FINAL environment immediately before
//! every spawn, and a live or missing value panics before a process exists.
//! The names it requires are pinned equal to `drive_isolation`'s required set.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

/// The built face. The only place under `tests/` that names it.
const BIN: &str = env!("CARGO_BIN_EXE_tui");

/// Live listeners on this machine (as `drive_isolation::LIVE_PORTS`).
const LIVE_PORTS: &[u16] = &[3651, 3653, 3753, 9010];

/// The fixed half of the isolation set: (name, isolated default).
pub const FIXED: &[(&str, &str)] = &[
    ("TUI_THREADS_CHASSIS_PORT", "1"),
    ("TUI_THREADS_PORT", "1"),
    ("AUI_V2_URL", "ws://127.0.0.1:1/"),
    ("CAULDRON_BIN", "/usr/bin/false"),
    ("CAULDRON_OPENER", "none"),
    ("CAULDRON_TYPE_ONE", "off"),
    // REL-6a2's test-only isolation MARKER (`cauldron::live::ISOLATED_ENV` on
    // rel/rel6-threads-tab): every test launch says it is one. Not read on
    // this branch; required here so a Face launch carries it when it is.
    (MARKER, "1"),
];

/// The marker's name. Not an address, so `drive_isolation`'s required set
/// gains it only where the source reads it (REL-6a2's LAUNCH_ALSO row).
pub const MARKER: &str = "CAULDRON_ISOLATED";

/// The path half: each defaults to a fresh dir under the test's scratch dir.
pub const PATHS: &[(&str, &str)] = &[("HOME", "home"), ("CAULDRON_HOME", "cauldron")];

/// Never handed to the face (TMUX: terminal negotiation would query his tmux
/// server; TUI_THREADS_URL: retired, nothing reads it).
const DROPPED: &[&str] = &["TMUX", "TUI_THREADS_URL"];

/// First arguments after which the face reads its environment and exits
/// without connecting to anything. Only these may run with a required var
/// deliberately UNSET ([`Face::unset_for_offline_verb`]).
const OFFLINE_VERBS: &[&str] = &["--build-stamp", "--resolve", "--help"];

/// Every name [`Face::check`] requires.
pub fn names() -> Vec<&'static str> {
    FIXED.iter().chain(PATHS).map(|(n, _)| *n).collect()
}

pub struct Face {
    cmd: Command,
    defaults: Vec<(String, OsString)>,
    /// Arguments given to the FACE (not to a wrapper).
    face_args: Vec<String>,
    unset: Vec<&'static str>,
    cleared: bool,
}

impl Face {
    /// `BIN` itself, isolated under `scratch` (a dir the test owns).
    pub fn new(scratch: &Path) -> Face {
        Face::build(Command::new(BIN), scratch)
    }

    /// `program pre_args… BIN`, for a wrapper that execs its last argument
    /// with the environment it was given (`script(1)`, a python pty driver).
    pub fn wrapped(scratch: &Path, program: impl AsRef<OsStr>, pre_args: &[&str]) -> Face {
        let mut c = Command::new(program);
        c.args(pre_args).arg(BIN);
        Face::build(c, scratch)
    }

    fn build(cmd: Command, scratch: &Path) -> Face {
        let mut defaults: Vec<(String, OsString)> =
            FIXED.iter().map(|(k, v)| (k.to_string(), OsString::from(v))).collect();
        for (k, sub) in PATHS {
            let p = scratch.join(sub);
            std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("scratch {}: {e}", p.display()));
            defaults.push((k.to_string(), p.into_os_string()));
        }
        let mut f = Face { cmd, defaults, face_args: Vec::new(), unset: Vec::new(), cleared: false };
        f.apply_defaults();
        f
    }

    fn apply_defaults(&mut self) {
        for (k, v) in &self.defaults {
            self.cmd.env(k, v);
        }
        for k in DROPPED {
            self.cmd.env_remove(k);
        }
    }

    pub fn arg(&mut self, a: impl AsRef<OsStr>) -> &mut Face {
        self.face_args.push(a.as_ref().to_string_lossy().into_owned());
        self.cmd.arg(a);
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Face
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for a in args {
            self.arg(a);
        }
        self
    }

    pub fn env(&mut self, k: impl AsRef<OsStr>, v: impl AsRef<OsStr>) -> &mut Face {
        self.cmd.env(k, v);
        self
    }

    pub fn envs<I, K, V>(&mut self, vars: I) -> &mut Face
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (k, v) in vars {
            self.env(k, v);
        }
        self
    }

    /// Removes a var that is NOT part of the isolation set. Removing one that
    /// is panics: say why with [`Face::unset_for_offline_verb`].
    pub fn env_remove(&mut self, k: &str) -> &mut Face {
        assert!(!names().contains(&k), "Face::env_remove({k}): {k} is isolation; use unset_for_offline_verb");
        self.cmd.env_remove(k);
        self
    }

    /// Nothing inherited from the test process; the isolation set is put back.
    pub fn env_clear(&mut self) -> &mut Face {
        self.cmd.env_clear();
        self.cleared = true;
        self.apply_defaults();
        self
    }

    /// Unset a required var for a verb that connects to nothing (a refusal
    /// probe). [`Face::check`] refuses it unless the face's first argument is
    /// one of [`OFFLINE_VERBS`].
    pub fn unset_for_offline_verb(&mut self, k: &'static str) -> &mut Face {
        assert!(names().contains(&k), "{k} is not an isolation var");
        self.cmd.env_remove(k);
        self.unset.push(k);
        self
    }

    pub fn stdin(&mut self, s: impl Into<Stdio>) -> &mut Face {
        self.cmd.stdin(s);
        self
    }

    pub fn stdout(&mut self, s: impl Into<Stdio>) -> &mut Face {
        self.cmd.stdout(s);
        self
    }

    pub fn stderr(&mut self, s: impl Into<Stdio>) -> &mut Face {
        self.cmd.stderr(s);
        self
    }

    /// Why this environment may not be handed to the face, or Ok. Judged on
    /// the Command's FINAL explicit environment, so it sees every override.
    pub fn check(&self) -> Result<(), String> {
        let envs: BTreeMap<String, Option<String>> = self
            .cmd
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned())))
            .collect();
        let get = |k: &str| envs.get(k).cloned().flatten();
        for k in DROPPED {
            if !self.cleared && envs.get(*k) != Some(&None) {
                return Err(format!("{k} is not removed"));
            }
            if get(k).is_some() {
                return Err(format!("{k} is set"));
            }
        }
        let homes: Vec<PathBuf> =
            ["HOME", "CAULDRON_HOME"].iter().filter_map(|k| get(k)).map(|v| canon(Path::new(&v))).collect();
        for name in names() {
            if self.unset.contains(&name) {
                let first = self.face_args.first().map(String::as_str).unwrap_or("");
                if !OFFLINE_VERBS.contains(&first) {
                    return Err(format!(
                        "{name} is unset, which only an offline verb ({OFFLINE_VERBS:?}) may do; the face's first argument is {first:?}"
                    ));
                }
                if get(name).is_some() {
                    return Err(format!("{name} was unset for an offline verb and then set again"));
                }
                continue;
            }
            let Some(v) = get(name) else {
                return Err(format!("{name} is not set on this launch"));
            };
            let v = v.trim();
            let bad = match name {
                "TUI_THREADS_CHASSIS_PORT" | "TUI_THREADS_PORT" => {
                    !matches!(v.parse::<u16>(), Ok(p) if p != 0 && !LIVE_PORTS.contains(&p))
                }
                "AUI_V2_URL" => !aui_ok(v),
                "CAULDRON_BIN" => v.is_empty() || Path::new(v).file_name().is_some_and(|f| f.to_string_lossy().starts_with("cauldron")),
                "CAULDRON_TYPE_ONE" => v != "off",
                MARKER => v != "1",
                // `none`, or a recorder the test planted in one of its own
                // scratch dirs (opener_guard_0327).
                "CAULDRON_OPENER" => v != "none" && !homes.iter().any(|h| canon(Path::new(v)).starts_with(h)),
                "HOME" | "CAULDRON_HOME" => !scratch_ok(Path::new(v)),
                other => unreachable!("an isolation name with no rule: {other}"),
            };
            if bad {
                return Err(format!("{name}={v:?} is live, empty or not a scratch value"));
            }
        }
        Ok(())
    }

    fn checked(&mut self) -> &mut Command {
        if let Err(why) = self.check() {
            panic!("REFUSED to start the face: {why}");
        }
        &mut self.cmd
    }

    pub fn spawn(&mut self) -> io::Result<Child> {
        self.checked().spawn()
    }

    pub fn output(&mut self) -> io::Result<Output> {
        self.checked().output()
    }
}

/// `p` with symlinks resolved through its nearest existing ancestor, so a
/// path not yet created compares like one that exists (`/var` is
/// `/private/var` on macOS).
fn canon(p: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut at = p;
    loop {
        if let Ok(c) = at.canonicalize() {
            return tail.iter().rev().fold(c, |acc: PathBuf, t| acc.join(t));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                at = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

fn aui_ok(v: &str) -> bool {
    let Some(rest) = v.strip_prefix("ws://127.0.0.1:") else { return false };
    let port: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    matches!(port.parse::<u16>(), Ok(p) if p != 0 && !LIVE_PORTS.contains(&p))
}

/// Absolute, and neither his home, inside it (his store is `~/.cauldron`),
/// nor an ancestor of it.
fn scratch_ok(p: &Path) -> bool {
    if !p.is_absolute() {
        return false;
    }
    let p = canon(p);
    let Some(real) = std::env::var_os("HOME").map(|h| canon(Path::new(&h))) else { return false };
    !(p.starts_with(&real) || real.starts_with(&p))
}
