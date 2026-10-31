//! **WHICH `cauldron` THE `cauldron` ON PATH RUNS** (R9.CFG1, implements
//! E18-19, operator ruling E21-7).
//!
//! The `cauldron` on the operator's PATH is a launcher, not the CLI itself.
//! It reads ONE key out of the cauldron root's own config file and execs the
//! binary that key names, with the same argv:
//!
//! ```toml
//! # <root>/config.toml   (root = $CAULDRON_HOME, else $HOME/.cauldron)
//! [cli]
//! interface = "globs2"                      # a named build, or
//! interface = "/abs/path/to/cauldron"       # an absolute path
//! ```
//!
//! Named builds:
//!
//! | name     | binary                                                   |
//! |----------|----------------------------------------------------------|
//! | `globs2` | `/Users/testuser/.local/share/cauldron-tui/bin/cauldron` (the operator's demo; what the `/Users/testuser/bin/cauldron` shell shim execs) |
//! | `prod`   | `<root>/bin/cauldron` (the prod holder)                  |
//!
//! **The default is today's behaviour.** No file, or a file with no
//! `[cli] interface`, resolves to [`DEFAULT_INTERFACE`] — the globs2 demo
//! build, which is what `/Users/testuser/bin/cauldron` exec'd before this
//! existed. Changing the default is the operator's call, not a code change
//! that happens by accident: it is one constant, pinned by a test that spells
//! the path out literally.
//!
//! **Every refusal names the key.** A value that is relative, names nothing,
//! names a directory, or names a file that is not executable is refused with
//! `[cli] interface` and the file it came from in the message, so the operator
//! fixes the config rather than guessing which of three launchers broke.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// The config file's name inside the cauldron root.
pub const CONFIG_FILE: &str = "config.toml";
/// The key, spelled the way a human reads it in an error.
pub const KEY: &str = "[cli] interface";
/// The named build the default resolves to.
pub const DEFAULT_INTERFACE: &str = "globs2";
/// The globs2 demo build — exactly what the `/Users/testuser/bin/cauldron`
/// shell shim execs today (its `BIN=` dir + `/cauldron`). Pinned to the live
/// shim by the test `default_equals_what_the_path_shim_execs_today`.
pub const GLOBS2_BINARY: &str = "/Users/testuser/.local/share/cauldron-tui/bin/cauldron";

#[derive(Debug, Default, Deserialize)]
struct File {
    #[serde(default)]
    cli: Cli,
}

#[derive(Debug, Default, Deserialize)]
struct Cli {
    interface: Option<String>,
}

/// Where the resolved binary came from — printed by the launcher's
/// `CAULDRON_LAUNCHER_WHICH=1` so a wrong pick is visible, not silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// No config file at all.
    DefaultNoFile,
    /// A config file without `[cli] interface`.
    DefaultNoKey,
    /// The key was set, to the given raw value.
    Key(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub binary: PathBuf,
    pub origin: Origin,
    pub config: PathBuf,
}

/// The cauldron root, resolved exactly as [`crate::paths`] does it
/// (`$CAULDRON_HOME` if set and non-empty, else `$HOME/.cauldron`) but
/// WITHOUT the production guard: the launcher only reads one file, and the
/// guard's job is to keep writers off production's root.
pub fn root_from_env() -> Result<PathBuf> {
    let nonempty = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    if let Some(h) = nonempty("CAULDRON_HOME") {
        return Ok(PathBuf::from(h));
    }
    let home = nonempty("HOME")
        .context("neither $CAULDRON_HOME nor $HOME is set, so there is no cauldron root to read")?;
    Ok(Path::new(&home).join(".cauldron"))
}

/// The binary a named build stands for, or `None` if `name` is not one.
fn named(name: &str, root: &Path) -> Option<PathBuf> {
    match name {
        "globs2" => Some(PathBuf::from(GLOBS2_BINARY)),
        "prod" => Some(root.join("bin").join("cauldron")),
        _ => None,
    }
}

