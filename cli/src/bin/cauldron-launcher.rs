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

    let err = Command::new(&i.binary).args(std::env::args_os().skip(1)).exec();
    anyhow::bail!(
        "exec {} (from {} in {}) failed: {err}",
        i.binary.display(),
        cli_config::KEY,
        i.config.display()
    )
}
