//! **`$ tui` — the ENTRY POINT, and deliberately nothing else.**
//!
//! Everything this program does is in the library beside this file: the
//! terminal lifecycle, the event loop, the repaint law, input, the wire, the
//! session model and the projection. `lib.rs` has the module map and the
//! reasoning. **What is left here is what only an entry point can do** — read
//! argv, resolve the three ambient values a pure layer may not read for
//! itself, open the log, hand over, and give the terminal back afterwards.
//!
//! Keeping it this thin is the point rather than a tidiness: a behavior that
//! lives in `main.rs` is a behavior no second face and no integration test can
//! reach, which is the exact defect `0037` found and fixed.
//!
//! It is reached as `$ tui`, which is `/opt/homebrew/bin/tui` →
//! `scripts/tui` → this binary. That shim resolves the endpoint and builds
//! when the sources are newer; see its header, which is the one place endpoint
//! resolution is documented.

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

use tui::{app, first_run, term};

/// **THE ENDPOINT IS NOT CHOSEN HERE ANY MORE, AND THAT IS THE POINT.**
///
/// A `DEFAULT_THREADS_URL` of `ws://127.0.0.1:3651/ws` stood here, resolved
/// `--server` > `$TUI_THREADS_URL` > that literal, and handed the answer to
/// `app::run`. Every part of that is gone: the seam reads
/// `TUI_THREADS_CHASSIS_PORT` (default 3653) and builds its own URL
/// (`seam::link::ws_url`), and there is deliberately no way to hand it one —
/// two places that can choose a URL is two places that can disagree about
/// which daemon this face is looking at.
///
/// 3651 was also the LEGACY port. The chassis door is 3653; 3651 is kept by the
/// producer for `/health` alone, which this face does not probe. So the old
/// default was not just in the wrong place, it named the wrong door.
///
/// `--server` is REFUSED rather than ignored — see [`Args::parse`].

/// `commit=<sha> provenance=<pin|git|unknown> dirty=<1|0|unknown>
/// dirt_scope=<a,b,...>` — every value compiled in by `build.rs`, in the order
/// `cauldron --build-stamp` and `tts --build-stamp` print theirs, so the one
/// `Stamp` reader in `cli/src/main.rs` parses all three. `dirty` is passed
/// through only as `0` or `1`; anything else is `unknown`, never clean.
fn build_stamp() -> String {
    let dirty = match env!("TUI_BUILD_DIRTY") {
        d @ ("0" | "1") => d,
        _ => "unknown",
    };
    format!(
        "commit={} provenance={} dirty={dirty} dirt_scope={}",
        env!("TUI_BUILD_COMMIT"),
        env!("TUI_BUILD_PROVENANCE"),
        env!("TUI_BUILD_DIRT_SCOPE")
    )
}