/// Which binary the config CHOOSES — the key, or the default — before
/// anything checks that the binary can be run. Split from [`resolve`] so the
/// default is testable on a machine where the globs2 build does not exist.
pub fn choose(root: &Path) -> Result<Interface> {
    let config = root.join(CONFIG_FILE);
    let (raw, origin) = match std::fs::read_to_string(&config) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (DEFAULT_INTERFACE.to_string(), Origin::DefaultNoFile)
        }
        Err(e) => bail!("cannot read {} (which holds {KEY}): {e}", config.display()),
        Ok(text) => {
            let f: File = toml::from_str(&text)
                .with_context(|| format!("parsing {} (which holds {KEY})", config.display()))?;
            match f.cli.interface {
                None => (DEFAULT_INTERFACE.to_string(), Origin::DefaultNoKey),
                Some(v) => (v.clone(), Origin::Key(v)),
            }
        }
    };
    let binary = match named(&raw, root) {
        Some(p) => p,
        None => {
            let p = PathBuf::from(&raw);
            if !p.is_absolute() {
                bail!(
                    "{} is `{raw}`, which is neither a named build (globs2, prod) nor an \
                     absolute path. Set it to one of those.",
                    whence(&origin, &config)
                );
            }
            p
        }
    };
    Ok(Interface { binary, origin, config })
}

fn whence(origin: &Origin, config: &Path) -> String {
    match origin {
        Origin::Key(_) => format!("{KEY} in {}", config.display()),
        _ => format!("the built-in default for {KEY} (not set in {})", config.display()),
    }
}

/// [`choose`], then [`runnable`].
pub fn resolve(root: &Path) -> Result<Interface> {
    runnable(choose(root)?)
}

