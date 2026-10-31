//! `cauldron-launcher` — the binary meant to sit on PATH as `cauldron`
//! (R9.CFG1, implements E18-19, operator ruling E21-7).
//!
//! It reads `[cli] interface` from `<root>/config.toml` (root =
//! `$CAULDRON_HOME`, else `$HOME/.cauldron`; see `env::cli_config`), and
//! EXECS that binary with the same argv — the process is replaced, so the
//! exit code, signals, stdio and `current_exe()` (which the router uses to
//! find its `tui` beside it) are the chosen binary's own. Absent key: the
//! built-in default, today's globs2 demo build.
//!
//! `CAULDRON_LAUNCHER_WHICH=1` prints the chosen binary and where the choice
//! came from, and exits 0 without running anything.
//!
//! A launcher failure exits **125** with one `cauldron-launcher:` line on
//! stderr naming the key — a code the CLI itself does not use, so a broken
//! config is never mistaken for a cauldron verb's own failure.

use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use env::cli_config::{self, Origin};

const LAUNCHER_FAILED: u8 = 125;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cauldron-launcher: {e:#}");
            ExitCode::from(LAUNCHER_FAILED)
        }
    }
}

fn run() -> anyhow::Result<ExitCode> {
    let root = cli_config::root_from_env()?;
    let i = cli_config::resolve(&root)?;

    // Pointing the key at the launcher itself would exec forever.
    if let (Ok(me), Ok(target)) = (
        std::env::current_exe().and_then(|p| p.canonicalize()),
        i.binary.canonicalize(),
    ) {
        if me == target {
            anyhow::bail!(
                "{} in {} names {} — the launcher itself, which would exec itself forever. \
                 Point {} at a cauldron CLI build.",
                cli_config::KEY,
                i.config.display(),
                i.binary.display(),
                cli_config::KEY
            );
        }
    }

    if std::env::var_os("CAULDRON_LAUNCHER_WHICH").is_some_and(|v| v == "1") {
        let origin = match &i.origin {
            Origin::DefaultNoFile => "default (no config file)".to_string(),
            Origin::DefaultNoKey => format!("default ({} not set)", cli_config::KEY),
            Origin::Key(v) => format!("{} = {v:?}", cli_config::KEY),
        };
        println!("binary={} origin={origin} config={}", i.binary.display(), i.config.display());
        return Ok(ExitCode::SUCCESS);
    }

    // REL-6a6 (REL-6a5-I B2): the launcher forwards EVERY verb to a CLI
    // build — by default his installed one, which predates the egress gate
    // and does not read the marker. Under isolation it forwards only from a
    // scratch root to a binary that is itself outside his login home.
    if let Some(why) = env::egress::process_isolation() {
        admit_forward(&root, &i.binary, &why).map_err(anyhow::Error::msg)?;
    }
    let err = Command::new(&i.binary).args(std::env::args_os().skip(1)).exec();
    anyhow::bail!(
        "exec {} (from {} in {}) failed: {err}",
        i.binary.display(),
        cli_config::KEY,
        i.config.display()
    )
}

/// **REL-6a6: AN ISOLATED LAUNCHER FORWARDS ONLY INSIDE A SCRATCH WORLD.**
/// Both the root it read its config from and the binary it would exec must be
/// scratch (`env::egress::scratch_root_refusal`: by (dev, ino) identity, canonical, case-folded,
/// outside his login home), so it can never hand a verb to his installed CLI
/// or act on his real root.
fn admit_forward(root: &std::path::Path, binary: &std::path::Path, why: &str) -> Result<(), String> {
    // REL-6a8: against EVERY login home (the passwd entry AND `$HOME`,
    // `env::egress::protected_homes`); none readable refuses.
    let homes = env::egress::protected_homes();
    if homes.is_empty() {
        return admit_forward_with(root, binary, None, why);
    }
    for h in &homes {
        admit_forward_with(root, binary, Some(h), why)?;
    }
    Ok(())
}

fn admit_forward_with(root: &std::path::Path, binary: &std::path::Path, login: Option<&std::path::Path>, why: &str) -> Result<(), String> {
    let refuse = |d: String| {
        Err(format!(
            "refused: cauldron-launcher forwards every verb to a cauldron CLI, and this process is isolated ({why}); {d}"
        ))
    };
    if let Some(d) = env::egress::scratch_root_refusal(root, login) {
        return refuse(format!("its root: {d}"));
    }
    if let Some(d) = env::egress::scratch_root_refusal(binary, login) {
        return refuse(format!("the binary it would run: {d}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn rel6a6_an_isolated_launcher_forwards_only_from_a_scratch_root_to_a_scratch_binary() {
        let login = Path::new("/Users/someone");
        let w = |r: &str, b: &str| admit_forward_with(Path::new(r), Path::new(b), Some(login), "a test");
        assert!(w("/Users/someone/.cauldron", "/Users/someone/.cauldron/bin/cauldron").is_err(), "his root, his CLI");
        assert!(w("/private/tmp/s/.cauldron", "/Users/someone/.cauldron/bin/cauldron").is_err(), "scratch root, his CLI");
        assert!(w("/private/tmp/s/.cauldron", "/Users/someone/bin/cauldron").is_err(), "scratch root, his PATH CLI");
        assert!(w("/Users/someone/.CAULDRON", "/private/tmp/s/fake").is_err(), "his root, another case");
        assert!(w("/private/tmp/s/.cauldron", "/private/tmp/s/fake").is_ok(), "scratch root, scratch binary");
        assert!(admit_forward_with(Path::new("/private/tmp/s"), Path::new("/private/tmp/s/f"), None, "t").is_err(), "unknown login");
    }
}