fn main() -> Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // **`tui --build-stamp` — WHICH COMMIT ARE THESE BYTES** (R7.S4, H179-1).
    // FIRST, before anything else reads argv, the environment or the terminal:
    // no port-lever check, no cauldron environment resolution (either can
    // refuse, and a refusal is not an answer to "what are you"), no log file,
    // no raw mode, no alternate screen. One line on stdout, exit 0, whether or
    // not stdout is a terminal — the same contract as `cauldron` and `tts`.
    if argv.first().map(String::as_str) == Some("--build-stamp") {
        println!("{}", build_stamp());
        return Ok(());
    }
    // **`--cauldron` IS A SECOND FACE IN THE SAME BINARY** (R7.S1b): the v0
    // Cauldron cluster tree, on fake data, with its own arguments (`--dump`,
    // `--cluster`, …). It owns its terminal through `term-shell` and never opens
    // the wire, so it is routed before anything below reads the environment.
    if argv.iter().any(|a| a == "--cauldron") {
        return tui::cauldron::cli::main(argv.into_iter().filter(|a| a != "--cauldron").collect());
    }
    // **`tui pool` IS THE ACCOUNT-POOL BOARD** (R7.S2, DEC-37), the Rust
    // replacement for the Python `threads-app`. It takes its own arguments,
    // but it reaches the SAME daemon the chat face would, so it is routed
    // after the port lever and the cauldron environment are resolved.
    let pool = argv.first().map(String::as_str) == Some("pool") || argv.iter().any(|a| a == "--pool");
    let (args, pool_argv) = if pool {
        let rest: Vec<String> = argv
            .into_iter()
            .enumerate()
            .filter(|(i, a)| !(*i == 0 && a == "pool") && a != "--pool")
            .map(|(_, a)| a)
            .collect();
        (Args::parse(std::iter::empty())?, rest)
    } else {
        (Args::parse(argv.into_iter())?, Vec::new())
    };

    // **BOTH HALVES OF THE PORT LEVER, CHECKED BEFORE ANYTHING READS THEM** —
    // and before the environment resolution below, which may SET the chassis
    // half: the refusal is about what the operator set, not about what this
    // process derived. See `mirror::check_ports`.
    if let Err(why) = tui::mirror::check_ports() {
        anyhow::bail!("tui: {why}");
    }

    // **RULE A (REL-6a3): A NON-INTERACTIVE MODE IS GIVEN ITS ENDPOINT.** Before
    // the environment resolution below SETS the chassis lever, so what is
    // judged is what the launch set: `--headless`, `pool --dump`, and any
    // chat or pool face with no terminal on stdin/stdout must be pointed
    // (TUI_THREADS_CHASSIS_PORT, never one of his live ports), or exit
    // non-zero having dialled nothing. Marked, so every constructor in this
    // process refuses his ports and every child inherits the marker (rule B).
    {
        use std::io::IsTerminal;
        let no_tty = !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal();
        let help = pool_argv.iter().any(|a| a == "-h" || a == "--help");
        let mode = if pool && help {
            None
        } else if pool && pool_argv.iter().any(|a| a == "--dump") {
            Some("tui pool --dump")
        } else if pool && no_tty {
            Some("tui pool (no terminal)")
        } else if pool {
            None
        } else if args.headless.is_some() {
            Some("tui --headless")
        } else if no_tty && !args.resolve_only {
            Some("tui (no terminal)")
        } else {
            None
        };
        if let Some(mode) = mode {
            let explicit = std::env::var("TUI_THREADS_CHASSIS_PORT").ok();
            if let Err(why) = tui::cauldron::live::require_explicit_endpoint(mode, explicit.as_deref()) {
                anyhow::bail!("tui: {why}");
            }
            tui::cauldron::live::mark_non_interactive(mode);
        }
    }

    // **WHICH THREADS, ASKED OF THE CAULDRON ENVIRONMENT** (R7.S1c). The seam
    // reads one variable, `TUI_THREADS_CHASSIS_PORT`, and its default is
    // production's 3653. So the environment's answer is handed to the seam
    // through that same variable — the seam file stays byte-identical to the
    // reference — and it is set HERE, before the runtime exists, because
    // `set_var` with other threads alive is a race on `getenv`. A refusal
    // (a test environment that would land on prod's address) stops the face
    // before a socket is opened.
    let resolved = match tui::cauldron_env::resolve() {
        Ok(r) => r,
        Err(why) => anyhow::bail!("tui: {why}"),
    };
    if args.resolve_only {
        println!("{}", resolved.line());
        return Ok(());
    }
    if resolved.source != tui::cauldron_env::Source::Override {
        let port = resolved.port.to_string();
        std::env::set_var("TUI_THREADS_CHASSIS_PORT", &port);
        // The legacy `/health` half, set to the same port so the pair stays
        // coherent; this face never probes it.
        std::env::set_var("TUI_THREADS_PORT", &port);
    }
    tui::cauldron_env::set_resolved(resolved);
    if pool {
        return tui::pool::cli::main(pool_argv);
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("start the tokio runtime")?
        .block_on(run(args))
}