/// Refuse a chosen binary that cannot be run — naming the key, and saying
/// whether the value was the operator's or the built-in default.
pub fn runnable(i: Interface) -> Result<Interface> {
    let raw = match &i.origin {
        Origin::Key(v) => v.clone(),
        _ => DEFAULT_INTERFACE.to_string(),
    };
    let w = whence(&i.origin, &i.config);
    let b = i.binary.display();
    let meta = std::fs::metadata(&i.binary).map_err(|e| {
        anyhow::anyhow!(
            "{w} is `{raw}`, which resolves to {b} — and that cannot be run: {e}. \
             Fix {KEY} or build that binary."
        )
    })?;
    if !meta.is_file() {
        bail!("{w} is `{raw}`, which resolves to {b} — not a file. Fix {KEY}.");
    }
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            bail!("{w} is `{raw}`, which resolves to {b} — not executable. Fix {KEY}.");
        }
    }
    Ok(i)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("cauldron-cfg1-{tag}-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn exe(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The default must be today's PATH `cauldron` — spelled out literally,
    /// not read back from the constant, so changing the default fails here.
    #[test]
    fn default_is_the_globs2_demo_build() {
        assert_eq!(DEFAULT_INTERFACE, "globs2");
        assert_eq!(
            named(DEFAULT_INTERFACE, Path::new("/nowhere")).unwrap(),
            PathBuf::from("/Users/testuser/.local/share/cauldron-tui/bin/cauldron")
        );
    }

    /// The shell shim on PATH that this launcher is meant to replace. Read,
    /// never written.
    const PATH_SHIM: &str = "/Users/testuser/bin/cauldron";

    /// What a `/bin/sh` shim execs: the first word of its LAST `exec` line,
    /// with `$NAME` / `${NAME}` expanded from plain `NAME=value` lines above it.
    fn shim_exec_target(text: &str) -> Option<PathBuf> {
        let mut vars: Vec<(String, String)> = Vec::new();
        let expand = |s: &str, vars: &[(String, String)]| {
            let mut out = s.to_string();
            for (k, v) in vars.iter().rev() {
                out = out.replace(&format!("${{{k}}}"), v).replace(&format!("${k}"), v);
            }
            out
        };
        let unquote = |s: &str| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        let mut target = None;
        for line in text.lines().map(str::trim) {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("exec ") {
                let rest = rest.trim_start();
                let word = match rest.strip_prefix('"') {
                    Some(q) => q.split('"').next().unwrap_or(""),
                    None => rest.split_whitespace().next().unwrap_or(""),
                };
                target = Some(PathBuf::from(expand(word, &vars)));
            } else if let Some((k, v)) = line.split_once('=') {
                if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    let v = expand(&unquote(v), &vars);
                    vars.push((k.to_string(), v));
                }
            }
        }
        target
    }

    /// Ok when the built-in default names exactly what `shim` execs.
    fn default_matches_shim(shim: &str) -> std::result::Result<(), String> {
        let target = shim_exec_target(shim).ok_or("no `exec` target could be read from the shim")?;
        let default = named(DEFAULT_INTERFACE, Path::new("/nowhere")).unwrap();
        if default == target {
            Ok(())
        } else {
            Err(format!(
                "the built-in default for {KEY} is {} but the shim execs {} — the default has \
                 drifted from today's PATH `cauldron`; update GLOBS2_BINARY",
                default.display(),
                target.display()
            ))
        }
    }

    /// THE DRIFT GUARD (inspect r36): the default must equal what the live
    /// PATH shim execs TODAY, read from the shim itself, so the next time the
    /// shim moves this goes red instead of the default silently going stale.
    /// Skips, saying why, when there is no shell shim to compare against.
    #[test]
    fn default_equals_what_the_path_shim_execs_today() {
        let bytes = match std::fs::read(PATH_SHIM) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("SKIP default_equals_what_the_path_shim_execs_today: cannot read {PATH_SHIM} ({e}), so there is no shim to compare the default against");
                return;
            }
        };
        let text = match String::from_utf8(bytes) {
            Ok(t) if t.starts_with("#!") => t,
            _ => {
                eprintln!("SKIP default_equals_what_the_path_shim_execs_today: {PATH_SHIM} is not a shell script (the launcher itself may be installed there now), so there is no shim to compare the default against");
                return;
            }
        };
        if let Err(m) = default_matches_shim(&text) {
            panic!("{m}\n--- {PATH_SHIM} ---\n{text}");
        }
    }

    /// The comparator is not vacuous: a shim that execs anything else is
    /// refused, in both shapes the shim has had (a `BIN=` variable, and a
    /// literal path).
    #[test]
    fn a_shim_that_execs_another_build_is_refused() {
        let var_shape = "#!/bin/sh\n# c\nBIN=/opt/elsewhere/release\nexec \"$BIN/cauldron\" \"$@\"\n";
        let lit_shape = "#!/bin/sh\nexec /Users/testuser/code/cauldron-wt-globs2/target/release/cauldron \"$@\"\n";
        assert_eq!(shim_exec_target(var_shape), Some(PathBuf::from("/opt/elsewhere/release/cauldron")));
        for s in [var_shape, lit_shape] {
            let m = default_matches_shim(s).expect_err(s);
            assert!(m.contains(KEY) && m.contains("drifted"), "{m}");
        }
        assert!(default_matches_shim("#!/bin/sh\necho no exec here\n").is_err());
    }

    /// And a shim of today's shape that execs the default is accepted.
    #[test]
    fn a_shim_that_execs_the_default_is_accepted() {
        let s = format!(
            "#!/bin/sh\nBIN={}\nfor b in cauldron tui; do :; done\nexec \"${{BIN}}/cauldron\" \"$@\"\n",
            Path::new(GLOBS2_BINARY).parent().unwrap().display()
        );
        assert_eq!(default_matches_shim(&s), Ok(()));
    }

    #[test]
    fn set_key_absolute_path_resolves_to_that_binary() {
        let root = scratch("abs");
        let bin = root.join("alt").join("cauldron");
        exe(&bin);
        std::fs::write(root.join(CONFIG_FILE), format!("[cli]\ninterface = {:?}\n", bin.to_str().unwrap())).unwrap();
        let i = resolve(&root).unwrap();
        assert_eq!(i.binary, bin);
        assert_eq!(i.origin, Origin::Key(bin.to_str().unwrap().into()));
    }

    #[test]
    fn set_key_named_prod_resolves_under_the_root() {
        let root = scratch("prod");
        exe(&root.join("bin").join("cauldron"));
        std::fs::write(root.join(CONFIG_FILE), "[cli]\ninterface = \"prod\"\n").unwrap();
        assert_eq!(resolve(&root).unwrap().binary, root.join("bin").join("cauldron"));
    }

    /// Absent file, absent section and absent key all take the default.
    #[test]
    fn absent_file_and_absent_key_take_the_default() {
        for (tag, file, want) in [
            ("nofile", None, Origin::DefaultNoFile),
            ("nokey", Some("[cli]\n# nothing\n"), Origin::DefaultNoKey),
            ("nosect", Some("[other]\nx = 1\n"), Origin::DefaultNoKey),
        ] {
            let root = scratch(tag);
            if let Some(t) = file {
                std::fs::write(root.join(CONFIG_FILE), t).unwrap();
            }
            let i = choose(&root).unwrap();
            assert_eq!(i.origin, want, "{tag}");
            assert_eq!(i.binary, PathBuf::from(GLOBS2_BINARY), "{tag}");
        }
    }

    /// A default whose binary is missing is refused like any value, and the
    /// message says it was the DEFAULT, so the fix (set the key) is obvious.
    #[test]
    fn missing_default_binary_is_refused_naming_the_key() {
        let root = scratch("defmissing");
        let missing = root.join("no-such-globs2-build");
        let i = Interface {
            binary: missing.clone(),
            origin: Origin::DefaultNoFile,
            config: root.join(CONFIG_FILE),
        };
        let m = format!("{:#}", runnable(i).unwrap_err());
        assert!(m.contains("built-in default") && m.contains(KEY), "{m}");
        assert!(m.contains(&missing.display().to_string()) && m.contains("cannot be run"), "{m}");
    }

    #[test]
    fn bad_values_are_refused_naming_the_key() {
        let root = scratch("bad");
        let not_exec = root.join("plain");
        std::fs::write(&not_exec, "x").unwrap();
        std::fs::set_permissions(&not_exec, std::fs::Permissions::from_mode(0o644)).unwrap();
        let missing = root.join("does-not-exist");
        for (val, needle) in [
            (missing.to_str().unwrap().to_string(), "cannot be run"),
            (root.to_str().unwrap().to_string(), "not a file"),
            (not_exec.to_str().unwrap().to_string(), "not executable"),
            ("relative/cauldron".to_string(), "neither a named build"),
        ] {
            std::fs::write(root.join(CONFIG_FILE), format!("[cli]\ninterface = {val:?}\n")).unwrap();
            let m = format!("{:#}", resolve(&root).expect_err(&val));
            assert!(m.contains(KEY), "names the key: {m}");
            assert!(m.contains(&val), "names the value: {m}");
            assert!(m.contains(needle), "says why ({needle}): {m}");
            assert!(m.contains(&root.join(CONFIG_FILE).display().to_string()), "names the file: {m}");
        }
    }

    #[test]
    fn malformed_config_is_refused_naming_the_file() {
        let root = scratch("malformed");
        std::fs::write(root.join(CONFIG_FILE), "[cli\ninterface = ").unwrap();
        let m = format!("{:#}", resolve(&root).unwrap_err());
        assert!(m.contains(CONFIG_FILE) && m.contains(KEY), "{m}");
    }
}