async fn run(args: Args) -> Result<()> {

    // **BEFORE THE LOG, AND THAT ORDER IS THE MECHANISM.** The detector asks
    // whether `~/.tui` exists and `init_logging` is what creates it, so asking
    // afterwards would answer "not the first run" on the first run, forever.
    // `tui::first_run` carries the whole argument, including why this needs no
    // marker file of its own.
    // EVERY home this face uses is `face_home` (REL-6a4, REL-6a4b B1): the
    // logs, the first-run question, and the `App` home handed to `app::run`
    // below — which is NOT read-only: `index::apply_registry` writes
    // `<home>/.cauldron/projects.json` on register / reorder. Under isolation
    // it is the isolated home, never `dirs::home_dir()`.
    let face_home = tui::cauldron::live::face_home();
    let first_run = first_run::is_first_run(face_home.as_deref());

    let _log = init_logging(first_run::log_name(args.headless.is_some()))?;
    tracing::info!(endpoint = %tui::seam::link::ws_url(), "tui starting");

    // **THE FLIGHT RECORDER** — `tui::diag`. On by default, to a file of its
    // own; `TUI_DIAG=<path>` moves it and `TUI_DIAG=off` stops it. A headless
    // run writes a different default file so it never interleaves with the
    // operator's live face.
    if let Some(default) = first_run::diag_default(args.headless.is_some()) {
        if let Some(p) = tui::diag::path_for(default) {
            tui::diag::init(&p);
        }
    }

    // **BOTH HALVES OF THE PORT LEVER, CHECKED BEFORE A SOCKET IS OPENED.**
    //
    // `TUI_THREADS_PORT` alone moves nothing — it is the legacy `/health` door
    // this face never probes — so a run that sets it and not
    // `TUI_THREADS_CHASSIS_PORT` goes to the daemon on 3653 while its author
    // believes it is pointed somewhere safe. An agent made exactly that mistake
    // with the AUI's two variables last night and its shape socket reached the
    // operator's live daemon, where it created a real thread and spent real
    // money. This refuses instead of guessing: deriving one port from the other
    // would be the reconciliation that makes two values look like one.
    //
    // Before `term::enter`, so the refusal is readable on an ordinary terminal
    // rather than painted over an alternate screen.
    // (The port lever was checked in `main`, before the environment set it.)

    // **HEADLESS: NO TERMINAL IS CLAIMED AT ALL.** See `app::run_headless`.
    if let Some(h) = args.headless.clone() {
        let tz = chrono::Local::now().offset().local_minus_utc() / 60;
        let home = tui::cauldron::live::face_home_string();
        let r = app::run_headless(h, home, tz).await;
        tui::diag::flush_now();
        return r;
    }

    // The hook is installed BEFORE the terminal is claimed, so there is no
    // window in which a panic could leave the screen taken.
    term::install_panic_hook();

    let mut terminal = term::enter().context("take the terminal")?;

    if args.panic_now {
        panic!("--panic-now: deliberate panic, to prove the terminal comes back");
    }

    // The display timezone is resolved HERE, in the shell, and handed to the
    // pure crate as minutes. `chrono::Local` is a banned type in `tui-render`
    // for exactly this reason: a zone lookup is ambient state a golden test
    // cannot fix.
    let tz_offset_minutes = chrono::Local::now().offset().local_minus_utc() / 60;

    // The home, resolved ONCE and here (`face_home`), for shortening a cwd to
    // `~` AND for the registry write. Same reasoning as the timezone: the
    // render crate has no filesystem and no environment, and a path shortened
    // inside it would be ambient state a golden test cannot fix. An empty
    // string means "do not shorten" (and every registry write is refused),
    // the honest degradation when there is no home.
    let home = tui::cauldron::live::face_home_string();

    let outcome = app::run(
        &mut terminal,
        home,
        tz_offset_minutes,
        first_run,
    )
    .await;

    // Every exit path goes through here. The `?` is AFTER the restore, not
    // before it: an error is worth reporting on a terminal he can read.
    term::restore();
    tui::diag::flush_now();
    outcome
}

#[derive(Debug)]
struct Args {
    /// Debug builds only. `#[cfg(debug_assertions)]` is why this cannot be a
    /// way to crash a release binary: the flag is rejected there like any
    /// other unknown one.
    panic_now: bool,
    /// `--headless`: run against the daemon with no terminal and dump every
    /// distinct frame as text. See `app::run_headless`.
    headless: Option<app::Headless>,
    /// `--resolve`: print which environment and threads address this face
    /// would read (one `key=value` line) and exit, opening nothing.
    resolve_only: bool,
}

impl Args {
    /// **THERE IS NO ENDPOINT ARGUMENT, AND `--server` IS REFUSED BY NAME.**
    ///
    /// It used to resolve `--server` > `$TUI_THREADS_URL` > a loopback literal,
    /// in the same three steps as `scripts/tui`, so that running the binary out
    /// of `target/` was not a different program from running `$ tui`. The seam
    /// owns the URL now, so there is nothing for a flag to set.
    ///
    /// **A flag that is silently ignored is worse than one that is refused**, and
    /// that is why `--server` gets a sentence of its own instead of falling into
    /// the unknown-argument arm: somebody with it in a shell alias or a tmux
    /// script needs to be told the lever MOVED, not that it was never there.
    /// `$TUI_THREADS_URL` cannot be refused the same way — nothing reads it, so
    /// nothing can see it — which is why it is named in the refusal text.
    fn parse(argv: impl Iterator<Item = String>) -> Result<Self> {
        let mut panic_now = false;
        let mut headless: Option<app::Headless> = None;
        let mut saw_headless = false;
        let mut resolve_only = false;
        let mut it = argv.peekable();
        let hl = |h: &mut Option<app::Headless>| -> app::Headless {
            h.clone().unwrap_or(app::Headless {
                thread: None,
                seconds: 20,
                width: 120,
                height: 40,
                out: None,
                every: false,
            })
        };
        while let Some(a) = it.next() {
            match a.as_str() {
                "--panic-now" if cfg!(debug_assertions) => panic_now = true,
                "--resolve" => resolve_only = true,
                "--headless" => {
                    saw_headless = true;
                    headless = Some(hl(&mut headless));
                }
                "--every" => {
                    let mut h = hl(&mut headless);
                    h.every = true;
                    headless = Some(h);
                }
                "--thread" | "--seconds" | "--size" | "--out" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("tui: {a} needs a value"))?;
                    let mut h = hl(&mut headless);
                    match a.as_str() {
                        "--thread" => h.thread = Some(v),
                        "--seconds" => h.seconds = v.parse().context("--seconds")?,
                        "--out" => h.out = Some(v.into()),
                        _ => {
                            let (w, ht) = v
                                .split_once('x')
                                .ok_or_else(|| anyhow::anyhow!("tui: --size is WxH, got {v:?}"))?;
                            h.width = w.parse().context("--size width")?;
                            h.height = ht.parse().context("--size height")?;
                        }
                    }
                    headless = Some(h);
                }
                s if s == "--server" || s.starts_with("--server=") => {
                    // Consume the value too, so the refusal names the flag
                    // rather than the URL that followed it.
                    if s == "--server" {
                        let _ = it.next();
                    }
                    anyhow::bail!(
                        "tui: --server is gone, and so is $TUI_THREADS_URL.\n  \
                         The seam builds its own URL from a PORT, so point this face \
                         with:\n    \
                         TUI_THREADS_CHASSIS_PORT=<port>   the shape socket, default \
                         {}\n    \
                         TUI_THREADS_PORT=<port>           the legacy /health door, \
                         default {} (unused by this face)\n  \
                         Setting only the second one is refused at startup: it moves \
                         nothing, and the frames would still go to the default.",
                        tui::seam::link::DEFAULT_CHASSIS_PORT,
                        tui::seam::link::DEFAULT_HEALTH_PORT,
                    );
                }
                other => {
                    // Not silently ignored. A flag that does nothing and says
                    // nothing is how `$ tui --relase` spends an afternoon.
                    anyhow::bail!(
                        "tui: unknown argument {other:?}\n  \
                         accepts: no endpoint arguments — the daemon is named by \
                         $TUI_THREADS_CHASSIS_PORT (default {})\n  \
                         and: --headless [--thread ID] [--seconds N] [--size WxH] \
                         [--out PATH] [--every]\n  \
                         and: --resolve — print the environment and threads address, and exit\n  \
                         and: --build-stamp — print the commit this binary was built from, and exit \
                         (first argument only)\n  \
                         and: --cauldron [--env NAME] [--dump …] — the Cauldron soup face on fake data \
                         (`tui --cauldron --help`)",
                        tui::seam::link::DEFAULT_CHASSIS_PORT,
                    );
                }
            }
        }
        if headless.is_some() && !saw_headless {
            anyhow::bail!("tui: --thread/--seconds/--size/--out/--every are --headless options");
        }
        Ok(Self { panic_now, headless, resolve_only })
    }
}

/// **To a FILE, never to the terminal** (mined from the old `apps/tui`; new
/// home `~/.tui/log`). A `println!` or a `tracing` line reaching stdout from
/// inside raw mode paints over the face and cannot be cleaned up — `0018`
/// names it as the garbled-screen defect.
///
/// The returned guard must stay alive for the process: dropping it flushes and
/// stops the writer thread.
fn init_logging(name: &str) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let dir = first_run::state_dir().context("no home directory")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let file = tracing_appender::rolling::never(&dir, name);
    let (writer, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("TUI_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(writer)
        .with_ansi(false)
        .init();
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args> {
        Args::parse(args.iter().map(|s| s.to_string()))
    }

    /// **THE LEVER THAT MOVED IS REFUSED BY NAME, IN BOTH SPELLINGS.**
    ///
    /// The two tests this replaces — `the_flag_beats_everything` and
    /// `the_default_is_the_daemon_on_loopback` — pinned a precedence that no
    /// longer exists. They are not rewritten to assert the new default, because
    /// there is nothing here to assert it ABOUT: the default lives in
    /// `seam::link::DEFAULT_CHASSIS_PORT` and `tests/it/seam_reference.rs` is what
    /// holds that file to the reference.
    ///
    /// What IS this file's job is telling somebody with `--server` in a shell
    /// alias that the lever moved, and the refusal has to carry the two
    /// variable names or it is a dead end rather than a redirection.
    #[test]
    fn the_endpoint_flag_that_moved_is_refused_by_name() {
        for argv in [
            vec!["--server", "ws://flag:1/ws"],
            vec!["--server=ws://flag:1/ws"],
        ] {
            let err = parse(&argv).unwrap_err().to_string();
            assert!(err.contains("--server is gone"), "{err}");
            assert!(
                err.contains("TUI_THREADS_CHASSIS_PORT"),
                "the refusal must name the lever that replaced it: {err}"
            );
            assert!(
                err.contains("TUI_THREADS_URL"),
                "the env var is unreadable from here, so the refusal is the only \
                 place somebody using it can be told: {err}"
            );
        }
    }

    /// **`--server` DOES NOT FALL INTO THE UNKNOWN-ARGUMENT ARM**, and this is
    /// the opposing forcing for the test above: if the dedicated arm were
    /// deleted, `--server` would still be refused, and refused LOUDLY, and the
    /// test above is the only thing that would notice the sentence had gone.
    #[test]
    fn the_two_refusals_are_different_sentences() {
        let moved = parse(&["--server"]).unwrap_err().to_string();
        let unknown = parse(&["--relase"]).unwrap_err().to_string();
        assert!(unknown.contains("unknown argument"), "{unknown}");
        assert!(
            !unknown.contains("--server is gone"),
            "an unknown flag is being told the endpoint story: {unknown}"
        );
        assert!(
            !moved.contains("unknown argument"),
            "`--server` fell into the unknown-argument arm: {moved}"
        );
    }

    /// An unknown flag still says what IS accepted, which now includes the
    /// variable that names the daemon.
    #[test]
    fn an_unknown_flag_is_refused_out_loud() {
        let err = parse(&["--relase"]).unwrap_err().to_string();
        assert!(err.contains("unknown argument"), "{err}");
        assert!(
            err.contains("TUI_THREADS_CHASSIS_PORT"),
            "the refusal must say what IS accepted: {err}"
        );
    }
}
