//! `cauldron` — the entry point.
//!
//! # Why a CLI and not a daemon (wave 1)
//!
//! A CLI that writes files and spawns processes has no long-lived process, so
//! there is nothing to supervise the supervisor; a daemon would need exactly
//! that. More importantly, restart supervision for `threads` already exists
//! (`~/.threads-supervisor/supervise-service.sh` plus launchd), and a second
//! owner of restarts is how a service gets killed twice and started three
//! times. So wave 1 does the thing that has no owner yet — *launching* a
//! service into a named environment and recording where it landed — and
//! deliberately does not do the thing that already has one.
//!
//! The services this starts are put in their **own process group**, so they
//! outlive the shell that ran `cauldron start` and a Ctrl-C in that shell does
//! not reach them. Their parent becomes PID 1, which is how every long-lived
//! process on this machine ends up anyway.
//!
//! # The verbs
//!
//! ```text
//!   cauldron                      the TUI (bare invocation)
//!   cauldron tui [args...]        the TUI, explicitly
//!   cauldron start   [service]    launch into this environment
//!   cauldron stop    [service]    stop, after proving we own the seam
//!   cauldron status  [service]    the seam table, probed
//!   cauldron build   [service]    build and install one version
//! ```
//!
//! An unrecognised verb exits 2 with the verb list. It never falls through to
//! the TUI: a typo must not look like success.
//!
//! # Warnings are part of the contract, and the COMPILER enforces it first
//!
//! A cauldron service serves a **warnings shape** (`env::warnings`). The
//! enforcement that matters is not here: it is a **compile-time check on the
//! producer build**. `ServiceEnv::register` takes an `env::warnings::Attestation`
//! that only `attest_warnings!` can mint and that does not type-check without a
//! `ServesWarnings` impl, so a producer with no warnings shape **does not
//! link** — it cannot write a seam, and there is no address for anything to
//! read. Proved in `scripts/prove-compile-gate`.
//!
//! What follows is therefore the **BACKSTOP**, not the mechanism. It exists
//! because a build check cannot see a binary that compiled fine and then lies,
//! and cannot see a process `cauldron start` never spawned. `start` REFUSES a
//! service that does not serve warnings — twice, at two different moments,
//! because there are two different lies to catch:
//!
//! 1. **before spawning**, if the declaration says `warnings = false`. There
//!    is nothing to ask, so nothing is launched.
//! 2. **after the seam appears**, by asking the running service for the shape
//!    its declaration named. This is the one that matters: it is a handshake
//!    with the process, so a declaration cannot claim compliance it does not
//!    have. A service that writes its seam and then will not answer is **not
//!    started** — it is killed, its seam is cleared, and `start` fails naming
//!    the shape it asked for.
//!
//! Why refuse rather than warn is argued where the refusal lives
//! ([`start_one`]). The short version: the enforcement for *"your warnings
//! reach nobody"* must not itself be a warning.
//!
//! # Where the aggregate lives: `status`, and nothing long-lived
//!
//! `cauldron status` is the aggregate. It already opens every live service's
//! socket to probe the seam; it now asks the same socket one more question and
//! sums the answers. **No daemon was introduced** — wave 1 is a CLI, the pull
//! is on demand, and nothing stays resident between invocations. The cost is
//! stated honestly rather than hidden: a count nobody runs `status` to read is
//! a count nobody reads, and the fix for that is a face that polls, not a
//! supervisor process. See [`verb_status`].

mod bundle;
mod ctl;
mod daemon;
mod envs;
mod gc;
mod jobs;
mod launchd;
mod logrotate;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use env::decl::{self, BuildRecord, Declaration, Manifest};
use env::seams::{self, Seam, SeamState, Table, Verdict};
use env::supervisions;
use env::warnings::{self as warn, Aggregate, Compliance};
use env::{guard, Environment};

const VERBS: &[(&str, &str)] = &[
    ("daemon", "HOLD this environment and host the services it pins (the supervisor)"),
    ("install", "put this environment's holder under launchd, with its own process name"),
    ("uninstall", "take this environment's holder out from under launchd"),
    ("promote", "place this binary at a non-prod environment's own holder launch path"),
    ("restart", "stop and relaunch a service on the version this environment pins"),
    ("start", "launch a service into this environment"),
    ("stop", "stop a service in this environment"),
    ("down", "stop this environment's holder and every service in it; data and pins kept"),
    ("status", "probe this environment's seam table"),
    ("build", "build a service and install it as a version"),
    ("envs", "list every environment in this root and what is registered in each"),
    ("env", "list / create / show environments: named instances, each with its own data"),
    ("test", "build a version and prove it in another environment"),
    ("versions", "every installed version, and which are good here"),
    ("pin", "tell this environment which version to run"),
    ("rollback", "re-pin to the previous version that was good here"),
    ("gc", "remove builds no environment pins, rolls back to, or has open (--builds)"),
    ("permissions", "which identity a permission prompt names, and what is actually asking"),
    ("supervisions", "every act this environment's supervisor performed on a service, and WHY"),
    ("tui", "the Cauldron face (the default); --chat for the chat face"),
    ("ctl", "the coordinator's face, headless: dump | frame | node (`ctl help`)"),
];

const USAGE: &str = "\
cauldron — the Cauldron entry point

    cauldron                         the TUI (the Cauldron face)
    cauldron tui [args...]           the Cauldron face, explicitly (args pass
                                     through to `tui --cauldron`)
    cauldron tui --chat [args...]    the chat face instead
    cauldron ctl dump|frame|node ... the coordinator's face, headless, on the TUI's own
                                     calls: `ctl dump --env <name>` sends one dump (text on
                                     stdin) exactly as the send key does; `ctl frame` is one
                                     `--dump` frame; `ctl node <id>` reads one soup node.
                                     JSON out, non-zero on refusal (`cauldron ctl help`)
    cauldron --build-stamp           the commit THIS binary was built from, as
                                     one `key=value` line, the format it reads
                                     off every service it launches
    cauldron --version               the same commit, for a human
    cauldron daemon                  HOLD this environment and host what it pins
    cauldron install [--dry-run]     put the holder under launchd (per environment)
    cauldron uninstall               take the holder out from under launchd
    cauldron promote                 place THIS binary at a non-prod environment's own
                                     holder launch path (<root>/bin/cauldron-<env>);
                                     `daemon` in a non-prod environment runs from it
    cauldron restart <service>       stop it and relaunch it on the pinned version
    cauldron start  [service]        launch into this environment
    cauldron stop   [service]        stop in this environment
    cauldron down   [--supervised]   stop this environment's holder AND its services;
                                     data, pins and declarations stay (`daemon`
                                     restarts it). Refuses prod / a launchd-installed
                                     environment without --supervised
    cauldron status [service]        probe this environment's seam table
    cauldron build  [service]        build and install one version
                    [--env-copy]     ... as a NEW version `<label>+<env>` (or `+<env>.N`)
                                     that no other environment pins; `<label>` is untouched
    cauldron build --snapshot-declaration <service> <label> [--force]
                                     BACKFILL: copy the CURRENT declaration into an
                                     existing build dir as its `declaration.toml`
                                     (refuses to overwrite one without --force)
    cauldron envs   [--probe]        every environment in this root, side by side
    cauldron env list [--probe]      the same listing, under the `env` verb
    cauldron env create <name> [--auto-restart] [--from <env> [--pins-only]]
                                     make a new environment: its own data root,
                                     seam table, logs, hold and launchd label.
                                     --from SEEDS it: the source's pins, a fresh
                                     port per hosted service, and a copy of each
                                     pinned service's data (refused while the
                                     source runs; --pins-only copies no data)
    cauldron env remove <name> [--stop] [--yes]
                                     delete a NON-prod environment's data root.
                                     Dry run without --yes; --stop stops its
                                     holder and services first; refuses prod and
                                     an environment installed under launchd
    cauldron env show [name]         where an environment keeps everything, and
                                     whether a daemon is holding it
    cauldron --env <name> <verb> ... run any verb in environment <name>
                                     (wins over $CAULDRON_ENV)
    cauldron test   [service]        build a version and prove it in ANOTHER environment
    cauldron versions [service]      every installed version, and which are good here
    cauldron pin    <service> <ver> [--addr <host:port>]
                    [--at-cap handoff|extend] [--drain-cap <secs>]
                                     tell this environment which version to run;
                                     --addr is this environment's port for a
                                     HOSTED service (refused if any environment
                                     in this root already claims that port);
                                     --addr auto: prod keeps the declared port
                                     if it is free, any other environment gets
                                     its own free port (never prod's).
                                     --at-cap / --drain-cap replace, FOR THIS
                                     PIN ONLY, the running build's [quiet.drain]
                                     at_cap / cap_secs when the holder drains it
                                     for this re-pin (handoff: restart at the
                                     cap, running turns re-adopted; extend: wait
                                     for them). Cap 1..=3600 s; 0 or more is
                                     refused before anything is written. Absent,
                                     the running declaration governs. `status`
                                     shows the terms in force and says when a
                                     pin overrode them.
    cauldron rollback [service]      re-pin to the previous version that was good here
    cauldron gc --builds [--keep N] [--dry-run]
                                     remove every build no environment pins or
                                     rolls back to, beyond the N newest good ones;
                                     never one a running process has open
    cauldron permissions [--hours N] which identity a prompt names, and what is asking
    cauldron supervisions [...]      what the supervisor DID to a service, and why

  cauldron supervisions:
    --since <rfc3339>   only acts at or after this instant (string compare, UTC)
    --service <name>    only acts on this service
    --why <word>        repin | down | requested | unpinned | rollback | schedule | retry
    --root <path>       only acts by a supervisor serving this cauldron root
    --json              the ledger as one object
    EXIT 0 with records, 0 with none, and NON-ZERO (5) when there is NO LEDGER
    at all -- because \"no acts\" and \"nobody has ever written here\" are not the
    same answer and a caller that cannot tell them apart has a check that
    cannot fail.

  With no service, start/stop/status/build act on every declared service.

the warnings contract:
    A cauldron service serves a warnings shape, and that is checked FIRST BY THE
    COMPILER: a producer with no warnings shape does not link, because writing a
    seam needs an attestation only an implementation can mint.
    What `start` and `status` do is the BACKSTOP, for what a build cannot see.
    `start` asks the running service for the shape, on its own seam, and REFUSES
    to call it started if it does not answer. A declaration names the shape
    (warnings = \"<name>\", default `warnings`); absent is the default, not an
    exemption; `warnings = false` is the only way to declare non-compliance and
    start refuses it by name. `status` is the aggregate: a pull, on demand, with
    no daemon. It exits 4 when a RUNNING service does not serve warnings — which
    means it was started by something other than `cauldron start`.

the version identity, and the protocols over it:
    A version IS A COMMIT. `cauldron build` labels a build with the commit it
    came from (12 chars) and REFUSES a dirty working tree, because then the
    commit does not identify the bytes — `--allow-dirty` installs it under a
    label that says so. Nothing relies on a signature or an attestation: a
    commit is nameable, comparable and checkable by anybody with the repository,
    and it survives being copied, which a held file does not.
    The registry is clear about it: every seam carries the commit, and
    `cauldron envs` / `status` print it.
      build      a commit becomes an installed version
      test       that version is started IN ANOTHER ENVIRONMENT and probed.
                 Passing is what makes it GOOD. `test` REFUSES to run in the
                 environment you are standing in.
      pin        this environment is told which version to run
      rollback   this environment is re-pinned to the PREVIOUS version that was
                 good HERE. Good is per environment and is recorded by `start`
                 when a service completed both of its handshakes — there is no
                 verb that declares a version good.

the declaration a build carries:
    A declaration (`services/<svc>.toml`) is per ROOT, not per version, and the
    daemon re-reads it at every launch, restart and rollback. So `cauldron build`
    SNAPSHOTS the declaration it resolved into the build dir as
    `declaration.toml` (sha256 in build.json), and a PINNED version launches
    under ITS snapshot — never under the shipped file. The shipped file governs
    `build`, and services that are not pinned. A build dir with no snapshot
    (made before this existed) falls back to the shipped file and `status`
    says `declaration: shipped (no snapshot)`; backfill one deliberately with
    `cauldron build --snapshot-declaration <svc> <label>`. `pin` warns when the
    label's snapshot differs from the shipped file, which is what a cut-over is.

environments, in the plural:
    `cauldron envs` lists every environment in this root with what is registered
    in each, and names any service registered in more than one at once. Two
    services of the same name in two environments are two rows in two tables
    that cannot be confused: the table lives inside the data root, every entry
    restates the env it was written under, and a table that disagrees with the
    directory it sits in is REFUSED. Neither can open the other's data root.
    Ports are not a knob: every service binds 127.0.0.1:0 and publishes what the
    kernel gave it, so simultaneous deployments cannot collide on a port.

environment (the two knobs compose by nesting, and are orthogonal):
    CAULDRON_HOME=<dir>   the cauldron root; wins over HOME
    HOME=<dir>            the root is $HOME/.cauldron
    --env <name>          (before the verb) the environment; wins over both below
    CAULDRON_ENV=<name>   segments <root>/data/<name>; wins over ENV
    ENV=<name>            same, but ENV is reserved by POSIX sh — prefer CAULDRON_ENV
  default: root $HOME/.cauldron, environment `prod`

options:
    --json            machine-readable output (status, build)
    -h, --help        this text
";

fn main() {
    let code = match run() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cauldron: {e:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> Result<i32> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let argv = take_env_flag(argv)?;
    if let Some(verb) = argv.first() {
        if let Err(refusal) = admit_control_verb(verb) {
            eprintln!("cauldron: {refusal}");
            return Ok(3);
        }
    }
    match argv.first().map(|s| s.as_str()) {
        // Bare `cauldron`, or leading flags, go to the TUI. A leading flag is
        // the TUI's, because the TUI is the default face and `cauldron --dump`
        // has to mean something.
        None => tui_face(&[]),
        Some("-h") | Some("--help") => {
            print!("{USAGE}");
            Ok(0)
        }
        // THE SUPERVISOR STATES ITS OWN COMMIT, on the same terms it demands
        // of every service it launches. `build` reads `<image> --build-stamp`
        // off each service binary and refuses to label one it cannot read;
        // until this arm existed the supervisor itself refused the flag,
        // fell through to the TUI's flag handling, printed tui help and
        // exited 1 — so the one process that signs for everyone else's
        // commit could not report its own, and its only witness was its own
        // `envs` self-report. Same line format as `threads --build-stamp`,
        // parsed by the same `Stamp` reader below.
        Some("--build-stamp") => {
            println!("{}", self_stamp());
            Ok(0)
        }
        Some("--version") | Some("-V") | Some("-v") => {
            println!("cauldron {}{}", SELF_COMMIT, dirt_suffix(env!("CAULDRON_SELF_DIRTY")));
            Ok(0)
        }
        Some(a) if a.starts_with('-') => tui_face(&argv),
        Some("tui") => tui_face(&argv[1..]),
        // `cauldron aui` is NOT the AUI and never launches it: the AUI has its
        // own launcher (`~/bin/aui`). Refused in one line rather than falling
        // to the unknown-verb listing, so the answer is unambiguous.
        Some("aui") => {
            eprintln!("cauldron: `aui` is not a cauldron verb — the AUI is launched by `aui`, not by cauldron.");
            Ok(2)
        }
        Some("ctl") => ctl::verb_ctl(&argv[1..]),
        Some("daemon") => daemon::verb_daemon(&argv[1..]),
        Some("install") => launchd::verb_install(&argv[1..]),
        Some("uninstall") => launchd::verb_uninstall(&argv[1..]),
        Some("promote") => launchd::verb_promote(&argv[1..]),
        Some("restart") => verb_restart(&argv[1..]),
        Some("start") => verb_start(&argv[1..]),
        Some("stop") => verb_stop(&argv[1..]),
        Some("down") => envs::verb_down(&argv[1..]),
        Some("status") => verb_status(&argv[1..]),
        Some("build") => verb_build(&argv[1..]),
        Some("envs") => verb_envs(&argv[1..]),
        Some("env") => verb_env(&argv[1..]),
        Some("test") => verb_test(&argv[1..]),
        Some("versions") => verb_versions(&argv[1..]),
        Some("pin") => verb_pin(&argv[1..]),
        Some("rollback") => verb_rollback(&argv[1..]),
        Some("gc") => gc::verb_gc(&argv[1..]),
        Some("permissions") => verb_permissions(&argv[1..]),
        Some("supervisions") => verb_supervisions(&argv[1..]),
        Some(other) => {
            eprintln!("cauldron: unknown verb `{other}`.");
            eprintln!("cauldron: known verbs are:");
            for (v, d) in VERBS {
                eprintln!("    {v:<8} {d}");
            }
            eprintln!(
                "cauldron: a bare `cauldron` (no arguments) opens the TUI; an unknown verb does \
                 not, because a typo must not look like success."
            );
            Ok(2)
        }
    }
}

/// The verbs that start, stop, restart, drain, pin or otherwise control a
/// daemon or its launchd job (REL-6a5, REL-6a3-I F1).
const CONTROL_VERBS: &[&str] =
    &["daemon", "install", "uninstall", "promote", "restart", "start", "stop", "down", "pin", "rollback", "env", "test"];

/// **REL-6a5: A DAEMON-CONTROL VERB IN AN ISOLATED PROCESS ACTS ONLY ON A
/// SCRATCH ROOT.** Isolated (`env::egress::process_isolation`: the marker,
/// which every test binary and drive exports to its children, or a test
/// harness), a control verb whose resolved root is his real
/// `~/.cauldron` is refused before it reads or signals anything. His own
/// shell is not isolated, and nothing changes for it.
///
/// A root that does not RESOLVE is admitted here: every control verb resolves
/// it first and fails with its own (more specific) error before acting.
fn admit_control_verb(verb: &str) -> std::result::Result<(), String> {
    let Some(why) = env::egress::process_isolation() else { return Ok(()) };
    let Ok(e) = Environment::resolve() else { return Ok(()) };
    // REL-6a8: against EVERY login home (the passwd entry AND `$HOME`).
    let homes = env::egress::protected_homes();
    if homes.is_empty() {
        return admit_control_verb_with(verb, &e.root, None, &why);
    }
    for h in &homes {
        admit_control_verb_with(verb, &e.root, Some(&h.join(".cauldron")), &why)?;
    }
    Ok(())
}

/// [`admit_control_verb`] over explicit inputs.
fn admit_control_verb_with(verb: &str, root: &Path, real: Option<&Path>, why: &str) -> std::result::Result<(), String> {
    if !CONTROL_VERBS.contains(&verb) {
        return Ok(());
    }
    env::egress::admit_daemon_control_with(&format!("`cauldron {verb}`"), Some(root), real, why)
}

#[cfg(test)]
mod rel6a5_control_verbs {
    use super::*;

    /// Every control verb is refused on his real root and admitted on a
    /// scratch one; a read verb is admitted on either (opposition).
    #[test]
    fn rel6a5_a_control_verb_in_an_isolated_process_acts_only_on_a_scratch_root() {
        let real = Path::new("/Users/someone/.cauldron");
        for v in ["start", "stop", "restart", "daemon", "install", "uninstall", "promote", "down", "pin", "rollback", "env", "test"] {
            assert!(admit_control_verb_with(v, real, Some(real), "a test").is_err(), "`{v}` admitted on his real root");
            assert!(admit_control_verb_with(v, &real.join("x"), Some(real), "a test").is_err(), "`{v}` admitted inside it");
            assert!(admit_control_verb_with(v, Path::new("/tmp/scratch/.cauldron"), Some(real), "a test").is_ok(), "`{v}` refused on scratch");
        }
        for v in ["status", "envs", "versions", "build", "tui"] {
            assert!(admit_control_verb_with(v, real, Some(real), "a test").is_ok(), "read verb `{v}` refused");
        }
    }
}

// --------------------------------------------------------------------- apps

/// The app a bare `cauldron` routes to.
const DEFAULT_APP: &str = "tui";

/// **`cauldron tui` IS THE CAULDRON FACE** (operator, an internal thread).
/// The `tui` binary carries two faces: the harness chat face (its default,
/// and what `$ tui` on PATH opens) and the Cauldron face behind
/// `--cauldron`. The router picks the CAULDRON face by default, so `cauldron`,
/// `cauldron tui` and `cauldron --dump …` all open it. The chat face stays
/// reachable behind an explicit `--chat`, which is stripped and the rest
/// passed through untouched. The `tui` binary itself is unchanged, so
/// `$ tui` keeps meaning what it meant.
fn tui_face(args: &[String]) -> Result<i32> {
    if args.iter().any(|a| a == "--chat") {
        let rest: Vec<String> = args.iter().filter(|a| *a != "--chat").cloned().collect();
        return app(DEFAULT_APP, &rest, None);
    }
    let mut with: Vec<String> = vec!["--cauldron".to_string()];
    with.extend(args.iter().filter(|a| *a != "--cauldron").cloned());
    let opens = Environment::resolve().ok().and_then(|env| cauldron_face_opens(&env, &with[1..]));
    app(DEFAULT_APP, &with, opens.as_deref())
}

/// **Which environment the CAULDRON face will open** (V1.2b), so the router
/// launches THAT environment's build (`tui-<env>`, R6.S8) rather than the
/// build of the name it merely defaulted to.
///
/// A name he gave wins: the face's own `--env <name>` or bare positional
/// name (`cauldron tui prod`, V1.2d), read by the face's own rule
/// `env::face::named_in`; else a name the router resolved from anything but
/// the default. Arguments the face refuses (`--env` with no value, two bare
/// names) name nothing trustworthy: `None`, the resolved name's build, and the
/// face then says what was wrong. When nobody named one, it is the
/// face's own rule, `apps/tui/src/cauldron/store.rs` `default_env` (V1.2,
/// Q-F1) — the one last opened (`<root>/data/shared/tui/last_env.json`),
/// while it still exists; else `prod`, if it exists; else the first that
/// exists. `None` (no environment at all) is the first-run screen, which
/// opens in the build of the resolved name, as before. Nothing is created or
/// written by asking. The rule is ONE function, `env::face::opens` (V1.2c),
/// which the face calls too; `face_env.rs` pins the router's use of it.
fn cauldron_face_opens(env: &Environment, args: &[String]) -> Option<String> {
    let named = if let Some(n) = env::face::named_in(args).ok()? {
        Some(n)
    } else if env.name_source != env::paths::Source::Default {
        Some(env.name.clone())
    } else {
        None
    };
    // A name he gave never depends on listing the root (as before V1.2c):
    // an unlistable root still opens the named cauldron's build.
    let there = match env.environments() {
        Ok(there) => there,
        Err(_) if named.is_some() => Vec::new(),
        Err(_) => return None,
    };
    // An unreadable record is passed over, exactly as the face does.
    let last = std::fs::read(env.shared_root().join("tui").join("last_env.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("env")?.as_str().map(str::to_string));
    env::face::opens(named, last, &there)
}

/// This binary's own commit, stamped on every supervision record so a reader
/// can tell which cauldron performed an act.
pub(crate) const SELF_COMMIT: &str = env!("CAULDRON_SELF_COMMIT");

/// The human suffix `--version` puts after the commit: nothing for `0`,
/// ` (dirty)` for `1`, and ` (dirt unknown)` for ANYTHING else — never the
/// empty suffix, because an empty suffix reads as clean and the build script
/// could not establish that (R1.S3b: the dirt flag is fail-closed).
pub(crate) fn dirt_suffix(dirty: &str) -> &'static str {
    match dirty {
        "0" => "",
        "1" => " (dirty)",
        _ => " (dirt unknown)",
    }
}

/// The one line `cauldron --build-stamp` prints: `commit=<sha>
/// provenance=<pin|git|unknown> dirty=<1|0|unknown> dirt_scope=<a,b,...>`,
/// every value compiled in by
/// `cli/build.rs`. Whitespace-separated `key=value`, the format `Stamp`
/// parses, so the supervisor can read its own stamp with the reader it
/// uses on everyone else — and so a stamp that grows a field later does
/// not stop an older reader.
///
/// `dirty=` is `1`, `0` or `unknown` and nothing else: a value the build script
/// did not produce is rendered `unknown`, never passed through as something a
/// reader might take for clean. `dirt_scope=` names what `dirty=0` covers.
pub(crate) fn self_stamp() -> String {
    let dirty = match env!("CAULDRON_SELF_DIRTY") {
        d @ ("0" | "1") => d,
        _ => "unknown",
    };
    format!(
        "commit={} provenance={} dirty={} dirt_scope={}",
        SELF_COMMIT,
        env!("CAULDRON_SELF_PROVENANCE"),
        dirty,
        env!("CAULDRON_SELF_DIRT_SCOPE")
    )
}

/// Exec the app named `app`, looked for BESIDE this binary. `cauldron <name>`
/// is a router and nothing else: it finds the app's binary and hands it the
/// terminal. We never modify the app.
///
/// Deliberately NOT searched on PATH. Before the rename the app binary was
/// called `cauldron-tui`, a name nothing else on this machine uses, so a PATH
/// fallback was harmless. The app is now called `tui`, and `tui` on PATH is the
/// HARNESS launcher (`/opt/homebrew/bin/tui` -> `harness/scripts/tui`) — so a
/// PATH fallback would silently open a different program instead of reporting a
/// missing build. The app is a member of this workspace and is built beside the
/// router by `cargo build --workspace`; beside-the-binary is the whole answer.
///
/// `opens` is the environment the app will actually open, when the caller
/// knows it differs from the resolved name (V1.2b: the cauldron face opens
/// the last-used cauldron when nobody named one); its build is the one run.
fn app(app: &str, args: &[String], opens: Option<&str>) -> Result<i32> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            // R6.S8: a non-prod `install` puts its face at `<app>-<env>`
            // (`launchd::app_file_name`), so that is looked for first; the
            // plain name stays the fallback (a build dir, or an env never
            // installed).
            let name = opens.map(str::to_string).or_else(|| Environment::resolve().ok().map(|e| e.name));
            if let Some(name) = name {
                let own = launchd::app_file_name(app, &name);
                if own != app {
                    candidates.push(dir.join(own));
                }
            }
            candidates.push(dir.join(app));
        }
    }
    let mut tried = Vec::new();
    for c in &candidates {
        if !c.exists() {
            tried.push(c.display().to_string());
            continue;
        }
        // **THE ENVIRONMENT GOES THROUGH** (R7.S1c). `--env` was already
        // exported as `CAULDRON_ENV` by `take_env_flag`, and `CAULDRON_HOME`
        // is inherited; both are restated here so the face's environment is a
        // fact this call sets rather than one it happens to inherit. And the
        // router's OWN path goes as `CAULDRON_BIN`, so the face's restart
        // button (`cauldron --env <name> start threads`) runs THIS cauldron —
        // the one that resolved the environment — and not whichever one PATH
        // finds first.
        let mut cmd = Command::new(c);
        cmd.args(args);
        if let Ok(env) = Environment::resolve() {
            cmd.env("CAULDRON_ENV", &env.name).env("CAULDRON_HOME", &env.root);
            // …and where the name came from: `default` means nobody named
            // one, so the face may open the last one he used (V1.2, Q-F1).
            cmd.env("CAULDRON_ENV_SOURCE", env.name_source.as_str());
        }
        if let Ok(me) = std::env::current_exe() {
            cmd.env("CAULDRON_BIN", me);
        }
        match cmd.status() {
            Ok(st) => return Ok(st.code().unwrap_or(1)),
            Err(e) => tried.push(format!("{} ({e})", c.display())),
        }
    }
    bail!(
        "could not launch the Cauldron app `{app}`. Tried:\n{}\nBuild it with `cargo build -p \
         {app}` so it sits beside this binary.",
        tried.iter().map(|t| format!("  {t}")).collect::<Vec<_>>().join("\n")
    )
}

// ----------------------------------------------------------------- plumbing

struct Opts {
    json: bool,
    /// `envs --probe`: open a socket per registered seam instead of only
    /// reading the tables. Opt-in, because a LISTING of environments is a file
    /// read and must stay one — a verb that answers "which environments are
    /// there" by connecting to every process in all of them is a verb nobody
    /// can run while production is up.
    probe: bool,
    /// `build --allow-dirty`: produce a version from a working tree with
    /// uncommitted changes. Off by default, because the version identity is the
    /// commit and a dirty tree means the commit does not identify the bytes.
    allow_dirty: bool,
    /// `build --env-copy` (REL-4.S8c): install the build as a NEW version
    /// `<label>+<env>` (first free of `+<env>`, `+<env>.2`, ...) instead of
    /// over `<label>`, which every environment in the root shares and another
    /// one may be pinned to. `scripts/install` uses it.
    env_copy: bool,
    /// `test --test-env <name>`: which environment the test runs in. Never the
    /// caller's — see `verb_test`.
    test_env: Option<String>,
    /// `build --snapshot-declaration <service> <label>`: the BACKFILL verb.
    /// Writes the CURRENT shipped (or root-override) declaration into an
    /// EXISTING build directory as its `declaration.toml` and records the
    /// hash. Builds nothing. Exists for build directories that predate
    /// R3.S2.0b, and only ever runs when the operator names the label.
    snapshot_declaration: bool,
    /// `--force`: with `--snapshot-declaration`, overwrite a snapshot that is
    /// already there. Refused without it, because the snapshot is the contract
    /// a pinned version launches under and replacing it is a cut-over act.
    force: bool,
    services: Vec<String>,
}

/// The test environment `cauldron test` uses when none is named.
const DEFAULT_TEST_ENV: &str = "test";

fn parse(args: &[String]) -> Result<Opts> {
    let mut o = Opts {
        json: false,
        probe: false,
        allow_dirty: false,
        env_copy: false,
        test_env: None,
        snapshot_declaration: false,
        force: false,
        services: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--json" => o.json = true,
            "--probe" => o.probe = true,
            "--allow-dirty" => o.allow_dirty = true,
            "--env-copy" => o.env_copy = true,
            "--snapshot-declaration" => o.snapshot_declaration = true,
            "--force" => o.force = true,
            "--test-env" => {
                // Two-token form. A flag that takes a value and finds none is
                // an error, never an empty value: `--test-env` with nothing
                // after it must not silently mean the default, because the
                // default is a DIFFERENT environment from the one the operator
                // was in the middle of naming.
                i += 1;
                let v = args.get(i).with_context(|| {
                    "`--test-env` needs an environment name after it".to_string()
                })?;
                o.test_env = Some(v.clone());
            }
            s if s.starts_with("--test-env=") => {
                o.test_env = Some(s["--test-env=".len()..].to_string());
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            s if s.starts_with('-') => bail!("unknown option `{s}`\n\n{USAGE}"),
            s => o.services.push(s.to_string()),
        }
        i += 1;
    }
    Ok(o)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A per-start boot token. Not a secret and not cryptographic: it only has to
/// be different from the last one, so that a listener answering the previous
/// token is recognised as the previous instance.
pub(crate) fn mint_token() -> String {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let pid = std::process::id() as u128;
    let addr = &mint_token as *const _ as u128;
    env::fingerprint::fingerprint(
        format!("{t}:{pid}:{addr}").as_bytes(),
    )
}

/// A declaration that does not load is REPORTED and SKIPPED by a bare verb,
/// never fatal to it: one broken file must not take every other service's
/// `status`, `start` or `stop` down with it. This is the choice the daemon
/// already makes per pinned service (`daemon.rs`, the `pinned` loop); the CLI
/// used to collect the whole scan through `?` and die on the first.
///
/// Naming the service explicitly (`cauldron start broken-one`) still fails
/// loudly through `decl::load` — the skip is only for "every declared one".
fn report_broken(scan: &decl::Scan) {
    for (name, e) in &scan.broken {
        eprintln!("cauldron: skipping `{name}` — its declaration does not load: {e:#}");
    }
}

/// The services a verb acts on: the ones named, or every declared one.
fn targets(env: &Environment, named: &[String]) -> Result<Vec<Declaration>> {
    if named.is_empty() {
        let scan = decl::all(&env.service_dir());
        report_broken(&scan);
        if scan.ok.is_empty() {
            bail!(
                "no services are declared{}. Declarations are looked for in:\n{}",
                if scan.broken.is_empty() {
                    String::new()
                } else {
                    format!(
                        " that load ({} declared, every one broken — see above)",
                        scan.broken.len()
                    )
                },
                decl::search_paths(&env.service_dir())
                    .iter()
                    .map(|p| format!("  {}", p.display()))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
        Ok(scan.ok)
    } else {
        named.iter().map(|n| decl::load(&env.service_dir(), n)).collect()
    }
}

/// **The services a verb acts on when none was named.**
///
/// This used to be `targets` minus the ADOPTED ones, with a printed line
/// saying what it had skipped, because `threads` and `tts` were declared and
/// cauldron could neither build nor start them. There is nothing to skip now:
/// cauldron hosts every service it declares, so "every declared service" means
/// what it says, and this is a wrapper kept only so the call sites still read
/// as a statement about ownership.
fn owned_targets(env: &Environment, named: &[String], _verb: &str) -> Result<Vec<Declaration>> {
    targets(env, named)
}

/// **CREATE `env.toml` ON FIRST USE, PINNING NOTHING.**
///
/// # Presence is the pin, and this function is where that became true
///
/// His S-7 ruling is that a service runs in an environment **if and only if it
/// is pinned there**, so there is no second list and nothing to keep in step.
/// This function used to contradict it: it added EVERY declared service to a
/// new environment's `env.toml` pinned to `latest`, which made "declared" and
/// "pinned here" the same thing and left presence with nothing to say.
///
/// That was harmless while every declaration was a testkit fixture. It stopped
/// being harmless the moment `threads` and `tts` became hosted services:
/// a fresh scratch environment then pinned the operator's ledger daemon and
/// his voice daemon, and a bare `cauldron start` in it tried to fork both.
///
/// **It was caught by the pre-fork bind check rather than by a review**, which
/// is the only reason it is a paragraph and not an incident: `prove-wave1`'s
/// bare `start` reached `threads`, and `launch_hosted` refused because
/// something was already listening on 3653 — the live daemon. That refusal did
/// its job; this function is why it had to.
///
/// So a new environment is now EMPTY, and the three things that put a pin in
/// it are all deliberate acts: `cauldron pin`, `cauldron start <service>`
/// (which pins what it was told to start), and `cauldron test`, which pins
/// into the environment it is testing in. This also closes IS-5, the testkit
/// hole, without a new concept.
fn ensure_manifest(env: &Environment, _decls: &[Declaration]) -> Result<Manifest> {
    let path = env.manifest();
    if !path.exists() {
        // The data root first. A manifest cannot exist without the directory
        // it names, and `pin` is reachable in an environment nothing has
        // created yet — which is the normal way a new environment begins now
        // that nothing is auto-pinned.
        guard::refuse_protected(&env.data_root)?;
        std::fs::create_dir_all(&env.data_root)
            .with_context(|| format!("creating {}", env.data_root.display()))?;
        ensure_test_window();
        // R9.C7f5 (C-7, inspect R9.C7f4 low 1): the existence check above is
        // only a fast path. The create re-checks UNDER the env.toml writers'
        // lock, so a concurrent `cauldron pin` that created and pinned since
        // that check keeps its pin instead of being overwritten by an empty
        // manifest.
        if !Manifest::create_if_absent(&path)? {
            return Manifest::load(&path);
        }
        let m = Manifest::load(&path)?;
        println!(
            "cauldron: created environment `{}` at {} — PINNING NOTHING. A service runs here \
             only if it is pinned here, so this environment hosts nothing until you say what it \
             runs (`cauldron pin <service> <version>`, or `cauldron start <service>`, which \
             pins what it starts).",
            env.name,
            path.display()
        );
        return Ok(m);
    }
    Manifest::load(&path)
}

/// **The services a BARE verb acts on in this environment: the ones it PINS.**
///
/// Not every declared service, which is what `owned_targets` means and what
/// every bare verb used to get. The distinction only started to matter when
/// cauldron began hosting the two real daemons: "every declared service"
/// then included the operator's ledger and his voice, in every environment,
/// including a scratch one.
///
/// `build` is deliberately NOT routed through this — see its own call site.
/// Building is environment-free: it installs a version into the shared store
/// and starts nothing, so "every declared service" is the right answer there
/// and is the only way a fresh environment can get something to pin.
fn pinned_targets(env: &Environment, named: &[String], verb: &str) -> Result<Vec<Declaration>> {
    if !named.is_empty() {
        return named.iter().map(|n| decl::load(&env.service_dir(), n)).collect();
    }
    let m = Manifest::load(&env.manifest())?;
    let names: Vec<String> = m.services.keys().cloned().collect();
    if names.is_empty() {
        bail!(
            "nothing is pinned in environment `{}`, so a bare `cauldron {verb}` has nothing to \
             act on. A service runs in an environment IF AND ONLY IF it is pinned there — \
             presence is the pin, and there is no second list. {}\n\
             Pin one with `cauldron pin <service> <version>` (`cauldron versions` shows what is \
             built), or name one explicitly: `cauldron {verb} <service>`.",
            env.name,
            {
                let scan = decl::all(&env.service_dir());
                report_broken(&scan);
                if !scan.ok.is_empty() {
                    format!(
                        "Services are DECLARED in this root — {} — but a declaration says what a \
                         service IS, not that this environment runs it.",
                        scan.ok.iter().map(|d| d.name.clone()).collect::<Vec<_>>().join(", ")
                    )
                } else if !scan.broken.is_empty() {
                    format!(
                        "{} declaration(s) exist in this root and none of them loads (see above).",
                        scan.broken.len()
                    )
                } else {
                    "No services are declared in this root at all.".to_string()
                }
            }
        );
    }
    names.iter().map(|n| decl::load(&env.service_dir(), n)).collect()
}

// ------------------------------------------------------------------- status// ------------------------------------------------------------------- status

#[derive(serde::Serialize)]
struct Row {
    service: String,
    state: String,
    detail: String,
    addr: Option<String>,
    pid: Option<u32>,
    /// **The version this environment pins, or `null` because it pins none.**
    ///
    /// `null` is the whole fix and the shape was chosen over two others:
    ///
    /// * NOT an omitted field. Omission would make one absence mean two
    ///   things — "this cauldron does not report pins" and "this environment
    ///   does not pin this service" — and `jq '.pinned'` cannot tell a
    ///   missing key from a null one, so a reader would have to reach for
    ///   `has("pinned")` to recover what the value could have said outright.
    /// * NOT an explicit word like `"unpinned"`. A string in the field where
    ///   a version label goes can be read AS a version label, which is
    ///   exactly the mistake `"latest"` was: `latest` is a legal pin, so a
    ///   reader could not tell the sentinel from the thing. `null` is the
    ///   only value in JSON that cannot be mistaken for a label.
    ///
    /// A row still exists for an unpinned service, because a declaration says
    /// what a service IS; `state` and `detail` say that this environment does
    /// not run it, and this field says it names no version.
    pinned: Option<String>,
    running_version: Option<String>,
    pid_alive: Option<bool>,
    probe: Option<String>,
    data_dir: Option<String>,
    /// The shape the DECLARATION named, or `false` where it declared none.
    warnings_shape: Option<String>,
    /// What the supervisor found when it ASKED. The declaration is a pointer;
    /// this is the evidence.
    warnings: Compliance,
    /// **Did the service write its own seam, or did the supervisor write one
    /// for it?**
    ///
    /// Read from the DECLARATION (`registers`) when there is no row yet, and
    /// from the ROW once there is, because they answer the same question at two
    /// different times: the declaration says what the binary is capable of and
    /// the row says what actually happened.
    registered: bool,
    /// Where the recorded pid came from. `self` for a row the service wrote,
    /// `child` for one the supervisor wrote about a process it forked — both
    /// proof, and nothing else is spellable.
    pid_source: Option<String>,
    /// **WHICH DECLARATION THIS SERVICE RUNS UNDER** — the snapshot in its
    /// build directory, or the shipped file because there is no snapshot.
    /// Named for the RUNNING version when there is a live row and for the
    /// PINNED version otherwise; `null` for a service this environment does
    /// not run. See `describe_declaration`.
    declaration: Option<String>,
    /// **WHO LAUNCHES IT.** `cauldron` when this environment pins it (the
    /// daemon hosts it), the `[migration]` section's `launched_by` when an
    /// outside launcher still does (`launchd:com.harness.aui`), and `null`
    /// when nothing in this environment is asked to run it.
    supervised_by: Option<String>,
    /// **A CALENDAR JOB'S NEXT SLOT** (R5.S5, Deploy5-2): `{unix, utc,
    /// local, due}`, computed by the holder's own due-ness function
    /// (`jobs::next_slot`). `null` for everything else, interval jobs
    /// included (their `detail` already says `next pass in Ns`).
    next_slot: Option<jobs::NextSlot>,
}

/// `outside:up` / `outside:down` for a registered service an outside launcher
/// still owns. A CONNECT to the declared address and nothing else: no row is
/// written, no pid is read, nothing is signalled. A `job` has no address and
/// is reported as `outside:job`.
fn outside_state(d: &Declaration) -> (String, String) {
    let Some(mg) = &d.migration else { return (SeamState::Down.as_str().into(), String::new()) };
    let Some(l) = &d.launch else {
        return (
            "outside:job".into(),
            format!(
                "registered, not hosted: a `{}` launched by `{}`. {}",
                d.kind,
                mg.launched_by,
                mg.refusal(&d.name).replace("REFUSED: ", "")
            ),
        );
    };
    let up = l
        .addr
        .parse::<std::net::SocketAddr>()
        .ok()
        .map(|sa| env::egress::connect_tcp("outside service", &sa, Duration::from_millis(300)).is_ok())
        .unwrap_or(false);
    (
        if up { "outside:up" } else { "outside:down" }.into(),
        format!(
            "registered, not hosted: {} at {} (connect check only). {}",
            if up { "something IS listening" } else { "NOTHING is listening" },
            l.addr,
            mg.refusal(&d.name).replace("REFUSED: ", "")
        ),
    )
}

/// **Which declaration version `label` of `service` launches under, as one
/// line for `status` and `envs`.** Never fails: a snapshot that cannot be
/// used is REPORTED here and REFUSED at launch (`declaration_for_build`),
/// and a reporting verb that died on the row it most needed to show would be
/// the wrong half of that pair.
///
/// The fixed phrases are load-bearing — `shipped (no snapshot)` is what the
/// operator greps for before a cut-over — and are proven in
/// `scripts/prove-declaration-snapshot`.
fn describe_declaration(env: &Environment, service: &str, label: &str) -> String {
    let dir = env.build_dir(service, label);
    let rec: BuildRecord = match std::fs::read_to_string(dir.join("build.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
    {
        Some(r) => r,
        None => return format!("UNBUILT — version {label} has no build.json in this root"),
    };
    match decl::snapshot(&dir, &rec, service) {
        Ok(decl::Snapshot::Present { sha256, source, .. }) => format!(
            "snapshot ({label}, sha256 {}, taken from {})",
            &sha256[..12],
            if source.is_empty() { "an unrecorded path" } else { &source }
        ),
        Ok(decl::Snapshot::Absent { recorded_sha256 }) if recorded_sha256.is_empty() => format!(
            "shipped (no snapshot) — version {label} predates declaration snapshots, so it \
             launches under whatever `services/{service}.toml` says TODAY. Backfill deliberately: \
             `cauldron build --snapshot-declaration {service} {label}`"
        ),
        Ok(decl::Snapshot::Absent { recorded_sha256 }) => format!(
            "shipped (no snapshot) — build.json records sha256 {} for a declaration.toml that \
             is GONE from {}; version {label} launches under the shipped file",
            &recorded_sha256[..12],
            dir.display()
        ),
        Err(e) => format!("snapshot UNUSABLE — version {label} will be REFUSED at launch: {e:#}"),
    }
}

fn examine(env: &Environment, table: &Table, m: &Manifest, d: &Declaration) -> Row {
    let pinned = m.pin(&d.name);
    let shape = d.warnings_shape().map(str::to_string);
    // The declaration the RUNNING version is under if one runs, else the one
    // the PIN would launch under. `latest` is resolved to a label first,
    // because a snapshot belongs to a build directory and `latest` is a
    // question rather than a directory.
    let declaration = |label: Option<String>| -> Option<String> {
        let label = label?;
        let label = if label == "latest" {
            decl::resolve_version(&env.builds_for(&d.name), "latest", &env.name).ok()?.0
        } else {
            label
        };
        Some(describe_declaration(env, &d.name, &label))
    };
    // Computed before `pinned` moves into the row below.
    let under_pin = declaration(pinned.clone());
    let supervised_by = match (&d.migration, &pinned) {
        (Some(mg), _) => Some(mg.launched_by.clone()),
        (None, Some(_)) => Some("cauldron".to_string()),
        (None, None) => None,
    };
    match table.seams.get(&d.name) {
        None if d.migration.is_some() => {
            let (state, detail) = outside_state(d);
            Row {
                service: d.name.clone(),
                state,
                detail,
                addr: d.launch.as_ref().map(|l| l.addr.clone()),
                pid: None,
                pinned,
                running_version: None,
                pid_alive: None,
                probe: Some(if d.launch.is_some() { "connect".into() } else { d.probe.clone() }),
                data_dir: None,
                warnings_shape: shape,
                warnings: Compliance::NoProducer { state: SeamState::Down.as_str().into() },
                registered: d.registers,
                next_slot: None,
                pid_source: None,
                declaration: under_pin,
                supervised_by,
            }
        }
        None => Row {
            service: d.name.clone(),
            state: SeamState::Down.as_str().into(),
            // TWO DIFFERENT ABSENCES, SAID DIFFERENTLY. "No seam in this
            // environment's table" is true of both but reads as "it should be
            // here and is not", which for an unpinned service is the same
            // conflation `pinned = "latest"` was: a declaration says what a
            // service IS, not that this environment runs it.
            detail: match &pinned {
                Some(v) => format!(
                    "pinned {v} here, but there is no seam for it in the table — nothing is \
                     running"
                ),
                None => format!(
                    "`{}` is not pinned in `{}`, so this environment does not run it. Presence \
                     is the pin and there is no second list; it is listed here because it is \
                     DECLARED in this root.",
                    d.name, env.name
                ),
            },
            addr: None,
            pid: None,
            pinned,
            running_version: None,
            pid_alive: None,
            probe: None,
            data_dir: None,
            warnings_shape: shape,
            // Nothing is running, so there is nothing to ask. A declaration
            // that says `warnings = false` is still reported as the violation
            // it is, because that one needs no process to be true.
            warnings: match d.warnings_shape() {
                None => Compliance::RefusedByDeclaration { service: d.name.clone() },
                Some(_) => Compliance::NoProducer { state: SeamState::Down.as_str().into() },
            },
            registered: d.registers,
            next_slot: None,
            pid_source: None,
            declaration: under_pin,
            supervised_by,
        },
        Some(s) => {
            let v: Verdict = seams::probe(s);
            // THE SUPERVISOR AS AN ORDINARY CONSUMER: a second short-lived
            // connection to the same socket, one word, one line back. It never
            // asks for anything but warnings, and it never relays.
            let w = warn::check(s, &v, d.warnings_shape());
            Row {
                service: d.name.clone(),
                state: v.state.as_str().into(),
                detail: v.detail,
                addr: Some(s.addr.clone()),
                pid: Some(s.pid),
                pinned,
                running_version: Some(s.version.clone()),
                pid_alive: v.pid_alive,
                probe: Some(s.probe.clone()),
                data_dir: Some(s.data_dir.clone()),
                warnings_shape: shape,
                warnings: w,
                // THE ROW, not the declaration: by the time a row exists,
                // which function wrote it is a fact and the declaration is
                // only a statement of intent. A disagreement between them —
                // `registers = true` in the file and `registered = false` on
                // the row — is real news and is shown as the row has it,
                // because the row is the one that happened.
                registered: s.registered,
                next_slot: None,
                pid_source: Some(s.pid_source.clone()),
                declaration: declaration(Some(s.version.clone())),
                supervised_by,
            }
        }
    }
}

/// `status` is the aggregate: a PULL, on demand, with no process.
///
/// # Why the aggregate lives here and not in a daemon
///
/// Wave 1 is a CLI, and the thing that makes a CLI sufficient is that **this
/// supervisor is an ordinary consumer**. It does not need to have been watching
/// in order to know the number, because the producers keep their own counters
/// and `counts_since` says what window they cover. So the aggregate is exactly
/// what the producers say when asked, and asking is cheap: one connection per
/// live service, one word, one line.
///
/// A daemon would buy two things this does not have — a number that changes
/// without being asked for, and a history — and would cost the one thing wave 1
/// was built to avoid: a long-lived process that can itself die, with nothing
/// supervising the supervisor. **Nothing long-lived was introduced.** The
/// honest limit is stated where the operator will read it: the aggregate is as
/// current as the last `status`.
fn verb_status(args: &[String]) -> Result<i32> {
    let o = parse(args)?;
    let env = Environment::resolve()?;
    let decls = targets(&env, &o.services)?;
    let m = Manifest::load(&env.manifest())?;
    let table = Table::load(&env.name, &env.data_root, &env.seams())?;
    let rows: Vec<Row> = decls
        .iter()
        .map(|d| {
            let mut r = examine(&env, &table, &m, d);
            jobs::overlay(&env, &m, d, &mut r.state, &mut r.detail, &mut r.pid, &mut r.next_slot);
            r
        })
        .collect();

    // THE AGGREGATE. Folded from what the producers answered, with every
    // service that did not answer carried beside the total as a named gap —
    // because a total whose coverage is unknown is the 47 MB log again: a
    // number that looks complete and is not.
    // R9.C7c: what a pending re-pin is waiting on, read off each running
    // service's own restart gate — the drain, or the quiet signal it awaits.
    // R9.C7b2: and what the HOLDER decided — its recorded deferrals (since
    // when, why) and its parks — read once off its own `STATUS`.
    let holder = holder_status(&env);
    let gates: Vec<(String, String, serde_json::Value)> = rows
        .iter()
        .filter_map(|r| {
            let held = holder_row(holder.as_ref(), &r.service);
            repin_wait(&env, &m, &table, r, held)
                .or_else(|| parked_wait(held))
                .map(|(l, j)| (r.service.clone(), l, j))
        })
        .collect();

    let mut agg = Aggregate::default();
    for r in &rows {
        agg.add(
            &r.service,
            r.warnings_shape.as_deref().unwrap_or("<none declared>"),
            &r.warnings,
        );
    }

    if o.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "environment": &env,
                "seam_table": table.path().display().to_string(),
                "services": rows,
                "warnings": &agg,
                "restart_gates": gates
                    .iter()
                    .map(|(s, _, j)| (s.clone(), j.clone()))
                    .collect::<serde_json::Map<_, _>>(),
            }))?
        );
    } else {
        println!("{}", env.banner());
        println!("seam table {}", table.path().display());
        // RUNNING is 22 and not 17 because a version is a COMMIT now (1a73f71)
        // and a commit-labelled version does not fit in 17. Measured on the two
        // real ones: threads' corrected `eee8e1c46694-release` is 20 and tts's
        // `unstamped-302a1f2423ef` is 22, so at 17 both of them shoved PROBE out
        // of its column. Rust's width is a MINIMUM, never a truncation, so this
        // was never a clipping bug — it was a silently ragged table, which is
        // the kind of thing nobody files and everybody reads.
        println!(
            "{:<10} {:<18} {:<26} {:<22} {:<8} {:<17} {:<22} {}",
            "SERVICE", "STATE", "SUPERVISED BY", "ADDR", "PID", "PINNED", "RUNNING", "PROBE"
        );
        for r in &rows {
            println!(
                "{:<10} {:<18} {:<26} {:<22} {:<8} {:<17} {:<22} {}",
                r.service,
                r.state,
                r.supervised_by.clone().unwrap_or_else(|| "-".into()),
                r.addr.clone().unwrap_or_else(|| "-".into()),
                r.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                // `-`, the same word ADDR, PID and RUNNING already use for an
                // absence in this table, rather than the `latest` this column
                // used to print for a service the environment does not run.
                r.pinned.clone().unwrap_or_else(|| "-".into()),
                r.running_version.clone().unwrap_or_else(|| "-".into()),
                match (&r.probe, r.registered) {
                    (Some(p), _) if r.state.starts_with("outside:") => format!("{p} (outside)"),
                    (Some(p), false) => format!("{p} (hosted)"),
                    (Some(p), true) => p.clone(),
                    (None, _) => "-".into(),
                },
            );
        }
        for r in &rows {
            if !r.detail.is_empty() && r.state != "down" {
                println!("\n{}: {}", r.service, r.detail);
            }
        }
        for (svc, line, _) in &gates {
            println!("\n{svc}: {line}");
        }
        // WHICH DECLARATION, per service that this environment runs or pins.
        // One line each, because the phrase `shipped (no snapshot)` is the
        // one an operator has to see before he lands a changed declaration.
        let mut said = false;
        for r in &rows {
            if let Some(dcl) = &r.declaration {
                if !said {
                    println!();
                    said = true;
                }
                println!("{}: declaration: {dcl}", r.service);
            }
        }
        print_warnings(&agg);
    }
    // Exit 3 when the table lies. `down` is a true statement and exits 0.
    //
    // Exit 4 when a RUNNING service will not serve warnings. A separate code
    // from 3 because it is a separate fault: 3 says the table is wrong about
    // where a service is, 4 says a service is where it claims to be and is not
    // a well-formed cauldron service. `start` refuses that case, so reaching it
    // means the process was started by something other than `cauldron start` —
    // which is exactly when the supervisor has to be the one that says so.
    let lying = rows.iter().any(|r| {
        matches!(r.state.as_str(), "stale:no-listener" | "stale:wrong-owner" | "stale:wrong-env")
    });
    Ok(if lying {
        3
    } else if agg.violations > 0 {
        4
    } else {
        0
    })
}

/// The aggregate, as the operator reads it.
///
/// Shaped by one rule learned the expensive way: **a number that is always on
/// screen becomes wallpaper.** 27,705 warnings went unread for five weeks in a
/// log nobody opened, so the quiet case here is one short line and the loud
/// case is the only one that spends vertical space.
fn print_warnings(agg: &Aggregate) {
    let (answered, asked) = agg.coverage();
    println!();
    println!(
        "warnings  {} warn, {} error across {answered} of {asked} declared services \
         (pulled now; no daemon)",
        agg.warn, agg.error
    );
    for p in &agg.producers {
        // Only producers with something to report get a line. A row of zeroes
        // per service is the wallpaper.
        if p.total == 0 && p.sink_errors == 0 {
            continue;
        }
        println!(
            "  {:<10} warn {:<6} error {:<6} since {}{}",
            p.service,
            p.warn,
            p.error,
            p.counts_since,
            if p.sink_errors > 0 {
                format!("  [{} FAILED writes to its own durable record]", p.sink_errors)
            } else if !p.sink_attached {
                "  [no durable record attached]".to_string()
            } else {
                String::new()
            }
        );
    }
    if !agg.top_kinds.is_empty() {
        // By CALL SITE, not by message text. One escalating `error!` in the
        // live threads daemon wore 18 different message strings; by text that
        // is 18 kinds and the breakdown is all one defect.
        println!("  busiest call sites:");
        for k in agg.top_kinds.iter().take(5) {
            println!(
                "    {:>6}  {:<5} {}:{}  {}",
                k.count, k.level, k.target, k.line, k.message
            );
        }
    }
    for g in &agg.gaps {
        if g.kind == "violation" {
            println!("  {} IS NOT SERVING WARNINGS: {}", g.service, g.reason);
        }
    }
    // ──────── THE HOSTED GAPS, PRINTED EVERY TIME, BY NAME ───────────────
    //
    // This loop is the price of hosting a binary that does not link cauldron's
    // `env` crate, paid out loud. Such a process never satisfied the
    // compile-time warnings obligation and cannot retroactively, so the total
    // does not include it — and a total that excludes something silently is
    // the 47 MB log again.
    //
    // It is printed at the SAME volume as a violation and in a different
    // sentence, because the two are different faults and the difference
    // matters to whoever reads it: a violation is somebody's bug to fix, and
    // this is a boundary of what cauldron can know about a binary somebody
    // else compiled.
    for g in &agg.gaps {
        if g.kind == "unregistered" {
            println!(
                "  {} IS HOSTED AND NOT COVERED BY THIS TOTAL: {}",
                g.service, g.reason
            );
        }
    }
    // ONE sentence for every kind of hole, from the aggregate itself, so no
    // face can report this number while omitting what it leaves out. See
    // `Aggregate::incompleteness`.
    //
    // **PRINTED FOR FAULTS AND FOR ADOPTIONS, NOT FOR MERE ABSENCE** — and the
    // distinction is this function's own rule about wallpaper, applied to the
    // new case. A service that is simply not running is already stated, exactly
    // and quietly, by the `{answered} of {asked}` line above, and it resolves
    // itself the moment somebody starts it; shouting about it would put a
    // permanent loud line on a fresh environment's status and teach the
    // operator to skip the loud line. A VIOLATION and an ADOPTION are both
    // structural: one is a breach that needs fixing, the other a boundary of
    // what cauldron can know, and neither goes away by starting something.
    // `Aggregate::incompleteness` still describes every hole including absence,
    // so a face that wants the full sentence has it; this is the one face
    // deciding when to raise its voice.
    if agg.violations > 0 || agg.unregistered > 0 {
        if let Some(why) = agg.incompleteness() {
            println!("  -> {why}");
        }
    }
}


// --------------------------------------------------------------------- envs

/// One registered seam, seen from OUTSIDE its own environment.
#[derive(serde::Serialize)]
struct EnvRow {
    env: String,
    service: String,
    addr: String,
    pid: u32,
    /// The commit this instance is running, which is the version identity.
    /// Empty only for a row written before commits were the identity, or for a
    /// hosted image that cannot state its own commit.
    commit: String,
    dirty: bool,
    /// The build label — commit-derived for anything cauldron built.
    version: String,
    data_root: String,
    registered: bool,
    /// Only with `--probe`.
    state: Option<String>,
    /// Which declaration the RUNNING version is under: `snapshot`,
    /// `shipped (no snapshot)`, `snapshot UNUSABLE` or `unbuilt`. The short
    /// form of `status`'s `declaration` line, because this is a table.
    declaration: String,
}

#[derive(serde::Serialize)]
struct EnvReport {
    root: String,
    environments: Vec<String>,
    reserved: &'static [&'static str],
    seams: Vec<EnvRow>,
    /// Service names registered in MORE THAN ONE environment at once, which is
    /// the capability the operator asked for and the thing a single-environment
    /// listing cannot show.
    shared_names: Vec<String>,
}

/// **`cauldron envs` — every environment in this root, side by side.**
///
/// # Why this verb exists at all, when the registry was already keyed
///
/// The seam table has been per-environment since wave 1: it lives at
/// `data/<env>/seams.json`, every entry restates the `env` and `data_root` it
/// was written under, and [`env::seams::Table::load`] refuses a table whose
/// contents disagree with the directory it was loaded from. Two services of the
/// same name in two environments were therefore already two rows in two files
/// that cannot be confused for one another, and `require_inside` already kept
/// each out of the other's data root. **The key was never missing.**
///
/// What was missing is a READER. Every verb resolved exactly one environment
/// from the process environment and could only ever see that one, so the
/// property "both are running and neither can reach the other" was true on disk
/// and invisible from every command. An isolation guarantee nobody can look at
/// is one an agent will breach by accident and then not notice — which is
/// precisely what happened on the night a test set `AUI_THREADS_PORT` and not
/// `AUI_THREADS_CHASSIS_PORT`.
///
/// So this verb reads the tables rather than the process environment, and it
/// names `shared_names`: the services registered in more than one environment
/// at once. That list is the answer to "are they really separate?", and it is
/// computed from the files rather than claimed.
///
/// # Why it does not probe by default
///
/// A listing of environments is a file read. Connecting to every registered
/// process in every environment in order to answer "which environments exist"
/// would make this verb unrunnable while production is up — and production is
/// always up. `--probe` asks for the sockets explicitly.
fn verb_envs(args: &[String]) -> Result<i32> {
    let o = parse(args)?;
    if !o.services.is_empty() {
        bail!(
            "`cauldron envs` takes no service argument (got `{}`). It lists ENVIRONMENTS; \
             `cauldron status [service]` is the per-service view inside one.",
            o.services.join(", ")
        );
    }
    let here = Environment::resolve()?;
    let names = here.environments_including_self()?;

    let mut seams: Vec<EnvRow> = Vec::new();
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for name in &names {
        // CONSTRUCTED, never switched to. See `Environment::for_name`: reading
        // a second environment by mutating this process's `CAULDRON_ENV` is how
        // a reader becomes a writer.
        // A name whose data root is refused (an alias onto another
        // environment's, R6.S3) is REPORTED and skipped, like a refused table.
        let e = match here.for_name(name) {
            Ok(e) => e,
            Err(err) => {
                eprintln!("cauldron: environment `{name}`: {err:#}");
                continue;
            }
        };
        let table = match Table::load(&e.name, &e.data_root, &e.seams()) {
            Ok(t) => t,
            Err(err) => {
                // A refused table is REPORTED, not fatal. One corrupt or
                // misplaced table in one environment must not blind the
                // operator to the other environments — and the refusal text is
                // the most useful thing on the screen when it happens.
                eprintln!("cauldron: environment `{name}`: {err}");
                continue;
            }
        };
        for s in table.seams.values() {
            *counts.entry(s.service.clone()).or_default() += 1;
            seams.push(EnvRow {
                env: e.name.clone(),
                service: s.service.clone(),
                addr: s.addr.clone(),
                pid: s.pid,
                commit: s.commit.clone(),
                dirty: s.dirty,
                version: s.version.clone(),
                data_root: s.data_root.clone(),
                registered: s.registered,
                state: if o.probe { Some(seams::probe(s).state.as_str().to_string()) } else { None },
                declaration: if s.service == daemon::SELF_SERVICE {
                    // The supervisor is not installed by `cauldron build`
                    // and has no build directory to carry anything.
                    "-".to_string()
                } else {
                    let long = describe_declaration(&e, &s.service, &s.version);
                    if long.starts_with("snapshot (") {
                        "snapshot".to_string()
                    } else if long.starts_with("shipped (no snapshot)") {
                        "shipped (no snapshot)".to_string()
                    } else if long.starts_with("snapshot UNUSABLE") {
                        "snapshot UNUSABLE".to_string()
                    } else {
                        "unbuilt".to_string()
                    }
                },
            });
        }
    }
    let shared_names: Vec<String> =
        counts.iter().filter(|(_, n)| **n > 1).map(|(k, _)| k.clone()).collect();

    let report = EnvReport {
        root: here.root.display().to_string(),
        environments: names.clone(),
        reserved: env::paths::RESERVED_ENV_NAMES,
        seams,
        shared_names,
    };
    if o.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(0);
    }

    println!("root {}", report.root);
    println!(
        "environments  {}   (reserved, never an environment: {})",
        if report.environments.is_empty() { "-".to_string() } else { report.environments.join(" ") },
        report.reserved.join(", ")
    );
    println!("standing in   {} (from {})", here.name, here.name_source.as_str());
    println!();
    println!(
        "{:<10} {:<10} {:<22} {:<8} {:<14} {:<22} {}",
        "ENV", "SERVICE", "ADDR", "PID", "COMMIT", "DECLARATION", if o.probe { "STATE" } else { "DATA ROOT" }
    );
    for r in &report.seams {
        let commit = if r.commit.is_empty() {
            "-".to_string()
        } else {
            let short: String = r.commit.chars().take(12).collect();
            if r.dirty { format!("{short}+dirty") } else { short }
        };
        println!(
            "{:<10} {:<10} {:<22} {:<8} {:<14} {:<22} {}",
            r.env,
            if r.registered { r.service.clone() } else { format!("{} (hosted)", r.service) },
            r.addr,
            r.pid,
            commit,
            r.declaration,
            r.state.clone().unwrap_or_else(|| r.data_root.clone())
        );
    }
    if report.seams.is_empty() {
        println!("(nothing is registered in any environment in this root)");
    }
    println!();
    if report.shared_names.is_empty() {
        println!(
            "no service is registered in more than one environment at once. \
             (That is a fact about right now, not a limit: see below.)"
        );
    } else {
        println!(
            "registered in MORE THAN ONE environment at once: {}",
            report.shared_names.join(", ")
        );
        println!(
            "  -> each of those is a separate process with its own address, its own data root, \
             and its own row in its own environment's table. Neither can open the other's data \
             root: `require_inside` refuses it structurally, not by name."
        );
    }
    Ok(0)
}

// ════════════════════════════════════════════ ENVIRONMENTS AS A FIRST-CLASS VERB

/// **`--env <name>` / `--env=<name>`, taken off the FRONT of the argument
/// list, before the verb.**
///
/// Only leading, deliberately: after the verb, arguments belong to the verb —
/// and for `tui` they pass straight through to another program — so a flag
/// stripped from the middle could be somebody else's `--env`. A verb's own
/// parser refuses an unknown `--env`, so a misplaced flag is loud, never
/// silently ignored.
///
/// The name is recorded twice, on purpose: [`env::paths::set_env_flag`] so the
/// banner can say it came from `--env`, and `CAULDRON_ENV` in this process's
/// environment so a face this binary EXECS (the TUI) inherits the same
/// environment rather than resolving `prod`. Services are handed `CAULDRON_ENV`
/// explicitly by `start` and the daemon already, so they need neither.
fn take_env_flag(mut argv: Vec<String>) -> Result<Vec<String>> {
    loop {
        let name = match argv.first().map(|s| s.as_str()) {
            Some("--env") => {
                let v = argv.get(1).cloned().with_context(|| {
                    "`--env` needs an environment name after it. An empty flag is refused \
                     rather than read as the default, because the default is `prod`."
                        .to_string()
                })?;
                argv.drain(..2);
                v
            }
            Some(a) if a.starts_with("--env=") => {
                let v = a["--env=".len()..].to_string();
                argv.remove(0);
                v
            }
            _ => return Ok(argv),
        };
        env::paths::set_env_flag(&name)?;
        // Single-threaded here: nothing has been spawned yet. Edition 2021, so
        // `set_var` is a safe fn; it is still only called at this one point.
        std::env::set_var("CAULDRON_ENV", &name);
    }
}

/// **`cauldron env <list|create|show>`** — the environment as a thing you can
/// name, make and look at.
///
/// An ENVIRONMENT is a named instance of cauldron: its own data root
/// (`<root>/data/<name>/`), its own seam table, its own pins and supervision
/// policy (`env.toml`), its own logs, its own hold (one daemon per
/// environment, kernel-enforced), its own launchd label
/// (`<label_prefix>.cauldron-<name>`, default `ai.broomstick`) and process name. The default, `prod`,
/// is exactly the layout production already occupies; nothing about it moves.
///
/// Builds, declarations and model weights are per ROOT and shared — "build
/// once, pin per environment".
///
/// `env remove` (R6.S3) deletes a NON-prod environment's data root, dry run by
/// default and refused for `prod` by name — see `envs::verb_env_remove`.
fn verb_env(args: &[String]) -> Result<i32> {
    match args.first().map(|s| s.as_str()) {
        None | Some("list") | Some("ls") => verb_envs(args.get(1..).unwrap_or(&[])),
        Some("create") | Some("new") => envs::verb_env_create(&args[1..]),
        Some("show") => verb_env_show(&args[1..]),
        Some("remove") | Some("rm") => envs::verb_env_remove(&args[1..]),
        Some(other) => {
            eprintln!(
                "cauldron: unknown `env` subcommand `{other}`. Known: list, create <name> \
                 [--from <env> [--pins-only]] [--auto-restart], show [name], remove <name> \
                 [--stop] [--yes]."
            );
            Ok(2)
        }
    }
}

fn verb_env_show(args: &[String]) -> Result<i32> {
    if args.len() > 1 {
        bail!("`cauldron env show [name]` takes at most one name");
    }
    let here = Environment::resolve()?;
    let e = match args.first() {
        Some(n) if n.starts_with('-') => bail!("unknown option `{n}` for `cauldron env show`"),
        Some(n) => here.for_name(n)?,
        None => here,
    };
    print_env(&e)?;
    Ok(0)
}

pub(crate) fn print_env(e: &Environment) -> Result<()> {
    println!("{}", e.banner());
    println!("  data root      {}{}", e.data_root.display(), if e.data_root.exists() { "" } else { "  (not created yet)" });
    println!("  seam table     {}", e.seams().display());
    println!("  pins + policy  {}", e.manifest().display());
    println!("  logs           {}", e.data_root.join("logs").display());
    println!("  hold           {}", e.data_root.join(env::hold::HOLD_FILE).display());
    println!("  process name   {}", launchd::process_name(&e.name));
    println!("  launchd label  {}", launchd::label(e));
    println!("  launch path    {}", launchd::launch_path(e).display());
    println!("  shared (root)  builds {}  declarations {}", e.shared_root().join("builds").display(), e.service_dir().display());
    if e.manifest().exists() {
        let m = Manifest::load(&e.manifest())?;
        let pins: Vec<String> = m
            .services
            .iter()
            .map(|(k, p)| match &p.addr {
                Some(a) => format!("{k}={} @{a}", p.version),
                None => format!("{k}={}", p.version),
            })
            .collect();
        println!(
            "  services       {}",
            if pins.is_empty() { "(none pinned)".to_string() } else { pins.join("  ") }
        );
        println!("  auto_restart   {}", m.supervisor.auto_restart);
    }
    // Is a daemon holding it? Asked of the supervisor's OWN ROW by handshake,
    // never by taking the lock — a probe that took the hold would overwrite
    // the holder's note and, for an instant, BE a second holder.
    let held = match Table::load(&e.name, &e.data_root, &e.seams()) {
        Ok(t) => match t.seams.get(daemon::SELF_SERVICE) {
            Some(s) => {
                let v = seams::probe(s);
                format!("{} (pid {}, {})", v.state.as_str(), s.pid, s.addr)
            }
            None => "no supervisor row".to_string(),
        },
        Err(err) => format!("seam table unreadable: {err}"),
    };
    println!("  supervisor     {held}");
    Ok(())
}


// -------------------------------------------------------------------- build

fn verb_build(args: &[String]) -> Result<i32> {
    let o = parse(args)?;
    let env = Environment::resolve()?;
    // ROOT-LEVEL directories only (R6.S3). A build is per ROOT, not per
    // environment, and `ensure_dirs` also creates `data/<env>/` — so
    // `cauldron --env dev build` used to bring `dev` into existence as an
    // empty environment with no env.toml, after which `env create dev` was
    // refused as "already exists".
    env.ensure_shared_dirs()?;
    if !o.json {
        println!("{}", env.banner());
    }
    if o.snapshot_declaration {
        return backfill_snapshot(&env, &o);
    }
    if o.force {
        bail!("`--force` belongs to `cauldron build --snapshot-declaration`; a build has nothing to force.");
    }
    if o.env_copy {
        // Refused here, not discovered half-way: a name that cannot be read
        // back by `decl::env_copy_owner` would be a copy `latest` misfiles.
        if env.name.is_empty() || env.name.contains(['+', '.', '/']) {
            bail!(
                "`--env-copy` names the copy after the environment, and `{}` cannot be part of a \
                 version label (no `+`, `.` or `/`).",
                env.name
            );
        }
    }
    let decls = owned_targets(&env, &o.services, "build")?;
    let mut out = Vec::new();
    for d in &decls {
        let rec = build_one(&env, d, o.json, o.allow_dirty, o.env_copy)?;
        out.push(rec);
    }
    if o.json {
        println!("{}", build_json(&out)?);
    } else {
        for r in &out {
            println!(
                "cauldron: built {} as version {} (commit {}, package {}, profile {}) -> {}",
                r.service,
                r.label,
                r.commit_short(),
                r.package,
                r.profile,
                env.build_dir(&r.service, &r.label).join(&r.bin).display()
            );
        }
    }
    Ok(0)
}

/// **`cauldron build --json`'s stdout: the build records, as a JSON array.**
///
/// `scripts/install` (REL-4.S8) reads the version label out of this to pin
/// `threads`, matching `"label": "<label>"` — so the key name is a contract
/// with that script, pinned by `install_contract::build_json_names_the_label`.
fn build_json(records: &[BuildRecord]) -> Result<String> {
    Ok(serde_json::to_string_pretty(records)?)
}

/// **The version's name, derived from the commit.**
///
/// This function is the operator's ruling in one place: *"we should have a
/// version that does not rely on attestation - likely the commit."*
///
/// Twelve characters of the commit, because that is short enough to read in a
/// table column and long enough that a collision in one repository is not a
/// thing that happens. The FULL commit is recorded in the build record and on
/// the seam; the label is the directory name and the thing a human types.
///
/// # Why the PROFILE is in the label too
///
/// Because a commit names the SOURCE and not the artifact, and this workspace
/// already has a service that depends on the difference: `threads` must be
/// built debug for structural reasons while `tally` is built release with
/// assertions on, and the profile is per service precisely so that nothing
/// forces one. A debug and a release build of one commit are two different
/// binaries with different behaviour, so calling them one version would mean
/// `cauldron pin` could not express the choice between them — and a rollback to
/// "that commit" would be a rollback to whichever of the two was installed
/// last.
///
/// Both halves are DECLARED FACTS rather than digests, which is what keeps this
/// a name and not a fingerprint: the commit comes from the repository and the
/// profile from the service's own declaration, so anybody can reconstruct the
/// label from the two things that determined the build.
///
/// # Why a dirty build gets a fingerprint and a clean one does not
///
/// `+dirty` is the honest statement that the commit does not identify these
/// bytes. The fingerprint after it is what stops two different dirty builds of
/// the same commit and profile from being one directory — without it the second
/// build would silently overwrite the first and the label would lie about which
/// one was installed. A clean build needs no tiebreaker because the commit and
/// the profile already determine it; if they ever fail to, that is a
/// REPRODUCIBILITY fault worth reporting rather than a naming problem worth
/// papering over, and `build_one` reports it.
fn label_for(commit: &str, profile: &str, dirty: bool, content: &str) -> String {
    let short: String = commit.chars().take(12).collect();
    if dirty {
        format!("{short}-{profile}-dirty.{}", content.chars().take(8).collect::<String>())
    } else {
        format!("{short}-{profile}")
    }
}

/// **Where a build lands, and under which label** (REL-4.S8c).
///
/// Without `--env-copy`: `<root>/data/shared/builds/<service>/<label>/`, the
/// version directory every environment in the root shares — a rebuild of the
/// same commit and profile renames new bytes over it.
///
/// With `--env-copy`: a NEW directory `<label>+<env>` (then `+<env>.2`, ...),
/// CLAIMED with `create_dir`, which fails on an existing name instead of
/// reusing it — so the build never writes into `<label>`, nor into any
/// directory that existed before this call (another environment's pin, an
/// earlier copy of this one). Found by inspect REL-4.S8b-I N1: `scripts/install`
/// ran a plain `build` for a NEW environment and so replaced the image prod
/// was pinned to, at the same commit, with an unsigned one.
fn claim_build_dir(
    env: &Environment,
    service: &str,
    label: &str,
    env_copy: bool,
) -> Result<(String, PathBuf)> {
    if !env_copy {
        return Ok((label.to_string(), env.build_dir(service, label)));
    }
    let parent = env.builds_for(service);
    guard::refuse_protected(&parent)?;
    std::fs::create_dir_all(&parent)?;
    claim_env_copy_dir(&parent, label, &env.name)
}

/// The claim itself, over a plain directory so it is testable on its own.
fn claim_env_copy_dir(parent: &Path, label: &str, env_name: &str) -> Result<(String, PathBuf)> {
    for n in 1..=999u32 {
        let l = decl::env_copy_label(label, env_name, n);
        let dir = parent.join(&l);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok((l, dir)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("creating {}", dir.display())),
        }
    }
    bail!("999 copies of `{label}` for environment `{env_name}` already exist in {}", parent.display())
}

#[cfg(test)]
mod env_copy_tests {
    use super::claim_env_copy_dir;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "cauldron-envcopy-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The shared `<label>` and an earlier `+dev` exist: the claim takes
    /// `+dev.2`, a directory that did not exist, and leaves both untouched.
    /// Opposed: in an empty parent it takes the bare `+dev` (so a claim that
    /// always skips the first name fails too).
    #[test]
    fn a_copy_claims_a_new_directory_never_the_shared_label_or_an_existing_copy() {
        let p = scratch("taken");
        for l in ["abc-release", "abc-release+dev"] {
            std::fs::create_dir_all(p.join(l)).unwrap();
            std::fs::write(p.join(l).join("build.json"), l).unwrap();
        }
        let (l, d) = claim_env_copy_dir(&p, "abc-release", "dev").unwrap();
        assert_eq!(l, "abc-release+dev.2");
        assert_eq!(d, p.join("abc-release+dev.2"));
        assert!(d.is_dir());
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0, "the claimed directory is new and empty");
        for l in ["abc-release", "abc-release+dev"] {
            assert_eq!(std::fs::read_to_string(p.join(l).join("build.json")).unwrap(), l);
        }
        let q = scratch("empty");
        let (l, _) = claim_env_copy_dir(&q, "abc-release", "dev").unwrap();
        assert_eq!(l, "abc-release+dev");
        assert!(!q.join("abc-release").exists(), "the shared label was created");
        let _ = std::fs::remove_dir_all(&p);
        let _ = std::fs::remove_dir_all(&q);
    }
}

/// Read-only: is the source workspace's tree dirty?
///
/// `--porcelain` over the whole worktree, including untracked files. Untracked
/// files count because a new `.rs` file that the build compiled is exactly the
/// change a commit does not contain, and the question being asked is whether
/// the commit identifies what was built.
///
/// `None` means the question could not be answered — not a repository, no git,
/// a broken index. `None` is never read as clean: see `build_one`, which
/// refuses it.
fn git_dirty(ws: &Path) -> Option<bool> {
    let out = Command::new("git")
        .arg("-C")
        .arg(ws)
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

fn build_one(
    env: &Environment,
    d: &Declaration,
    quiet: bool,
    allow_dirty: bool,
    env_copy: bool,
) -> Result<BuildRecord> {
    let b = d.build()?;

    // ══ AN ARTIFACT: NO COMPILER, AND THE COMMIT COMES OFF THE BYTES ══════
    //
    // First, before any workspace is resolved, because there is no workspace:
    // an artifact build has no cargo invocation to direct anywhere, and
    // resolving a tree it will never read would put a misleading path in any
    // error that followed.
    if let Some(artifact) = &b.artifact {
        return install_artifact(env, d, b, Path::new(artifact), quiet, env_copy);
    }

    // ── WHICH TREE, AND WHY IT IS NOT A LITERAL ANY MORE ────────────────
    //
    // `Declaration::workspace_for` resolves `[build] workspace` when the
    // declaration names one and otherwise the repository the declaration file
    // is sitting in. The literal it replaced — `/Users/testuser/code/cauldron`
    // in `echo.toml` and `tally.toml` — named the MAIN CHECKOUT from every git
    // worktree of this repo, so a build run in a worktree compiled,
    // fingerprinted and stamped a sibling's HEAD as its own commit. Under the
    // ruling that the commit IS the version identity that is the mechanism
    // lying, in the one direction a reader cannot catch.
    let ws = guard::refuse_protected(&d.workspace_for(b)?)
        .with_context(|| format!("service `{}` builds from", d.name))?;
    let manifest = ws.join("Cargo.toml");
    if !manifest.exists() {
        bail!("service `{}`: no Cargo.toml at {}", d.name, manifest.display());
    }
    // The environment's own target dir unless the declaration names another.
    // Naming one does NOT make the target tree a launch path: the artifact is
    // copied into `data/shared/builds/<service>/<label>/` below and the
    // supervisor execs the copy, so the D-2 hazard (`target/release/threads`
    // being the live daemon's launch path) is unreachable from here.
    let target = match &b.target_dir {
        Some(t) => PathBuf::from(t),
        None => env.cargo_target(),
    };
    guard::refuse_protected(&target)?;
    std::fs::create_dir_all(&target)?;

    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--manifest-path")
        .arg(&manifest)
        .args([
            "-p",
            b.package.as_deref().expect("validated at load: a cargo source names a package"),
        ])
        .args(["--bin", &b.bin])
        .args(b.profile_args())
        .args(&b.args)
        .env("CARGO_TARGET_DIR", &target)
        // Keep the build area small: this is a dev machine that ran out of
        // disk twice. Debug info for tests is not wanted here.
        .env("CARGO_PROFILE_TEST_DEBUG", "0")
        .stdin(Stdio::null());
    if quiet {
        cmd.stdout(Stdio::null()).stderr(Stdio::inherit());
    }
    let status = cmd.status().with_context(|| {
        format!("running cargo build for service `{}` (is cargo on PATH?)", d.name)
    })?;
    if !status.success() {
        bail!("cargo build for service `{}` failed with {status}", d.name);
    }

    let artifact = target.join(b.target_subdir()).join(&b.bin);
    if !artifact.exists() {
        bail!(
            "service `{}`: cargo reported success but {} does not exist",
            d.name,
            artifact.display()
        );
    }
    // ══ THE VERSION IDENTITY ═══════════════════════════════════════════════
    //
    // The label used to be `fingerprint_file(&artifact)` and nothing else — a
    // 16-hex FNV-1a of the installed bytes. That is a name for *these bytes*,
    // and the operator's ruling is that the version must be *the commit*. The
    // fingerprint is still computed and still recorded, because it answers "are
    // these the same bytes" and will keep answering it; it is simply no longer
    // the name of the version.
    //
    // Both refusals below come AFTER cargo has succeeded, deliberately. A build
    // that cannot be named is still a build that compiled, and the operator
    // asked a question about identity, not about the compiler — so he gets the
    // compiler's answer first and then the refusal to INSTALL an unnameable
    // version. Nothing is written into the build tree before the name exists.
    let content = env::fingerprint::fingerprint_file(&artifact)?;
    let commit = git_head(&ws).filter(|c| !c.trim().is_empty()).with_context(|| {
        format!(
            "REFUSED to install a version of `{}`: `git rev-parse HEAD` in {} produced no commit, \
             so this build has no version identity. The version IS the commit — it is the one \
             name that is nameable, comparable and checkable by anybody with the repository, and \
             it is the name a rollback target has to have. A fingerprint of the bytes cannot \
             stand in for it: you cannot look one up, order it, diff it, or ask the repository \
             anything about it. Build from a git worktree.",
            d.name,
            ws.display()
        )
    })?;
    let dirty = git_dirty(&ws).with_context(|| {
        format!(
            "REFUSED to install a version of `{}`: could not tell whether {} has uncommitted \
             changes. That is not read as clean — a supervisor that cannot establish the tree \
             state and assumes the flattering answer is how a build gets labelled with a commit \
             it was not built from.",
            d.name,
            ws.display()
        )
    })?;
    if dirty && !allow_dirty {
        bail!(
            "REFUSED to install a version of `{}`: the working tree at {} has \
             uncommitted changes, so commit {} does NOT identify the binary that was \
             just built. The version identity is the commit; labelling these bytes with \
             that commit would put a claim in the registry that the repository \
             contradicts, and a rollback to it would restore something nobody can \
             reproduce.\n\
             Commit the change, or pass `--allow-dirty` to install it under a label that \
             says what it is (`{}`). Nothing was installed and cargo's output is still in \
             the build cache.",
            d.name,
            ws.display(),
            &commit[..commit.len().min(12)],
            label_for(&commit, &b.profile, true, &content)
        );
    }
    let label = label_for(&commit, &b.profile, dirty, &content);
    let (label, dir) = claim_build_dir(env, &d.name, &label, env_copy)?;
    // ── IS THIS LABEL REPRODUCIBLE? ─────────────────────────────────────
    //
    // The label is the commit plus the profile, so a clean rebuild of the same
    // commit at the same profile MUST land on the same label — and if the bytes
    // differ, the name has stopped identifying the thing. That is not a reason
    // to add a digest to the label; it is a finding, and the only place it can
    // ever be noticed is here, comparing the record that is already on disk
    // against the bytes about to replace it.
    //
    // It is a report and not a refusal. The build is legitimate, the operator
    // asked for it, and the usual cause is benign (a dependency rebuilt, a
    // different toolchain). What is not acceptable is finding out silently.
    if let Ok(prev) = std::fs::read_to_string(dir.join("build.json")) {
        if let Ok(prev) = serde_json::from_str::<BuildRecord>(&prev) {
            if !env_copy && !prev.content.is_empty() && prev.content != content && !dirty {
                eprintln!(
                    "cauldron: NOT REPRODUCIBLE — `{}` version {label} was already installed \
                     with content fingerprint {} and this clean build of the same commit at \
                     the same profile produced {}. The label still names the commit it came \
                     from, so it is not wrong; it has simply stopped being enough to identify \
                     the bytes. The new build is being installed over the old one.",
                    d.name, prev.content, content
                );
            }
        }
    }
    guard::refuse_protected(&dir)?;
    std::fs::create_dir_all(&dir)?;
    let installed = dir.join(&b.bin);
    // ══ COPY TO A TEMPORARY NAME AND RENAME. NOT `copy` ONTO THE PATH. ════
    //
    // MEASURED, and it cost a confusing hour: `std::fs::copy` onto
    // `installed` writes into the EXISTING INODE, and that inode may be a
    // RUNNING IMAGE. On macOS, writing into the text of a running Mach-O gets
    // the running process SIGKILLed by the kernel.
    //
    // It is reachable by an ordinary sequence, not a contrived one, because
    // builds are SHARED across environments (`data/shared/builds/`) while the
    // label is just the commit and the profile. So:
    //
    //   cauldron build echo           -> installs label L
    //   CAULDRON_ENV=a cauldron start echo   (now executing  L/seam-probe)
    //   CAULDRON_ENV=b cauldron build echo   -> SAME commit, SAME profile,
    //                                           SAME label L, copy ONTO it
    //
    // and environment `a`'s service dies of SIGKILL, with no seam, no log
    // line and nothing naming the cause. That is exactly how it was found:
    // `prove-supervisor`'s second environment reported "`echo` exited with
    // signal: 9 (SIGKILL) before writing its seam" with an empty log, while
    // the first environment's copy was running the file being overwritten.
    //
    // A rename replaces the DIRECTORY ENTRY and leaves the old inode alone, so
    // the running process keeps executing the bytes it started with and the
    // next exec gets the new ones. It is also atomic, so a reader of the
    // version directory never sees a half-written image — and the reader here
    // is the supervisor about to exec it.
    //
    // This is the same defect class as the D-2 hazard that made
    // `target/release/threads` dangerous, one layer in: writing to a path
    // something is currently executing.
    let tmp = dir.join(format!(".{}.installing.{}", b.bin, std::process::id()));
    std::fs::copy(&artifact, &tmp)
        .with_context(|| format!("installing {} -> {}", artifact.display(), tmp.display()))?;
    std::fs::rename(&tmp, &installed)
        .with_context(|| format!("renaming into {}", installed.display()))?;

    // THE DECLARATION TRAVELS WITH THE BUILD. Written before the record, so a
    // record that names a hash always has the file it names.
    let declaration_sha256 = decl::write_snapshot(&dir, &d.text)?;
    let rec = BuildRecord {
        service: d.name.clone(),
        label: label.clone(),
        bin: b.bin.clone(),
        package: b.package.clone().unwrap_or_default(),
        profile: b.profile.clone(),
        workspace: ws.display().to_string(),
        built_at: now(),
        commit: commit.clone(),
        dirty,
        content,
        // The old field, kept populated so anything already reading it keeps
        // working. `commit` is the one to read.
        source_commit: Some(commit),
        declaration_sha256,
        declaration_source: d.loaded_from.display().to_string(),
    };
    let mut text = serde_json::to_string_pretty(&rec)?;
    text.push('\n');
    std::fs::write(dir.join("build.json"), text)?;
    Ok(rec)
}

/// **`cauldron build --snapshot-declaration <service> <label> [--force]` —
/// the BACKFILL.** Copies the declaration `decl::load` resolves NOW (root
/// override first, then shipped) into an existing build directory as its
/// `declaration.toml`, and records the hash in `build.json`. Builds nothing.
///
/// It exists for build directories that predate snapshots, and it is a verb
/// the operator runs by name rather than something a launch does for him:
/// a backfill is a statement that "the file in force today is the contract
/// this build was made under", and for a build made before a declaration
/// changed that statement is FALSE — which is exactly the case the snapshot
/// mechanism exists for. So a launch never backfills, `status` says
/// `shipped (no snapshot)` until somebody does, and an existing snapshot is
/// refused without `--force`.
fn backfill_snapshot(env: &Environment, o: &Opts) -> Result<i32> {
    let (service, label) = match o.services.as_slice() {
        [s, l] => (s.clone(), l.clone()),
        other => bail!(
            "`cauldron build --snapshot-declaration` takes exactly a service and a build label \
             (got {}). `cauldron versions <service>` lists the labels.",
            other.len()
        ),
    };
    let d = decl::load(&env.service_dir(), &service)?;
    let dir = env.build_dir(&service, &label);
    let rec_path = dir.join("build.json");
    let rec_text = std::fs::read_to_string(&rec_path).with_context(|| {
        format!(
            "version `{label}` of `{service}` is not built in this root ({} has no build.json). \
             A snapshot goes INTO an existing build directory; there is nothing to backfill.",
            dir.display()
        )
    })?;
    let mut rec: BuildRecord =
        serde_json::from_str(&rec_text).with_context(|| format!("parsing {}", rec_path.display()))?;
    let existing = dir.join(decl::DECLARATION_SNAPSHOT);
    if existing.exists() && !o.force {
        bail!(
            "REFUSED: {} already carries a declaration snapshot (sha256 {}). That snapshot is \
             the contract version `{label}` launches under; overwriting it with today's file \
             is a cut-over act, not a backfill. Pass `--force` to replace it deliberately.",
            dir.display(),
            if rec.declaration_sha256.is_empty() { "unrecorded".to_string() } else { rec.declaration_sha256.clone() }
        );
    }
    let sha = decl::write_snapshot(&dir, &d.text)?;
    rec.declaration_sha256 = sha.clone();
    rec.declaration_source = d.loaded_from.display().to_string();
    let mut text = serde_json::to_string_pretty(&rec)?;
    text.push('\n');
    std::fs::write(&rec_path, text).with_context(|| format!("writing {}", rec_path.display()))?;
    println!(
        "cauldron: snapshotted {} into {} (sha256 {sha}){}",
        d.loaded_from.display(),
        existing.display(),
        if o.force { " -- REPLACING the snapshot that was there" } else { "" }
    );
    println!(
        "cauldron:   version `{label}` of `{service}` now launches under THIS copy, whatever the \
         shipped file says later. To reverse: delete {} (the next launch falls back to the \
         shipped declaration and `status` says so).",
        existing.display()
    );
    Ok(0)
}

/// **WHAT AN IMAGE SAYS ABOUT ITSELF.** Parsed from one line of
/// `<image> <stamp_args...>` on stdout.
///
/// The format is `threads --build-stamp`'s, which is the only producer of one
/// on this machine: `commit=<sha> provenance=<pin|git|unknown> dirty=<1|0>
/// epoch=<n>`. Read as whitespace-separated `key=value` pairs so a stamp that
/// grows a field does not stop parsing.
struct Stamp {
    commit: String,
    dirty: bool,
    /// Verbatim, for the line `build` prints and for a human reading the log.
    line: String,
}

/// Run the image and read its stamp. `None` when it cannot state one.
///
/// **It runs the image.** That is a real act and it is the point: the commit
/// is read off the bytes that will actually run, which is stronger evidence
/// than `git rev-parse` in a workspace — that names a tree a build came from,
/// not the bytes. The risk of executing an image in order to ask it a question
/// is bounded by the image being the one about to be installed and launched
/// anyway: a build that cannot survive being asked cannot survive being
/// started.
fn read_stamp(image: &Path, args: &[String]) -> Option<Stamp> {
    let out = Command::new(image)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_stamp(String::from_utf8_lossy(&out.stdout).lines().next()?)
}

/// The parsing half of [`read_stamp`], separate so the refusal of
/// `dirty=unknown` can be tested on cauldron's OWN stamp: a cauldron that
/// could not establish its own dirt is refused as a stamped version of itself
/// exactly as any other service would be.
fn parse_stamp(line: &str) -> Option<Stamp> {
    let line = line.trim().to_string();
    let mut commit = String::new();
    let mut dirty: Option<bool> = None;
    for kv in line.split_whitespace() {
        let Some((k, v)) = kv.split_once('=') else { continue };
        match k {
            // `unknown` is the stamp's own word for "I was not told", and it
            // is NOT a commit. Reading it as one would put the literal string
            // `unknown` in the registry's COMMIT column.
            "commit" => {
                if v != "unknown" && !v.trim().is_empty() {
                    commit = v.to_string();
                }
            }
            // `dirty=unknown` leaves this `None`, which is REFUSED below
            // rather than read as clean. A supervisor that cannot establish
            // the tree state and assumes the flattering answer is how a build
            // gets labelled with a commit it was not built from.
            "dirty" => {
                dirty = match v {
                    "1" | "true" => Some(true),
                    "0" | "false" => Some(false),
                    _ => None,
                }
            }
            _ => {}
        }
    }
    if commit.is_empty() {
        return None;
    }
    Some(Stamp { commit, dirty: dirty?, line })
}

/// **INSTALL AN IMAGE PRODUCED ELSEWHERE AS A VERSION OF THIS SERVICE.**
///
/// No compiler runs. The file is copied into
/// `data/shared/builds/<service>/<label>/` exactly as a cargo build's artifact
/// would be, and from that point nothing downstream can tell the difference:
/// `pin`, `rollback`, `start`, `stop` and the daemon's restart path all see an
/// ordinary version directory with an ordinary `build.json`.
///
/// # Why this path exists at all
///
/// Because the operator's ruling is that the commit is the version identity
/// **read off the artifact**, and because the machine will not permit the
/// alternative. `threads`' own `build.rs` calls `std::process::exit(1)` rather
/// than build over a signed deploy image unless `THREADS_PROMOTE=1`; its
/// launch path is `chflags uchg` immutable; and a release build of it in a
/// fresh target directory is 3.4 GB on a disk that has been in single-digit
/// GiB all day. A design in which cauldron can only host what cauldron
/// compiles would have had cauldron host nothing.
///
/// It also keeps the signature in its ruled place — *identity only, not a
/// gate*. Cauldron never reads the signature. It reads the commit, which is
/// the thing anybody with the repository can check.
///
/// # How the version is named, in two cases, and why the second is honest
///
/// 1. **The image states its commit** (`stamp_args` is set and the image
///    answers). The label is the commit and the profile, exactly as a cargo
///    build's would be, and the registry's COMMIT column is a fact anybody can
///    check. `threads --build-stamp` is this case.
/// 2. **The image cannot state one.** `tts` has no `build.rs` and no
///    `--build-stamp`, so there is nothing compiled into it to read. The label
///    is then the CONTENT FINGERPRINT with an `unstamped-` prefix, and the
///    recorded commit is EMPTY.
///
/// Both halves of case 2 are deliberate. The prefix exists so that no reader
/// and no `--json` consumer can mistake sixteen hex characters for a commit;
/// the empty commit exists because `seams::probe` never compares an unstated
/// commit in either direction, so silence is correctly not read as
/// disagreement. The alternative was to sniff the commit from the declared
/// workspace's git log, and that is exactly how `services/threads.toml` came
/// to carry `version = "290113e"` — a real, plausible, checkable, WRONG commit
/// — for a day, because nothing compares a `version`.
fn install_artifact(
    env: &Environment,
    d: &Declaration,
    b: &decl::Build,
    artifact: &Path,
    quiet: bool,
    env_copy: bool,
) -> Result<BuildRecord> {
    let artifact = guard::refuse_as_image(artifact)
        .with_context(|| format!("service `{}` declares build.artifact", d.name))?;
    if !artifact.exists() {
        bail!(
            "service `{}`: its `[build] artifact` names {}, which does not exist. Cauldron runs \
             no compiler for an artifact — whatever ships the image produces it and cauldron \
             installs it — so there is nothing to build and nothing to wait for.",
            d.name,
            artifact.display()
        );
    }
    // R5.S3: a DIRECTORY artifact is a bundle; its identity is the bundle's
    // and its bytes are the executable `bin` names inside it.
    let is_bundle = artifact.is_dir();
    let exe = if is_bundle {
        artifact.join(env::hosting::bin_inside_bundle(&b.bin))
    } else {
        artifact.clone()
    };
    let content = env::fingerprint::fingerprint_file(&exe)?;
    let stamp = if b.stamps_itself() { read_stamp(&exe, &b.stamp_args) } else { None };

    // REFUSED rather than quietly downgraded: a declaration that says this
    // image stamps itself, over an image that will not answer, is a
    // DISAGREEMENT between the file and the bytes. Falling back to
    // `unstamped-` would silently demote the version identity of the one
    // service that has a real one, which is the failure this whole mechanism
    // exists to prevent.
    if b.stamps_itself() && stamp.is_none() {
        bail!(
            "REFUSED to install a version of `{}`: its declaration says the image states its \
             own commit (`stamp_args = {:?}`), and running {} with those args did not produce a \
             parseable `commit=<sha> ... dirty=<0|1>` line. The honest responses are to fix the \
             declaration or fix the image — not to fall back to a content fingerprint, which \
             would quietly demote the version identity of a service that has a real one. Remove \
             `stamp_args` if this image genuinely cannot state a commit.",
            d.name,
            b.stamp_args,
            artifact.display()
        );
    }

    let (label, commit, dirty) = match &stamp {
        Some(st) => (
            label_for(&st.commit, &b.profile, st.dirty, &content),
            st.commit.clone(),
            st.dirty,
        ),
        None => (
            format!("unstamped-{}", content.chars().take(16).collect::<String>()),
            String::new(),
            false,
        ),
    };

    let (label, dir) = claim_build_dir(env, &d.name, &label, env_copy)?;
    guard::refuse_protected(&dir)?;
    std::fs::create_dir_all(&dir)?;
    if is_bundle {
        bundle::install(&artifact, &dir, &b.bin)?;
    }
    let installed = dir.join(&b.bin);
    // Copied to a temporary name and RENAMED, so a reader of the version
    // directory never sees a half-written image. Sixteen megabytes is not an
    // atomic write, and the thing that would read a torn one is the supervisor
    // about to exec it.
    let tmp = dir.join(format!(".{}.installing.{}", b.bin, std::process::id()));
    if !is_bundle {
    // ══ THE BYTES ARE STREAMED, NOT `fs::copy`'d. MEASURED REASON. ════════
    //
    // `std::fs::copy` on macOS copies METADATA with the content, and the real
    // image this path exists for is `chflags uchg` IMMUTABLE — threads' own
    // `build.rs:253` sets that deliberately, so an agent's `cargo build`
    // cannot clobber a signed deploy image. The copy therefore arrived
    // immutable too, and the very next line — a chmod to 0755 — failed with
    // `Operation not permitted (os error 1)`.
    //
    // That is not a hypothetical: it is how `prove-wave1` aborted the first
    // time a bare `cauldron build` reached `threads`, with EPERM and nothing
    // naming the file. An installed version that inherited `uchg` would also
    // be un-replaceable by the next promotion, which is the same bug one step
    // later and much harder to find.
    //
    // `io::copy` between two handles writes an ORDINARY NEW FILE: the mode is
    // this process's umask and the flags are clear, which is what an installed
    // version should be. The immutability belongs to the shipper's launch path
    // and has no business travelling into cauldron's version store.
    {
        let mut src = std::fs::File::open(&artifact)
            .with_context(|| format!("reading the image {}", artifact.display()))?;
        let mut dst = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        std::io::copy(&mut src, &mut dst)
            .with_context(|| format!("copying {} -> {}", artifact.display(), tmp.display()))?;
        dst.sync_all()?;
    }
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&tmp)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&tmp, perm)
            .with_context(|| format!("making {} executable", tmp.display()))?;
    }
    std::fs::rename(&tmp, &installed)
        .with_context(|| format!("renaming into {}", installed.display()))?;
    }

    // THE DECLARATION TRAVELS WITH THE BUILD, exactly as for a cargo source:
    // an artifact build is the case that was measured to die under a changed
    // declaration (tts, hosted, `TTS_HOME` + `[launch] addr`).
    let declaration_sha256 = decl::write_snapshot(&dir, &d.text)?;
    let rec = BuildRecord {
        service: d.name.clone(),
        label: label.clone(),
        bin: b.bin.clone(),
        // No cargo package: there was no cargo invocation. Empty is the honest
        // record of that, and `workspace` below carries the one path that does
        // identify where these bytes came from.
        package: String::new(),
        profile: b.profile.clone(),
        workspace: artifact.display().to_string(),
        built_at: now(),
        commit: commit.clone(),
        dirty,
        content,
        source_commit: if commit.is_empty() { None } else { Some(commit) },
        declaration_sha256,
        declaration_source: d.loaded_from.display().to_string(),
    };
    let mut text = serde_json::to_string_pretty(&rec)?;
    text.push('\n');
    std::fs::write(dir.join("build.json"), text)?;
    if !quiet {
        match &stamp {
            Some(st) => println!(
                "cauldron: installed `{}` as version {label} from the image at {} -- the image \
                 itself states `{}`",
                d.name,
                artifact.display(),
                st.line
            ),
            None => println!(
                "cauldron: installed `{}` as version {label} from the image at {} -- it states \
                 NO commit (its declaration sets no `stamp_args`), so this version is named \
                 after its content fingerprint and the registry records an EMPTY commit rather \
                 than one guessed from a repository log",
                d.name,
                artifact.display()
            ),
        }
    }
    Ok(rec)
}

/// Read-only: `git rev-parse HEAD` in the source workspace, recorded for
fn git_head(ws: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(ws)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}


// ══════════════════════════════════════════════════════════════════════════
//  THE VERSION PROTOCOLS: build, test, pin, rollback
// ══════════════════════════════════════════════════════════════════════════
//
// The operator's ruling: *"the supervisor should have protocols for testing,
// building, and rolling back to the previous good version. that means we should
// have a version that does not rely on attestation - likely the commit. the
// registry should be clear about that version."*
//
// Four verbs over ONE identity, and the identity is the commit:
//
//   build     a commit becomes an installed version, under a label derived
//             from it. Refuses a dirty tree, because then the commit does not
//             identify the bytes.
//   test      that version is started IN ANOTHER ENVIRONMENT, probed, asked
//             for its warnings, and stopped. Passing is what makes it GOOD.
//   pin       an environment is told to run a named version.
//   rollback  an environment is re-pinned to the PREVIOUS version that was
//             good there.
//
// There is no `promote` and no `mark-good` verb, deliberately. Good is not a
// thing anybody declares: it is recorded by `start` when a service completed
// both of its handshakes, which is the only evidence that is not somebody's
// word. See `verb_start`.

/// **`cauldron test <service>` — prove a version in an environment that is not
/// this one.**
///
/// # The incident this verb is shaped by
///
/// A test set `AUI_THREADS_PORT` (the health port, 3651) and never
/// `AUI_THREADS_CHASSIS_PORT` (3653). Its gate therefore probed a scratch
/// daemon while every command and every turn travelled to the LIVE one. It
/// created a real thread and spent real money.
///
/// That was possible because "a test daemon" and "the real daemon" were
/// distinguished by *port numbers an agent has to remember to override*. A
/// forgotten variable fell back to production, and a partial override was
/// indistinguishable from a complete one.
///
/// So this verb does not take a port, and there is no port to forget. It takes
/// an ENVIRONMENT, which is a data root plus a seam table — the whole triple
/// moves together or not at all. A service started here binds `127.0.0.1:0`
/// like every other cauldron service and publishes whatever the kernel gave it,
/// so two deployments cannot collide on a port even in principle, and a
/// consumer in this environment reads this environment's table because that is
/// the only table its data root contains.
///
/// # Why it refuses to run in the caller's environment
///
/// The single most important line in this function is the refusal below. A test
/// is a thing you run *somewhere else*; a test that runs where you are standing
/// is the above incident with a different variable name. Making the test
/// environment default to a name the operator did not type, and refusing when
/// it collides with the one he is in, means the isolation is the mechanism's
/// rather than the caller's to remember.
fn verb_test(args: &[String]) -> Result<i32> {
    let o = parse(args)?;
    let here = Environment::resolve()?;
    let test_name = o.test_env.clone().unwrap_or_else(|| DEFAULT_TEST_ENV.to_string());

    // THE REFUSAL. See this function's doc.
    if test_name == here.name {
        bail!(
            "REFUSED: `cauldron test` will not run in `{}`, because that is the environment you \
             are standing in. A test that runs where you are standing is not a test of a \
             version, it is a deployment of one — and it would put a second process on this \
             environment's data root, which for `prod` means the live store.\\n\\
             This is the shape of the mistake that already cost money here: a test that \
             overrode one port variable and not the other probed a scratch daemon while every \
             turn went to the live daemon. There is no port to forget now — the unit of \
             isolation is the ENVIRONMENT, which is a data root and a seam table that move \
             together. Name a different one with `--test-env <name>`, or run this from another \
             environment.",
            here.name
        );
    }
    let test = here.for_name(&test_name)?;

    let decls = owned_targets(&here, &o.services, "test")?;
    println!("{}", here.banner());
    println!("testing in    {} (data {})", test.name, test.data_root.display());
    test.ensure_dirs()?;

    let mut failed = false;
    for d in &decls {
        match test_one(&here, &test, d, &o) {
            Ok(label) => println!(
                "cauldron: PASS {} version {} in environment `{}`\\n\
                 cauldron:   it registered its own seam carrying this start's boot token, and \
                 answered for its warnings shape on its own socket, against `{}`'s data root — \
                 not this one's.\\n\
                 cauldron:   deploy it here with `cauldron pin {} {}` then `cauldron start {}`.",
                d.name, label, test.name, test.name, d.name, label, d.name
            ),
            Err(e) => {
                eprintln!("cauldron: FAIL {}: {e:#}", d.name);
                failed = true;
            }
        }
    }
    // Exit 6: the test ran and a version did not pass. Its own code, distinct
    // from 1 (something went wrong) — a failing test is this verb working.
    Ok(if failed { 6 } else { 0 })
}

/// Build, pin into the test environment, start, stop. Returns the label.
fn test_one(here: &Environment, test: &Environment, d: &Declaration, o: &Opts) -> Result<String> {
    // ── A PENDING HANDOVER IS NOT TESTED EITHER, and it is refused BEFORE the
    // build, so a refused test leaves nothing behind. A test launches the
    // service — in another environment's data root, but on whatever the
    // declaration fixes that environments do not separate, such as a hosted
    // service's `[launch] addr` — while the outside launcher still runs the
    // real one. This path reaches `start_one` directly and not through
    // `launch_for_daemon`, so the check there never covered it (R5.S1
    // inspection, finding F-2; latent then only because every `[migration]`
    // declaration installs an unstamped artifact, which `start_one` refuses
    // for a different reason).
    if let Some(mg) = &d.migration {
        bail!("{}", mg.refusal(&d.name));
    }
    let rec = build_one(here, d, true, o.allow_dirty, false)?;
    println!(
        "cauldron: built {} as {} (commit {})",
        d.name,
        rec.label,
        rec.commit_short()
    );

    // Pinned in the TEST environment's manifest. The caller's `env.toml` is not
    // read and not written: a test must not be able to change what the
    // environment it was launched from runs, however it ends.
    ensure_manifest(test, std::slice::from_ref(d))?;
    let m = Manifest::update(&test.manifest(), |m| {
        m.set_version(&d.name, &rec.label);
        Ok(m.clone())
    })?;

    // Through the same resolver every launch uses, so the test runs the build
    // under the declaration the build carries — which `build_one` just wrote.
    let l = resolve_launch(test, &m, &d.name)?;
    let seam = start_one(test, &l)?;
    println!(
        "cauldron: `{}` came up in `{}` at {} (pid {}, commit {})",
        d.name,
        test.name,
        seam.addr,
        seam.pid,
        seam.commit.chars().take(12).collect::<String>()
    );

    // GOOD IS RECORDED IN THE ENVIRONMENT THAT PROVED IT, and only there. A
    // pass in `test` does not make a version good in `prod`: what differs
    // between the two is the data root, which is the thing a bad build breaks.
    //
    // And only under the stable-period rule (R9.C4, gap C-4): `test` stops the
    // service the moment it has registered, so it has proven that the build
    // LAUNCHES, not that it stays up. With the default `stable_after_secs` it
    // therefore records nothing; a test environment that declares
    // `stable_after_secs = 0` asks for registration to count, and gets it.
    if stayed_up(secs_since_epoch().saturating_sub(seam.started_at), m.supervisor.stable_after_secs) {
        Manifest::update(&test.manifest(), |m| {
            m.record_good(&d.name, &rec.label);
            Ok(())
        })?;
    }

    // Stopped on the way out, pass or fail, so a test does not leave a process
    // behind holding a port and a data root. The seam is signalable by
    // construction here — `start_one` returned it, so the service registered it
    // from inside itself and its pid provenance is `self` — but the check is
    // made rather than assumed.
    // Stopped on the way out, pass or fail, so a test does not leave a
    // process behind holding a port and a data root. `TERM` and escalation
    // allowed: a scratch environment's service is exactly the case the
    // hard-kill ruling covers.
    stop_by_seam(test, &seam, "TERM", true, supervisions::Why::REQUESTED)?;
    Ok(rec.label)
}

/// **STOP THE PROCESS A ROW DESCRIBES, after proving the pid is ours to
/// signal.** The one stop path, so `stop`, `test`'s teardown, `restart` and
/// the daemon cannot disagree about what stopping means.
///
/// # The signal is the SERVICE's, not this function's
///
/// `sig` comes from the declaration's `stop_signal`, and the two real services
/// disagree in a way that is measured rather than assumed. `threads` installs a
/// `SignalKind::terminate()` handler and, on SIGTERM, stops accepting on both
/// ports, drains for 45 s and runs a WAL checkpoint. `tts` installs **only**
/// `tokio::signal::ctrl_c()`, which on Unix is SIGINT alone — so a SIGTERM to
/// `tts` falls straight through to the default disposition: immediate death,
/// no drain, no checkpoint. Sending the wrong signal to the voice daemon is
/// not a stylistic matter, and this is why the key exists.
///
/// # And the escalation is the service's too
///
/// `escalate` carries the 2026-09-02 ruling — *"if it tries to softly kill it
/// and it won't go down, then it has to hard kill it. I'm okay with that …
/// You just have to like try to exercise it restarting itself first."* — and
/// its boundary. That ruling named the threads module. It did not extend to
/// the service that speaks to him mid-sentence, and
/// `~/.threads-supervisor/services/tts.conf` recorded the same decision as
/// `ESCALATION=0`. A service with `escalate = false` that ignores its stop
/// signal is REPORTED and left alone; it is never SIGKILLed.
/// **One of the two places cauldron ever takes a service down**, and so one of
/// the two places a supervision is recorded. `why` is one of
/// [`supervisions::Why`]'s words and it is a parameter rather than something
/// inferred here, because this function cannot know whether it was called by
/// an unpin sweep, an operator's ask or a re-pin — and the whole value of the
/// ledger is that word.
///
/// The record is written AFTER the stop succeeded. A line claiming an act that
/// did not happen is worse than a missing line: the first is believed.
pub(crate) fn stop_by_seam(
    env: &Environment,
    seam: &Seam,
    sig: &str,
    escalate: bool,
    why: &str,
) -> Result<()> {
    if let Some(refusal) = seam.refusal_to_signal() {
        bail!("cauldron will not stop `{}`: {refusal}", seam.service);
    }
    // ── C-1: GONE MEANS THE PID IS GONE, NOT THAT THE PORT WENT QUIET ──
    //
    // `threads` closes its port FIRST and then drains for up to 45 s. Waiting
    // on the port declared it stopped within ~50 ms of the signal, and every
    // caller then launched a replacement onto the same data directory while
    // the old one was still checkpointing it. So this waits on the process
    // (`Verdict::process_gone`), for the declaration's own grace.
    let grace = stop_grace(env, seam);
    let first = seams::probe(seam);
    let soft = if first.process_gone(seam) {
        // Already gone: nothing to signal, and a pid that is not ours any more
        // must not be signalled at all.
        "found it already gone".to_string()
    } else if first.owned_and_exiting(seam) {
        // Port quiet, pid alive: it is ALREADY stopping (a drain we did not
        // start, or an earlier stop). NOT signalled again — a second SIGTERM
        // makes `threads` abandon its drain. Waited for, under the same grace.
        eprintln!(
            "cauldron: `{}` (pid {}) has already closed {} and is still running — it is \
             exiting; waiting up to {}s for it, without signalling it again",
            seam.service,
            seam.pid,
            seam.addr,
            grace.as_secs()
        );
        "waited for it to finish exiting".to_string()
    } else {
        signal(seam.pid, sig);
        format!("signalled SIG{sig}")
    };
    if !wait_gone(seam, grace) {
        if !escalate {
            // REPORTED, NOT ESCALATED. The seam is left in place because it is
            // not a lie: the process really is still there.
            bail!(
                "`{}` was still running {}s after the clean stop ({soft}) and its declaration \
                 says `escalate = false`, so it was NOT hard-killed and is still running at {} \
                 (pid {}). That is the 2026-09-02 hard-kill ruling's boundary: it named the \
                 threads module and did not extend to the service that speaks to him \
                 mid-sentence. Nothing further was signalled and the seam was left in place, \
                 because the process really is there.",
                seam.service,
                grace.as_secs(),
                seam.addr,
                seam.pid
            );
        }
        eprintln!(
            "cauldron: `{}` was still running {}s after the clean stop ({soft}); sending \
             SIGKILL to pid {} (its declaration allows escalation)",
            seam.service,
            grace.as_secs(),
            seam.pid
        );
        signal(seam.pid, "KILL");
        if !wait_gone(seam, Duration::from_secs(5)) {
            // Never declared stopped while the pid lives: every caller would
            // launch a second instance on top of it.
            bail!(
                "`{}` (pid {}) is STILL RUNNING 5s after SIGKILL. It was NOT recorded as \
                 stopped and its row was left in place, so nothing will be launched on top of \
                 it.",
                seam.service,
                seam.pid
            );
        }
    }
    // R5.S8 (D-4): a child this process forked is REAPED here, at the one stop
    // path, rather than left `<defunct>` under the holder. No-op for a pid it
    // did not fork. See `daemon::reap_stopped`.
    daemon::reap_stopped(seam.pid);
    clear(env, &seam.service)?;
    // RECORDED HERE, at the chokepoint, and not at the six call sites. A
    // ledger with a missing site is a ledger a reader cannot trust the
    // ABSENCE of a line in, and the absence is the thing the guard asserts.
    let mut rec = supervisions::note(env, SELF_COMMIT, &seam.service, supervisions::Act::STOP, why);
    rec.version = seam.version.clone();
    rec.boot = seam.boot_token.clone();
    rec.pid = Some(seam.pid);
    rec.detail = format!("{soft} at {} and pid {} is gone", seam.addr, seam.pid);
    if let Err(e) = supervisions::append(&env.data_root, &rec) {
        eprintln!(
            "cauldron: {}",
            supervisions::write_failure(&seam.service, supervisions::Act::STOP, &env.data_root, &e)
        );
    }
    println!("cauldron: stopped `{}` in `{}` (was {})", seam.service, env.name, seam.addr);
    Ok(())
}

/// How long a clean stop is given before escalation is considered, for a
/// service whose declaration cannot be read (`stop_grace_secs` defaults to
/// the same 15 s; `threads` declares 75 — see [`stop_grace`]).
///
/// Fifteen rather than five, because `threads` drains for up to 45 s by design
/// and `tts` may be holding a half-synthesised utterance. The old supervisor
/// used 60 s for its long rung against a measured ~25 s health stall; this is
/// the budget before the SECOND decision, not before giving up, and a service
/// that needs longer declares `escalate = false` and is never killed at all.
pub(crate) const STOP_GRACE_SECS: u64 = 15;

// ═══════════════════════════════ WHICH DECLARATION A LAUNCH RUNS UNDER ═════
//
// The finding (R3.S2.0b, measured 2026-10-10): a declaration is per ROOT and
// a build is per VERSION, and nothing tied the two together. The daemon read
// `services/<svc>.toml` at every launch, restart and rollback, so the moment a
// migrated service's new declaration landed in the shipped directory the
// pinned OLD build relaunched under it and died — and `auto_rollback` then
// re-pinned the previous good label and launched THAT under the same new
// declaration, turning one dead daemon into two. The tts cut-over had to be
// hand-sequenced as a one-act window because of it.
//
// So: `cauldron build` snapshots the declaration into the build directory
// (`decl::write_snapshot`), and EVERY launch of a pinned version resolves its
// declaration through `resolve_launch` below, which reads the snapshot and
// never the shipped file when a snapshot exists. The shipped file is read
// here in exactly one case — a build directory with no snapshot, which is a
// build made before this existed — and that case is named on every surface
// that shows it (`DeclSource::Shipped`, `status`, `envs`, `pin`).

/// Where the declaration a launch runs under came from.
pub(crate) enum DeclSource {
    /// `builds/<svc>/<label>/declaration.toml`, hash-checked.
    Snapshot { label: String, sha256: String },
    /// The shipped (or root-override) file, because the build directory has
    /// no snapshot. `recorded_sha256` is non-empty when `build.json` names a
    /// snapshot that is no longer on disk.
    Shipped { label: String, path: PathBuf, recorded_sha256: String },
}

impl DeclSource {
    /// One clause for a log line or a `started` message.
    pub(crate) fn describe(&self) -> String {
        match self {
            DeclSource::Snapshot { label, sha256 } => {
                format!("snapshot in build {label} (sha256 {})", &sha256[..12])
            }
            DeclSource::Shipped { label, path, recorded_sha256 } if recorded_sha256.is_empty() => {
                format!(
                    "shipped (no snapshot) — build {label} predates declaration snapshots and \
                     runs under {}",
                    path.display()
                )
            }
            DeclSource::Shipped { label, path, recorded_sha256 } => format!(
                "shipped (no snapshot) — build {label}'s declaration.toml (sha256 {}) is GONE, \
                 so it runs under {}",
                &recorded_sha256[..12],
                path.display()
            ),
        }
    }
}

/// Everything a launch needs, resolved ONCE: the label the pin names, its
/// build record, and the declaration that build runs under.
pub(crate) struct Launchable {
    pub label: String,
    pub rec: BuildRecord,
    pub decl: Declaration,
    pub source: DeclSource,
}

/// **The declaration a given BUILD runs under.** The snapshot in its build
/// directory when there is one — and a snapshot that is present but cannot be
/// trusted (hash mismatch, unrecorded, unparseable) is a REFUSAL here, so no
/// launch path downstream can run a version under garbage. The shipped file
/// only when there is no snapshot at all, which is a build made before
/// R3.S2.0b, and the fallback is SAID in the returned source.
pub(crate) fn declaration_for_build(
    env: &Environment,
    service: &str,
    label: &str,
    rec: &BuildRecord,
) -> Result<(Declaration, DeclSource)> {
    let dir = env.build_dir(service, label);
    match decl::snapshot(&dir, rec, service)? {
        decl::Snapshot::Present { decl: d, sha256, .. } => {
            Ok((d, DeclSource::Snapshot { label: label.to_string(), sha256 }))
        }
        decl::Snapshot::Absent { recorded_sha256 } => {
            let d = decl::load(&env.service_dir(), service).with_context(|| {
                format!(
                    "version {label} of `{service}` carries no declaration snapshot (it was \
                     built before snapshots existed), so its declaration is the shipped file — \
                     and that could not be loaded"
                )
            })?;
            let path = d.loaded_from.clone();
            Ok((d, DeclSource::Shipped { label: label.to_string(), path, recorded_sha256 }))
        }
    }
}

/// **Resolve what this environment's pin says to launch for `service`.**
/// `latest` as a default lives HERE, at the one call site that means it: on
/// the launch path "this environment names no version" really is an
/// instruction to resolve the newest build (`cauldron start <service>` names a
/// service explicitly and pins what it starts).
pub(crate) fn resolve_launch(env: &Environment, m: &Manifest, service: &str) -> Result<Launchable> {
    let pin = m.pin(service).unwrap_or_else(|| "latest".into());
    let (label, rec) = decl::resolve_version(&env.builds_for(service), &pin, &env.name)
        .with_context(|| format!("resolving version `{pin}` for service `{service}`"))?;
    let (decl, source) = declaration_for_build(env, service, &label, &rec)?;
    Ok(Launchable { label, rec, decl, source })
}

/// **How to stop the instance a ROW describes**: `(stop_signal, escalate)`
/// from the declaration of the version that is RUNNING — its build
/// directory's snapshot — because the signal a daemon handles is a property
/// of the binary that is up, not of the file that will build the next one.
/// Falls back to the shipped file (no snapshot), then to `TERM`/escalate
/// (no usable declaration at all, which the unpinned sweep already tolerated).
pub(crate) fn stop_terms(env: &Environment, seam: &Seam) -> (String, bool) {
    let d = running_decl(env, seam).or_else(|| decl::load(&env.service_dir(), &seam.service).ok());
    match d {
        Some(d) => (d.stop_signal, d.escalate),
        None => ("TERM".to_string(), true),
    }
}

/// The declaration of the version a ROW is running (its build's snapshot).
fn running_decl(env: &Environment, seam: &Seam) -> Option<decl::Declaration> {
    decl::resolve_version(&env.builds_for(&seam.service), &seam.version, &env.name)
        .ok()
        .and_then(|(label, rec)| declaration_for_build(env, &seam.service, &label, &rec).ok())
        .map(|(d, _)| d)
}

/// **HOW LONG A CLEAN STOP OF THIS ROW IS GIVEN** before escalation (C-1):
/// `stop_grace_secs`, the LONGER of the running build's declaration and the
/// shipped one, else [`STOP_GRACE_SECS`].
///
/// The longer of the two, and not the running build's alone as with the
/// signal, because the key is new: every build snapshotted before it existed
/// reads the 15 s default, and the FIRST stop of the threads that is running
/// today would SIGKILL it mid-drain exactly as before. A longer grace costs a
/// wedged service some seconds; a shorter one costs a WAL checkpoint.
pub(crate) fn stop_grace(env: &Environment, seam: &Seam) -> Duration {
    let running = running_decl(env, seam).map(|d| d.stop_grace_secs);
    let shipped = decl::load(&env.service_dir(), &seam.service).ok().map(|d| d.stop_grace_secs);
    Duration::from_secs(running.max(shipped).unwrap_or(STOP_GRACE_SECS))
}

/// **LAUNCH ONE SERVICE FOR THE DAEMON.** Returns `(pid, label, addr)`.
///
/// Two paths, chosen by `registers`, and they differ in exactly one thing:
/// **who writes the row.**
///
/// * A REGISTERING service is handed `CAULDRON_BIND=127.0.0.1:0` and writes
///   its own row, carrying the boot token this call minted. That is
///   [`start_one`], unchanged, and it is also what makes the row's `live`
///   verdict available.
/// * A HOSTED service is forked, polled until something is listening on the
///   address its `[launch]` section declares, and then the supervisor writes
///   the row with [`seams::record_hosted`].
///
/// **Neither path records "good"** (R9.C3, R9.C4 — gaps C-3, C-4). This
/// returns at bind or registration, which proves the process launched and
/// not that the build stays up. Good is written once the row has been up
/// `stable_after_secs`, through [`record_good_if_stable`]: by the daemon for
/// every row it holds, hosted and registering alike.
pub(crate) fn launch_for_daemon(
    env: &Environment,
    l: &Launchable,
    why: &str,
) -> Result<(u32, String, String)> {
    let d = &l.decl;
    // ── A PENDING HANDOVER IS NOT LAUNCHED. `start`, the daemon's restart and
    // a requested restart all funnel through here. The other two places that
    // refuse are `pin` (before anything is written) and `test_one` (before
    // anything is built), which reaches `start_one` without coming through
    // here — so "the whole of" this guarantee is these three checks, not this
    // one alone — plus, for a scheduled job, `jobs::tend`'s own (R5.S3b), since
    // a job never comes through here.
    if let Some(mg) = &d.migration {
        bail!("{}", mg.refusal(&d.name));
    }
    // ── C-1: NEVER A SECOND INSTANCE WHILE THE OLD PID LIVES ─────────────
    //
    // Every launch path funnels through here, and the bind check in
    // `launch_hosted` only asks whether the PORT is held. `threads` closes its
    // port first and then drains for up to 45 s on the same data directory,
    // so a quiet port is no proof the old instance is gone. A row cauldron
    // owns whose process is still running refuses the launch; the stop paths
    // wait for the pid before they get here, so this fires only when
    // something skipped them.
    if let Ok(t) = Table::load(&env.name, &env.data_root, &env.seams()) {
        if let Some(old) = t.seams.get(&d.name) {
            // A row still answering as up (`live`, `live:unverified`) is
            // refused by the bind check (hosted) or by `start_one`, with its
            // own message. Every OTHER state of an owned row whose pid lives —
            // a quiet port mid-drain, `unhealthy`, `unknown:timeout`,
            // `stale:wrong-*` — is refused here (C-1b): `start_one` reads
            // those as a stale seam to clear and forks a second instance, and
            // a registering service has no bind check to stop it.
            let v = seams::probe(old);
            if v.owned_and_running(old)
                && !matches!(v.state, SeamState::Live | SeamState::LiveUnverified)
            {
                bail!(
                    "REFUSED to launch `{}`: the instance this environment's row records (pid \
                     {}, at {}) is STILL RUNNING. A quiet port is not a stopped service — \
                     `threads` closes its port and then drains for up to 45 s on the same data \
                     directory — so a second instance is not started until that pid is gone. \
                     Stop it (`cauldron stop {}`), which waits for the pid.",
                    d.name,
                    old.pid,
                    old.addr,
                    d.name
                );
            }
        }
    }
    if d.kind == "job" {
        bail!(
            "`{}` is a scheduled job: it is run by the daemon holding this environment on its \
             `[schedule]`, one pass at a time, and has no process to start. Pin it and run \
             `cauldron daemon` (or let the holder pick the pin up).",
            d.name
        );
    }
    // A launch that FAILS names the declaration it was under: that is the
    // first question after a dead daemon, and it is the one the pre-step
    // failure could not answer.
    let seam = (if d.registers { start_one(env, l) } else { launch_hosted(env, l) })
        .with_context(|| format!("under declaration: {}", l.source.describe()))?;
    // THE OTHER CHOKEPOINT. Both launch paths funnel through here, so a launch
    // that is not recorded is not spellable — which matters more than it
    // sounds: a crash-restart's evidence is a LAUNCH record with no preceding
    // stop, and a site that forgot to record would make that look like
    // nothing having happened.
    //
    // `was_version` IS DELIBERATELY LEFT EMPTY HERE. The only version this
    // function knows is the one it is launching ONTO, which is already
    // `version`; reading the pin again would just restate it and would read as
    // a from-version that was never running. A restart's from-version is the
    // STOP record's `version`, written by the half that had the old row in
    // hand — so from/to is read across the pair rather than guessed by either
    // half. `Act::REPIN` is the one record that carries both, because the
    // rollback path genuinely knows both.
    //
    // The BOOT TOKEN is taken off the row the launch just wrote, which is
    // where the instance identity is. It is empty for a hosted binary that
    // cannot echo one, and that emptiness is recorded rather than hidden: it
    // is the fact that for this service no identity stronger than a pid is
    // available, and a reader needs to know which kind of record it is
    // holding.
    let mut rec = supervisions::note(env, SELF_COMMIT, &d.name, supervisions::Act::LAUNCH, why);
    rec.version = seam.version.clone();
    rec.boot = seam.boot_token.clone();
    rec.pid = Some(seam.pid);
    rec.detail = format!(
        "forked and seen listening at {} (pid source `{}`, boot token {}); declaration: {}",
        seam.addr,
        seam.pid_source,
        if seam.has_boot_token() { "present" } else { "EMPTY — this binary cannot echo one" },
        l.source.describe()
    );
    if let Err(e) = supervisions::append(&env.data_root, &rec) {
        eprintln!(
            "cauldron: {}",
            supervisions::write_failure(&d.name, supervisions::Act::LAUNCH, &env.data_root, &e)
        );
    }
    Ok((seam.pid, seam.version, seam.addr))
}

/// Fork a binary that cannot register, wait for its listener, write its row.
fn launch_hosted(env: &Environment, l: &Launchable) -> Result<Seam> {
    // THE DECLARATION IS THE BUILD'S, resolved by `resolve_launch` from the
    // build directory's snapshot. The `[launch] addr`, the `[env]`, the `bin`
    // and the probe below are all read off it — which is the whole fix: a
    // hosted build made under `TTS_HOME` + `[launch] addr` keeps them when
    // the shipped file stops saying them.
    let d = &l.decl;
    let label = l.label.clone();
    let rec = &l.rec;
    let declared = d.launch.as_ref().with_context(|| {
        format!("`{}` declares `registers = false` but has no `[launch]` section", d.name)
    })?;
    // THE ENVIRONMENT'S ADDRESS, if it names one. A declaration is shared by
    // every environment in a root, so its `[launch] addr` is right in exactly
    // one of them; `[services.<name>] addr` in this environment's `env.toml`
    // is how a second environment hosts the same foreign binary without
    // colliding with the first. See `decl::Pin::addr`.
    let effective = Manifest::load(&env.manifest())?
        .hosted_addr(&d.name)
        .unwrap_or_else(|| declared.addr.clone());
    // THE STATIC HALF of the collision guard (R6.S3), before the live half
    // below: an address another environment in this root CLAIMS is refused
    // even while that environment is down — the moment it came up, one of
    // the two would fail to bind, or worse, be probed as the other's.
    envs::refuse_addr_collision(env, &d.name, &effective)?;
    let launch = &env::decl::Launch { addr: effective, ..declared.clone() };
    let binary = env.build_dir(&d.name, &label).join(&d.build()?.bin);
    if !binary.exists() {
        bail!("version `{label}` of `{}` has no binary at {}", d.name, binary.display());
    }
    // ── IS SOMETHING ALREADY ON THAT ADDRESS? ───────────────────────────
    //
    // Checked BEFORE the fork, and it is the most important line in this
    // function. A hosted service's port is declared rather than
    // kernel-assigned, so unlike a registering service it CAN collide — and
    // the thing it would collide with is another instance of itself on the
    // same data directory. On a long-lived machine that directory can hold a
    // multi-gigabyte sqlite store with thousands of the operator's threads in it.
    //
    // The bind is also the only mutual exclusion these daemons have: their own
    // source says so in as many words ("no pidfile, no advisory lock on
    // threads.db, no daemons table"). So a listener here means do not fork,
    // whatever the registry says.
    // REL-6a5: a REFUSED probe is not "nothing is listening" — an isolated
    // process never forks a service onto an address it may not even dial.
    let probe = env::egress::connect_tcp(
        "hosted listener",
        &launch.addr.parse().with_context(|| format!("`{}` is not a socket address", launch.addr))?,
        Duration::from_millis(500),
    );
    if let Err(e) = &probe {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            bail!("{e}");
        }
    }
    if probe.is_ok()
    {
        bail!(
            "REFUSED to launch `{}`: something is ALREADY listening on {}, which is the address \
             its `[launch]` section declares. A hosted service's port is declared rather than \
             kernel-assigned, so it can collide — and the thing it collides with is another \
             instance of itself on the same data directory. For `threads` that directory holds \
             the operator's live store. The bind is the only mutual exclusion these daemons \
             have, so a listener here means DO NOT FORK, whatever this environment's registry \
             says. Find out what is there (`lsof -nP -iTCP:{} -sTCP:LISTEN`) before anything \
             else.",
            d.name,
            launch.addr,
            launch.addr.rsplit(':').next().unwrap_or("")
        );
    }

    let data_dir = env.service_data(&d.name)?;
    std::fs::create_dir_all(&data_dir)?;
    let log = env.log(&d.name)?;
    std::fs::create_dir_all(log.parent().unwrap())?;
    // DG-2: rotated by rename past 64 MiB, 3 generations kept, before it is opened (logrotate.rs).
    let logfile = logrotate::open_log(&log)?;
    let errfile = logfile.try_clone()?;

    // R5.S3: `[launch] cwd` when the declaration names one (ui-server runs
    // in the harness checkout, as its launchd plist did); the data directory
    // otherwise, as before.
    let cwd = match &launch.cwd {
        Some(c) => PathBuf::from(expand_hosted(&expand(c, &data_dir), &launch.addr)),
        None => data_dir.clone(),
    };
    let mut cmd = Command::new(&binary);
    cmd.current_dir(&cwd)
        // The CAULDRON_* variables are set even though this binary does not
        // read most of them. Two reasons: a binary that LEARNS to register
        // needs nothing added here, and `threads` already reads CAULDRON_HOME
        // and CAULDRON_ENV for its consumer ledger — so omitting them would
        // point its ledger at a different environment than the one hosting it.
        .env("CAULDRON_SERVICE", &d.name)
        .env("CAULDRON_ENV", &env.name)
        .env("CAULDRON_HOME", &env.root)
        .env("CAULDRON_DATA_ROOT", &env.data_root)
        .env("CAULDRON_SERVICE_DATA_DIR", &data_dir)
        .env("CAULDRON_SEAMS", env.seams())
        .env("CAULDRON_VERSION", &label)
        .env("CAULDRON_COMMIT", &rec.commit)
        .env("CAULDRON_DIRTY", if rec.dirty { "1" } else { "0" })
        .env("CAULDRON_BINARY", &binary)
        .env("CAULDRON_PROBE", &d.probe)
        .env("CAULDRON_HOSTED_ADDR", &launch.addr)
        .env("CAULDRON_HOSTED_PORT", hosted_port(&launch.addr))
        .env_remove("ENV")
        .stdin(Stdio::null())
        .stdout(Stdio::from(logfile))
        .stderr(Stdio::from(errfile));
    // The declaration's own environment, with `${CAULDRON_SERVICE_DATA_DIR}`
    // expanded — which is how a declaration points a foreign daemon at this
    // environment's data root without hardcoding a path that would be wrong in
    // every other environment. `${CAULDRON_HOSTED_ADDR}` / `_PORT` do the same
    // for the address, so a per-environment `addr` reaches the child.
    for (k, v) in &d.env {
        cmd.env(k, expand_hosted(&expand(v, &data_dir), &launch.addr));
    }
    use std::os::unix::process::CommandExt;
    // ITS OWN PROCESS GROUP, and this is what makes everything else possible.
    // The child is therefore NOT in launchd's job, so launchd restarting the
    // holder never touches a running service — which is why re-attach works
    // and why the daemon may exit to pick up new bytes.
    cmd.process_group(0);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {} for service `{}`", binary.display(), d.name))?;
    let pid = child.id();

    // THE HANDLE IS KEPT, and `wait_for_listener` needs it: the check is not
    // "is something listening" but "is MY CHILD listening", and the cheapest
    // half of that is asking the handle whether the child is still alive.
    //
    // It is also what lets the daemon REAP. A dropped `Child` is never waited
    // on, so an exited child becomes a zombie until its parent dies — free for
    // a one-shot CLI, a leak for a daemon that restarts services for weeks.
    // Observed as `1745 1639 <defunct>` minutes after the first cutover.
    if let Err(e) = daemon::wait_for_listener(&launch.addr, &mut child, daemon::HOSTED_BIND_WAIT) {
        // Kill what we forked. A child that never bound is not something to
        // leave behind holding a data directory, and its pid is ours by
        // construction here.
        signal(pid, &d.stop_signal);
        std::thread::sleep(Duration::from_millis(500));
        signal(pid, "KILL");
        // Waited on, so the failure does not leave a zombie behind. This is
        // the one place the handle is consumed rather than handed on.
        let _ = child.wait();
        let tail = std::fs::read_to_string(&log).unwrap_or_default();
        let tail: Vec<&str> = tail.lines().rev().take(12).collect();
        return Err(e).with_context(|| {
            format!(
                "`{}` was forked (pid {pid}) and killed again, because it never bound. Last log \
                 lines from {}:\n{}",
                d.name,
                log.display(),
                tail.into_iter().rev().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")
            )
        });
    }

    // R5.S3: `probe = "http"` — bound is not enough; wait (within the
    // declared start budget) for the health URL to answer 200 before the row
    // is written, so a booting service is not read as unhealthy and killed.
    let health = launch
        .health
        .as_ref()
        .map(|h| expand_hosted(&expand(h, &data_dir), &launch.addr))
        .unwrap_or_default();
    if d.probe == "http" {
        if let Err(e) = jobs::wait_healthy(&health, &mut child, Duration::from_millis(d.start_timeout_ms)) {
            signal(pid, &d.stop_signal);
            std::thread::sleep(Duration::from_millis(500));
            signal(pid, "KILL");
            let _ = child.wait();
            return Err(e).with_context(|| {
                format!("`{}` (pid {pid}) bound {} but never became healthy; killed", d.name, launch.addr)
            });
        }
    }

    let hosted = seams::Hosted {
        health,
        service: d.name.clone(),
        addr: launch.addr.clone(),
        probe: d.probe.clone(),
        pid,
        version: label.clone(),
        commit: rec.commit.clone(),
        dirty: rec.dirty,
        binary: binary.display().to_string(),
        data_dir,
    };
    // EMPTY TOKEN: this binary cannot echo one, so the row records that fact
    // and `probe`'s ceiling reads it. See `record_hosted`.
    let seam = seams::record_hosted(&env.name, &env.data_root, &env.seams(), &hosted, "")?;
    // Handed to the daemon's reaper. If this incarnation of the daemon exits,
    // the handle goes with it and the child is reparented to init — which is
    // exactly what re-attach relies on.
    if let Ok(mut kids) = daemon::FORKED.lock() {
        kids.push(child);
    }

    // ── GOOD IS NOT RECORDED AT BIND (R9.C3, gap C-3) ───────────────────
    //
    // This used to write "good" here, on the evidence that the build bound
    // its address. A build that binds and then panics 20 s later binds on
    // every launch, so it was recorded good on every launch — and became its
    // own rollback target. The daemon now records a hosted build good only
    // once it has seen it up `stable_after_secs` (`daemon::tend`, through
    // [`record_good_if_stable`]).

    Ok(seam)
}

/// **THE STABLE-PERIOD RULE, in one place.** A launch has proven its build
/// when it has been up `stable_after_secs` or longer. Bound, or registered,
/// is not that proof (gaps C-3, C-4).
pub(crate) fn stayed_up(up_secs: u64, stable_after_secs: u64) -> bool {
    up_secs >= stable_after_secs
}

/// Seconds since the Unix epoch — the clock a seam row's `started_at` is on.
pub(crate) fn secs_since_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// **RECORD `label` GOOD FOR `service` IN `env` — IF IT HAS STAYED UP.**
///
/// "Good" is what `auto_rollback` and `cauldron rollback` roll back TO, so it
/// is written only on proof that the build runs: up `stable_after_secs` or
/// longer (`up_secs`, measured by the caller from the row's `started_at`).
/// Bound, or registered, is not that proof (gap C-3). Returns `Ok(true)` when
/// it recorded, `Ok(false)` when the run is not yet stable and nothing was
/// written. Re-recording the label already at the head of the good list is a
/// no-op in [`Manifest::record_good`], so a caller may call this every pass.
///
/// The daemon calls it for every row it sees live, hosted and registering
/// (R9.C4, gap C-4). `cauldron start` and `cauldron test` apply the same rule
/// through [`stayed_up`] to a manifest they already hold.
pub(crate) fn record_good_if_stable(
    env: &Environment,
    service: &str,
    label: &str,
    up_secs: u64,
    stable_after_secs: u64,
) -> Result<bool> {
    if !stayed_up(up_secs, stable_after_secs) {
        return Ok(false);
    }
    // R9.C7f5: only the holder calls this, so the lock wait is bounded; a
    // busy lock is an error the daemon logs, and it records on a later pass.
    Manifest::update_within(&env.manifest(), daemon::HOLDER_ENV_LOCK_BOUND, |m| {
        m.record_good(service, label);
        Ok(())
    })?;
    Ok(true)
}

/// `${CAULDRON_SERVICE_DATA_DIR}` in a declaration's `[env]` value.
///
/// The one substitution, deliberately. A declaration is shared by every
/// environment — that is the whole point of it being per cauldron root — so a
/// foreign daemon's `THREADS_HOME` cannot be a literal without being wrong in
/// every environment but one. This is the smallest thing that fixes that, and
/// it is not a template language: one name, no conditionals, no nesting.
fn expand(v: &str, data_dir: &Path) -> String {
    v.replace("${CAULDRON_SERVICE_DATA_DIR}", &data_dir.display().to_string())
}

/// The port half of a hosted address, for `${CAULDRON_HOSTED_PORT}`.
fn hosted_port(addr: &str) -> &str {
    addr.rsplit(':').next().unwrap_or("")
}

/// `${CAULDRON_HOSTED_ADDR}` / `${CAULDRON_HOSTED_PORT}` in a HOSTED
/// declaration's `[env]` value: the address this ENVIRONMENT hosts it on,
/// which is the declaration's `[launch] addr` unless `env.toml` overrides it.
/// Only the hosted path expands these; a registering service is handed
/// `CAULDRON_BIND=127.0.0.1:0` and has no declared address to pass on.
fn expand_hosted(v: &str, addr: &str) -> String {
    v.replace("${CAULDRON_HOSTED_ADDR}", addr).replace("${CAULDRON_HOSTED_PORT}", hosted_port(addr))
}

/// **`cauldron restart <service>`** — stop it and launch it on the pinned
/// version.
///
/// In an environment a daemon HOLDS, this is a request the daemon performs:
/// the hold is taken, so this process may not fork a competitor, and the
/// restart goes over the supervisor's own seam. In an unheld environment it is
/// done here. Either way exactly one process ever forks a service in one
/// environment, and which one it is is decided by the kernel lock rather than
/// by convention.
fn verb_restart(args: &[String]) -> Result<i32> {
    let o = parse(args)?;
    let env = Environment::resolve()?;
    println!("{}", env.banner());
    let decls = pinned_targets(&env, &o.services, "restart")?;
    match held_by_a_daemon(&env)? {
        Some(addr) => {
            for d in &decls {
                let reply = ask_supervisor(&addr, &format!("RESTART {}", d.name))?;
                println!(
                    "cauldron: environment `{}` is HELD by a supervisor at {addr}; asked it to \
                     restart `{}` -> {reply}",
                    env.name, d.name
                );
            }
            println!(
                "cauldron: the supervisor performs it. This process did not fork anything, \
                 because one environment has one spawner and the kernel lock decides which."
            );
            println!(
                "cauldron: a service that is running turns and whose build can DRAIN is asked to \
                 drain first and restarted when they end (R9.C7d); `cauldron status` shows the \
                 drain. One that cannot drain is restarted now, and that {}.",
                daemon::KILLS_TURNS
            );
            Ok(0)
        }
        None => {
            let m = Manifest::load(&env.manifest())?;
            let mut failed = false;
            for d in &decls {
                let table = Table::load(&env.name, &env.data_root, &env.seams())?;
                if let Some(seam) = table.seams.get(&d.name) {
                    let v = seams::probe(seam);
                    // C-1: an owned pid still draining with its port closed is
                    // waited for too, never launched on top of. C-1b: and so
                    // is an owned pid alive in ANY probe state (`unhealthy`,
                    // `unknown:timeout`, `stale:wrong-*`) — it is stopped
                    // first, or the launch below would sit beside it.
                    if matches!(v.state, SeamState::Live | SeamState::LiveUnverified)
                        || v.owned_and_running(seam)
                    {
                        // The RUNNING version's stop signal, from its own
                        // build's declaration — not the shipped file's.
                        let (sig, esc) = stop_terms(&env, seam);
                        if let Err(e) =
                            stop_by_seam(&env, seam, &sig, esc, supervisions::Why::REQUESTED)
                        {
                            eprintln!("cauldron: stop {} failed: {e:#}", d.name);
                            failed = true;
                            continue;
                        }
                    }
                }
                match resolve_launch(&env, &m, &d.name)
                    .and_then(|l| launch_for_daemon(&env, &l, supervisions::Why::REQUESTED).map(|r| (l.source, r)))
                {
                    Ok((source, (pid, label, addr))) => println!(
                        "cauldron: restarted `{}` in `{}` at {addr} (pid {pid}, version {label}; \
                         declaration: {})",
                        d.name,
                        env.name,
                        source.describe()
                    ),
                    Err(e) => {
                        eprintln!("cauldron: restart {} failed: {e:#}", d.name);
                        failed = true;
                    }
                }
            }
            Ok(if failed { 1 } else { 0 })
        }
    }
}

/// **IS A DAEMON HOLDING THIS ENVIRONMENT, AND WHERE IS ITS SEAM?**
///
/// Answered by PROBING the supervisor's own row, not by reading the lock file.
/// The lock is the kernel's and cannot be inspected from outside; its note is
/// advisory and is only ever read to compose an error message. So the question
/// a verb actually needs — *can I talk to the holder* — is answered the same
/// way every other liveness question in this crate is: a handshake.
fn held_by_a_daemon(env: &Environment) -> Result<Option<String>> {
    let t = Table::load(&env.name, &env.data_root, &env.seams())?;
    let Some(seam) = t.seams.get(daemon::SELF_SERVICE) else { return Ok(None) };
    if seams::probe(seam).state == SeamState::Live {
        return Ok(Some(seam.addr.clone()));
    }
    Ok(None)
}

/// One line to the supervisor's seam, one line back.
fn ask_supervisor(addr: &str, line: &str) -> Result<String> {
    use std::io::BufRead;
    let sa: std::net::SocketAddr = addr.parse()?;
    let s = env::egress::connect_tcp("supervisor", &sa, Duration::from_secs(2))?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut w = s.try_clone()?;
    w.write_all(format!("{line}\n").as_bytes())?;
    w.flush()?;
    let mut out = String::new();
    std::io::BufReader::new(s).read_line(&mut out)?;
    Ok(out.trim().to_string())
}

/// **`cauldron versions [service]` — every installed version, and which of them
/// this environment has proven good.**
///
/// The reading verb of the protocol. Builds are shared across environments
/// (`data/shared/builds/`) and "good" is per environment, so this is the one
/// view where those two facts sit in the same table — which is what makes
/// `rollback`'s target something the operator can see before he asks for it.
fn verb_versions(args: &[String]) -> Result<i32> {
    let o = parse(args)?;
    let env = Environment::resolve()?;
    let decls = owned_targets(&env, &o.services, "versions")?;
    let m = Manifest::load(&env.manifest())?;
    if !o.json {
        println!("{}", env.banner());
    }
    #[derive(serde::Serialize)]
    struct VRow {
        service: String,
        label: String,
        commit: String,
        dirty: bool,
        profile: String,
        built_at: u64,
        /// Is this the label the environment is pinned to?
        pinned: bool,
        /// Has this label been proven good in THIS environment, and where in
        /// the history? `Some(0)` is the current good one, `Some(1)` the
        /// previous — which is what `rollback` re-pins.
        good_rank: Option<usize>,
    }
    let mut rows: Vec<VRow> = Vec::new();
    for d in &decls {
        let dir = env.builds_for(&d.name);
        let pin = m.pin(&d.name);
        let good = m.good(&d.name);
        let mut labels: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if let Some(l) = e.file_name().to_str() {
                    if e.path().join("build.json").exists() {
                        labels.push(l.to_string());
                    }
                }
            }
        }
        let mut recs: Vec<BuildRecord> = Vec::new();
        for l in &labels {
            let p = dir.join(l).join("build.json");
            if let Ok(t) = std::fs::read_to_string(&p) {
                if let Ok(r) = serde_json::from_str::<BuildRecord>(&t) {
                    recs.push(r);
                }
            }
        }
        recs.sort_by(|a, b| b.built_at.cmp(&a.built_at));
        for r in recs {
            rows.push(VRow {
                service: d.name.clone(),
                commit: r.commit.clone(),
                dirty: r.dirty,
                profile: r.profile.clone(),
                built_at: r.built_at,
                // `Some(label) == pin`: an UNPINNED service marks no label
                // pinned, where before it compared against the inherited
                // `"latest"` and only failed to mark one because no commit
                // label is ever spelled `latest`.
                pinned: pin.as_deref() == Some(r.label.as_str()),
                good_rank: good.iter().position(|g| g == &r.label),
                label: r.label,
            });
        }
    }
    if o.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(0);
    }
    println!(
        "{:<10} {:<24} {:<14} {:<9} {}",
        "SERVICE", "VERSION", "COMMIT", "PROFILE", "IN THIS ENVIRONMENT"
    );
    for r in &rows {
        let mut notes: Vec<String> = Vec::new();
        if r.pinned {
            notes.push("PINNED".into());
        }
        match r.good_rank {
            Some(0) => notes.push("good (current)".into()),
            Some(1) => notes.push("good (PREVIOUS — rollback target)".into()),
            Some(n) => notes.push(format!("good (#{n} back)")),
            None => {}
        }
        println!(
            "{:<10} {:<24} {:<14} {:<9} {}",
            r.service,
            r.label,
            if r.commit.is_empty() {
                "-".to_string()
            } else {
                let sh: String = r.commit.chars().take(12).collect();
                if r.dirty { format!("{sh}+dirty") } else { sh }
            },
            r.profile,
            if notes.is_empty() { "-".to_string() } else { notes.join(", ") }
        );
    }
    if rows.is_empty() {
        println!("(nothing is built for these services in this root)");
    }
    for d in &decls {
        match m.pin(&d.name).as_deref() {
            Some("latest") => println!(
                "\n`{}` is pinned to `latest`, which is a QUESTION and not a version: it \
                 resolves to whatever was built most recently and will resolve to something \
                 else tomorrow. `latest` is never recorded as a known-good version for exactly \
                 that reason. Pin a label to make this environment's version a fact.",
                d.name
            ),
            // THE ABSENCE, SAID AS AN ABSENCE. This branch used to be
            // unreachable: an unpinned service inherited `"latest"` and got
            // the paragraph above, which told the operator that a service
            // `{}` does not run is pinned to something.
            None => println!(
                "\n`{}` is NOT pinned in `{}`, so this environment does not run it — presence \
                 is the pin and there is no second list. The builds above are listed because a \
                 declaration says what a service IS, not that this environment runs it. \
                 `cauldron pin {} <version>` is what makes it run here.",
                d.name, env.name, d.name
            ),
            Some(_) => {}
        }
    }
    Ok(0)
}

/// **R9.C7c: THE LINE `status` PRINTS FOR A RUNNING SERVICE'S RESTART GATE**,
/// when there is something to say: it is DRAINING (`draining since <t>, N
/// running, cap at <t>`), or it is pinned to another version and that re-pin
/// is waiting on its quiet signal (gap C-7's half of R9.C7 inspect F1: a wait
/// that was said once, into the daemon's log, and nowhere an operator reads).
/// `None` for a service with no `[quiet]`, not running, or with nothing
/// pending. One GET of the service's own gate, the same one the daemon reads.
fn repin_wait(
    env: &Environment,
    m: &Manifest,
    table: &Table,
    r: &Row,
    held: Option<&serde_json::Value>,
) -> Option<(String, serde_json::Value)> {
    let seam = table.seams.get(&r.service)?;
    if !matches!(seams::probe(seam).state, SeamState::Live | SeamState::LiveUnverified) {
        return None;
    }
    let mut gate = daemon::read_gate(env, seam);
    // R9.C7f: the terms the holder drains under — the pin's override, if any.
    let ovr = daemon::pin_drain_override(m, &r.service);
    gate.drain = gate.drain.map(|d| d.with_override(ovr.clone().ok().flatten()));
    let pinned = r.pinned.clone();
    let repin = pinned
        .as_deref()
        .and_then(|p| decl::resolve_version(&env.builds_for(&r.service), p, &env.name).ok())
        .map(|(l, _)| l)
        .filter(|l| *l != seam.version);
    let drain = gate.drain.as_ref().filter(|d| d.served && d.since.is_some());
    if let Some(d) = drain {
        let since = d.since.unwrap_or_default();
        let cap_at = d.cap_at().unwrap_or_default();
        let n = d.running.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
        // R9.C7b (E18-15): the drain's AGE and the turns it waits for, so a
        // drain that is holding intake is visible as one, not only its start.
        let age = secs_since_epoch().saturating_sub(since);
        let waits = match d.turn_ids.as_deref() {
            Some([]) | None => String::new(),
            Some(ids) => format!("; waiting for: {}", ids.join(", ")),
        };
        let terms = match (d.declared, &ovr) {
            (Some((a, c)), _) => format!(
                "; at_cap {} / cap {}s OVERRIDDEN by this pin (the running build declares {} / {}s)",
                daemon::at_cap_name(d.stanza.at_cap),
                d.stanza.cap_secs,
                daemon::at_cap_name(a),
                c
            ),
            (None, Err(e)) => format!("; this pin's drain override is IGNORED ({e})"),
            (None, Ok(_)) => String::new(),
        };
        let line = format!(
            "draining since {} (for {}), {n} running, cap at {} ({}s) — {}; at the cap {}{waits}{terms}",
            jobs::utc_iso(since),
            span_words(age),
            jobs::utc_iso(cap_at),
            d.stanza.cap_secs,
            match &repin {
                Some(l) => format!("then restarted onto {l}"),
                None => "the pin is the running version again, so the daemon cancels it".into(),
            },
            daemon::at_cap_words(d.stanza.at_cap)
        );
        let json = serde_json::json!({
            "draining_since": since, "draining_secs": age, "running": d.running, "cap_at": cap_at,
            "cap_secs": d.stanza.cap_secs, "at_cap": format!("{:?}", d.stanza.at_cap).to_lowercase(),
            "waiting_for": d.turn_ids, "repin_to": repin,
            "overridden": d.declared.is_some(),
            "declared": d.declared.map(|(a, c)| serde_json::json!({"at_cap": daemon::at_cap_name(a), "cap_secs": c})),
        });
        return Some((line, json));
    }
    let repin = repin?;
    // R9.C7b2: THE HOLDER'S OWN RECORD of this wait, when it has one for this
    // target: since when it has declined the re-pin and the reason IT read.
    // That is the signal being read, not re-derived — a fresh gate read here
    // has no "since", and a wait that is hours old looked like one a second old.
    if let Some(dfr) = held
        .map(|h| &h["deferred"])
        .filter(|d| d["repin_to"].as_str() == Some(repin.as_str()))
    {
        let since = dfr["since"].as_u64().unwrap_or_default();
        let age = secs_since_epoch().saturating_sub(since);
        let why = dfr["why"].as_str().unwrap_or("?").to_string();
        let kind = dfr["kind"].as_str().unwrap_or("?").to_string();
        let reason = match kind.as_str() {
            "busy" => format!(
                "busy ({why}) and its running build has no drain door, so the daemon will not \
                 restart it until a pass reads it quiet"
            ),
            "unreadable" => format!(
                "its quiet signal cannot be read ({why}) — fail safe, the daemon will not restart it"
            ),
            other => format!("{other} ({why})"),
        };
        let line = format!(
            "re-pin to {repin} WAITING since {} (for {}): {reason}. `cauldron restart {}` restarts \
             it now, and if turns are running that {}",
            jobs::utc_iso(since),
            span_words(age),
            r.service,
            daemon::KILLS_TURNS
        );
        return Some((
            line,
            serde_json::json!({
                "repin_to": repin, "waiting": why, "waiting_kind": kind,
                "waiting_since": since, "waiting_secs": age,
            }),
        ));
    }
    let (line, why) = match &gate.quiet {
        daemon::QuietRead::Undeclared => return None,
        daemon::QuietRead::Quiet(read) => {
            (format!("re-pin to {repin} pending: quiet now ({read}), restarted on the daemon's next pass"), read)
        }
        daemon::QuietRead::Busy(read) => (format!("re-pin to {repin} WAITING: busy ({read})"), read),
        daemon::QuietRead::Unreadable(why) => (
            format!("re-pin to {repin} WAITING: its quiet signal cannot be read ({why}) — fail safe, not restarted"),
            why,
        ),
    };
    Some((line.clone(), serde_json::json!({"repin_to": repin, "waiting": why})))
}

/// The holding daemon's `STATUS` answer, if a daemon holds `env` and answers.
fn holder_status(env: &Environment) -> Option<serde_json::Value> {
    let addr = held_by_a_daemon(env).ok().flatten()?;
    let line = ask_supervisor(&addr, "STATUS").ok()?;
    serde_json::from_str(&line).ok()
}

/// `service`'s row in a holder's `STATUS` answer.
fn holder_row<'a>(holder: Option<&'a serde_json::Value>, service: &str) -> Option<&'a serde_json::Value> {
    holder?["hosting"].as_array()?.iter().find(|r| r["service"] == service)
}

/// **R9.C7b2: A PARKED SERVICE, AND WHAT IT IS WAITING ON**, from the holder's
/// own row. A STOP-park waits for `cauldron restart` and nothing else (a pin
/// does not revive it, R9.C2); a FAILURE-park waits for a pin or rollback to a
/// build other than the one it was parked on, or a restart.
fn parked_wait(held: Option<&serde_json::Value>) -> Option<(String, serde_json::Value)> {
    let row = held.filter(|r| r["parked"].as_bool() == Some(true))?;
    let service = row["service"].as_str().unwrap_or("?");
    let by = row["parked_by"].as_str().unwrap_or("?");
    let on = row["parked_on"].as_str();
    let line = match by {
        "stop" => format!(
            "PARKED: stopped by request, waiting for `cauldron restart {service}` (a pin does not \
             revive a stopped service)"
        ),
        _ => format!(
            "PARKED: the daemon gave up on {} after {} failed starts, waiting for a `cauldron \
             pin`/`cauldron rollback` to a different build, or `cauldron restart {service}`",
            on.unwrap_or("a pin that resolved to no build"),
            row["start_failures"].as_u64().unwrap_or_default()
        ),
    };
    Some((line, serde_json::json!({"parked_by": by, "parked_on": on})))
}

/// `3s`, `4m05s`, `2h35m`: how long a drain has run, said the way a person
/// reads it (R9.C7b).
fn span_words(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// **PIN-RACE (CLI-ISO-I F2): WHAT A RE-PIN LANDS ON, read BEFORE it is
/// written.** The running instance, and the holder's own row for the service
/// (its park), taken while the pin holds the env.toml writers' lock and before
/// it saves. The holder takes that same lock to PARK a service (a stop-park or
/// a failure-park, `daemon::HOLDER_PARK_LOCK_BOUND`), and it cannot act on the
/// new pin (revive a park, restart onto it) before the save makes the pin
/// visible. So read-then-save is one step against every park transition: what
/// [`repin_consequence`] says is the state the pin was written over.
///
/// Before, the pin was saved FIRST and the park was read after, and with
/// `poll_secs = 1` the holder could clear a failure-park and launch the new
/// build in between, so `pin` said "is not running … next start launches"
/// for a service it had just revived.
struct RepinGround {
    /// The service's row when it answered Live, else `None`.
    running: Option<Seam>,
    /// The holder's `STATUS` answer, when a holder answered it.
    holder: Option<serde_json::Value>,
    /// A daemon holds this environment.
    held: bool,
}

fn repin_ground(env: &Environment, service: &str) -> RepinGround {
    let running = Table::load(&env.name, &env.data_root, &env.seams())
        .ok()
        .and_then(|t| t.seams.get(service).cloned())
        .filter(|s| matches!(seams::probe(s).state, SeamState::Live | SeamState::LiveUnverified));
    let addr = held_by_a_daemon(env).ok().flatten();
    let holder = addr
        .as_deref()
        .and_then(|a| ask_supervisor(a, "STATUS").ok())
        .and_then(|l| serde_json::from_str(&l).ok());
    RepinGround { running, holder, held: addr.is_some() }
}

/// PIN-RACE test hook: with `CAULDRON_TEST_PIN_READ_MARK` set, `pin` writes
/// that file once it has read what the pin lands on (under the lock, before
/// the save), so a test can land a park exactly inside the read→write step
/// (`CAULDRON_TEST_PIN_WINDOW_MS` holds it open). Unset, it does nothing.
fn pin_read_mark() {
    if let Some(p) = std::env::var_os("CAULDRON_TEST_PIN_READ_MARK") {
        let _ = std::fs::write(p, b"read\n");
    }
}

/// **R9.C7 (gap C-7): WHAT HAPPENS TO THE RUNNING INSTANCE AFTER A RE-PIN**,
/// said as it is rather than as it was once hoped to be.
///
/// The verb itself starts and stops nothing. But a daemon HOLDING the
/// environment re-reads the pin every `poll_secs` and restarts a running
/// instance onto it (`daemon::tend`), so "nothing was started or stopped" was
/// true of the command and false of the next four seconds. This names which of
/// those it is: no running instance, already this version, no daemon to act,
/// `auto_restart = false`, or the daemon will restart it — immediately when
/// the running version declares no quiet signal, at the first quiet pass when
/// it does, and NOT AT ALL while that signal cannot be read (fail safe).
fn repin_consequence(env: &Environment, m: &Manifest, service: &str, label: &str, ground: &RepinGround) -> String {
    let resolved = decl::resolve_version(&env.builds_for(service), label, &env.name)
        .map(|(l, _)| l)
        .unwrap_or_else(|_| label.to_string());
    let Some(seam) = ground.running.clone() else {
        // R9.C7b2 (R9.C2-3): under a holder a PARKED service does not simply
        // "launch this version next": what it does depends on the park.
        // PIN-RACE: the park as it stood when the pin was WRITTEN (read before
        // the save, under the writers' lock), never as it stands afterwards.
        if let Some(row) = holder_row(ground.holder.as_ref(), service).filter(|r| r["parked"].as_bool() == Some(true)) {
            return match (row["parked_by"].as_str(), row["parked_on"].as_str()) {
                (Some("stop"), _) => format!(
                    "nothing was started or stopped: `{service}` was STOPPED by request in `{}` and \
                     the daemon holding it keeps it down — a pin does not revive a stopped \
                     service. It is waiting for `cauldron restart {service}`, which starts this \
                     version",
                    env.name
                ),
                (_, Some(on)) if on == resolved => format!(
                    "nothing was started or stopped: `{service}` is PARKED in `{}` on {on}, the \
                     build this pin names, and it STAYS PARKED. It is waiting for a pin or rollback \
                     to a DIFFERENT build, or `cauldron restart {service}` to try {on} again",
                    env.name
                ),
                (_, _) if !m.supervisor.auto_restart => format!(
                    "nothing was started or stopped: `{service}` is PARKED in `{}` and STAYS \
                     PARKED — `auto_restart = false` there, so a pin does not revive it. It is \
                     waiting for `cauldron restart {service}`",
                    env.name
                ),
                (_, on) => format!(
                    "this command signalled nothing, but `{service}` is PARKED in `{}` on {} and this \
                     pin names a different build, so the daemon holding it clears the park and \
                     launches {resolved} on its next pass (every {}s)",
                    env.name,
                    on.unwrap_or("a pin that resolved to no build"),
                    m.supervisor.poll_secs
                ),
            };
        }
        return format!(
            "nothing was started or stopped: `{service}` is not running in `{}`, so this version \
             is what its next start launches",
            env.name
        );
    };
    if seam.version == resolved {
        // R9.C7c: a pin BACK to the running build ends a drain in progress.
        let gate = daemon::read_gate(env, &seam);
        if let Some(since) = gate.drain.filter(|d| d.served).and_then(|d| d.since) {
            return format!(
                "nothing was started or stopped: the running `{service}` (pid {}) is already \
                 version {}. It has been DRAINING since {} for the re-pin this replaces; the \
                 daemon holding `{}` cancels that drain on its next pass and its queued prompts \
                 start",
                seam.pid,
                seam.version,
                jobs::utc_iso(since),
                env.name
            );
        }
        return format!(
            "nothing was started or stopped: the running `{service}` (pid {}) is already version {}",
            seam.pid, seam.version
        );
    }
    if !ground.held {
        return format!(
            "nothing was started or stopped. The running `{service}` (pid {}, version {}) keeps \
             running until it is restarted — no daemon holds `{}` to do that — and `cauldron \
             restart {service}` does it now",
            seam.pid, seam.version, env.name
        );
    }
    let poll = m.supervisor.poll_secs;
    if !m.supervisor.auto_restart {
        return format!(
            "nothing was started or stopped, and the daemon holding `{}` will not restart the \
             running `{service}` (version {}) onto this pin: `auto_restart = false` there. \
             `cauldron restart {service}` does it",
            env.name, seam.version
        );
    }
    let lead = format!(
        "this command signalled nothing, but the daemon holding `{}` re-reads the pin every {poll}s \
         and restarts the running `{service}` (pid {}, version {}) onto it",
        env.name, seam.pid, seam.version
    );
    let mut gate = daemon::read_gate(env, &seam);
    // R9.C7f: the terms the holder will drain under for THIS pin.
    gate.drain = gate.drain.map(|d| d.with_override(daemon::pin_drain_override(m, service).ok().flatten()));
    if let (daemon::QuietRead::Busy(read), Some(d)) = (&gate.quiet, gate.drain.as_ref().filter(|d| d.served)) {
        let n = d.running.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
        let cap = d.stanza.cap_secs;
        let when = match d.since {
            Some(s) => format!("it has been draining since {}", jobs::utc_iso(s)),
            None => "the daemon asks it to DRAIN on its next pass".into(),
        };
        return format!(
            "{lead}, WITHOUT INTERRUPTING THE {n} TURN(S) RUNNING: it is busy ({read}), so {when} \
             — it starts no new turn and queues new prompts, which the new version runs — and \
             it is restarted when the running turns finish, or at the cap ({cap}s after the \
             drain began), when {}.{} `cauldron status` shows the drain",
            daemon::at_cap_words(d.stanza.at_cap),
            daemon::override_words(d)
        );
    }
    match gate.quiet {
        daemon::QuietRead::Undeclared => format!(
            "{lead} on its next pass, WHETHER OR NOT IT IS MID-WORK: version {} declares no quiet \
             signal to wait for",
            seam.version
        ),
        daemon::QuietRead::Quiet(read) => format!(
            "{lead} on its next pass: it reads quiet right now ({read}), and the daemon reads it \
             again before it acts"
        ),
        daemon::QuietRead::Busy(read) => format!(
            "{lead} at the first pass that reads it quiet, not before: it is busy right now ({read})"
        ),
        daemon::QuietRead::Unreadable(why) => format!(
            "{lead} only once its quiet signal can be read, and it cannot be now ({why}), so the \
             daemon will NOT restart it (fail safe). `cauldron restart {service}` would restart \
             it now, and if turns are running that {}",
            daemon::KILLS_TURNS
        ),
    }
}

/// **`cauldron pin <service> <version>` — tell this environment what to run.**
///
/// Rollback's inverse, and the thing `test` prints a line of. It only edits
/// `env.toml` — this process starts, stops and signals nothing. What happens
/// NEXT to a running instance is not this verb's act and is said after the pin
/// by [`repin_consequence`]: under a holding daemon it is restarted onto the
/// pin, at a quiet moment when its running version declares a quiet signal.
/// **Test-only widening of `pin`'s read-modify-write (R9.C7f4).** With
/// `CAULDRON_TEST_PIN_WINDOW_MS` set, `pin` sleeps that long between loading
/// env.toml and saving it, so a test of concurrent pins exercises an overlap
/// it would otherwise only sometimes hit. Unset (always, outside tests) it
/// does nothing.
/// R9.C7f5 test hook: `CAULDRON_TEST_ENSURE_WINDOW_MS` holds a first-create
/// between `ensure_manifest`'s unlocked existence check and the create, and
/// says on stderr that it is in the window, so a test can land a concurrent
/// `pin` exactly there. Unset, it does nothing.
fn ensure_test_window() {
    if let Some(ms) = std::env::var("CAULDRON_TEST_ENSURE_WINDOW_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
        eprintln!("cauldron(test): ensure_manifest window open for {ms}ms");
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

fn pin_test_window() {
    if let Some(ms) = std::env::var("CAULDRON_TEST_PIN_WINDOW_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

fn verb_pin(args: &[String]) -> Result<i32> {
    // `--addr <host:port>` (R6.S3): this environment's address for a HOSTED
    // service. Taken off before the shared parser, which knows no such flag.
    let mut addr: Option<String> = None;
    // R9.C7f: `--at-cap` / `--drain-cap`, this pin's drain terms. Checked
    // HERE, before anything is read or written, so a refused value can never
    // reach the holder and no drain opens under it.
    let mut at_cap: Option<daemon::AtCap> = None;
    let mut drain_cap: Option<u64> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--addr" {
            addr = Some(it.next().cloned().context("`--addr` needs a host:port after it")?);
        } else if let Some(v) = a.strip_prefix("--addr=") {
            addr = Some(v.to_string());
        } else if a == "--at-cap" || a.starts_with("--at-cap=") {
            let v = match a.strip_prefix("--at-cap=") {
                Some(v) => v.to_string(),
                None => it.next().cloned().context("`--at-cap` needs `handoff` or `extend` after it")?,
            };
            at_cap = Some(daemon::parse_at_cap(&v).context("REFUSED to pin")?);
        } else if a == "--drain-cap" || a.starts_with("--drain-cap=") {
            let v = match a.strip_prefix("--drain-cap=") {
                Some(v) => v.to_string(),
                None => it.next().cloned().context("`--drain-cap` needs a number of seconds after it")?,
            };
            drain_cap = Some(daemon::parse_drain_cap(&v).context("REFUSED to pin")?);
        } else {
            rest.push(a.clone());
        }
    }
    let o = parse(&rest)?;
    let env = Environment::resolve()?;
    let (service, label) = match o.services.as_slice() {
        [s, l] => (s.clone(), l.clone()),
        other => bail!(
            "`cauldron pin` takes exactly a service and a version (got {}). \
             Try `cauldron versions` to see what is installed.",
            other.len()
        ),
    };
    let d = decl::load(&env.service_dir(), &service)?;
    // A pin is an instruction to the daemon to LAUNCH this service; while the
    // outside launcher still owns it, that instruction would start a second one.
    if let Some(mg) = &d.migration {
        bail!("{}", mg.refusal(&d.name));
    }
    println!("{}", env.banner());
    // The label must EXIST as a build before it is pinned. A pin to a version
    // that was never built is a refusal deferred to the next `start`, in a
    // different session, with less context — and `resolve_version` is the thing
    // that would have to refuse it, which has no idea who asked for it or when.
    if label != "latest" {
        let (_, rec) = decl::resolve_version(&env.builds_for(&service), &label, &env.name)?;
        println!(
            "cauldron: version {} of `{service}` is commit {} (profile {})",
            rec.label,
            rec.commit_short(),
            rec.profile
        );
        // ── THE DECLARATION THIS PIN WOULD LAUNCH UNDER, AGAINST THE FILE ──
        //
        // A pinned version launches under the snapshot in its build directory
        // and never under the shipped file, so a snapshot that differs from
        // the file is exactly the situation a cut-over is in: the operator is
        // about to run a build whose contract is not the one in his checkout.
        // That is permitted — it is the whole point — and it is SAID.
        let shipped_sha = decl::sha256_hex(d.text.as_bytes());
        match decl::snapshot(&env.build_dir(&service, &label), &rec, &service) {
            Ok(decl::Snapshot::Present { sha256, source, .. }) if sha256 == shipped_sha => {
                println!(
                    "cauldron: its declaration snapshot matches the shipped file {} (sha256 {})",
                    d.loaded_from.display(),
                    &sha256[..12]
                );
                let _ = source;
            }
            Ok(decl::Snapshot::Present { sha256, source, .. }) => {
                eprintln!(
                    "cauldron: WARNING: version {label}'s declaration snapshot (sha256 {}, taken \
                     from {}) DIFFERS from the shipped declaration now at {} (sha256 {}). The \
                     pinned build launches under ITS SNAPSHOT — the file that was in force when \
                     it was built — and not under the shipped one. That is a cut-over: the \
                     running contract and the checked-in contract disagree until a build made \
                     under the new file is pinned. `cauldron build --snapshot-declaration \
                     {service} {label} --force` would re-snapshot it from today's file instead.",
                    &sha256[..12],
                    if source.is_empty() { "an unrecorded path" } else { &source },
                    d.loaded_from.display(),
                    &shipped_sha[..12]
                );
            }
            Ok(decl::Snapshot::Absent { .. }) => {
                eprintln!(
                    "cauldron: WARNING: version {label} carries NO declaration snapshot, so it \
                     launches under whatever {} says at the time — today, sha256 {}. A build \
                     made before snapshots existed has no contract of its own; backfill one \
                     deliberately with `cauldron build --snapshot-declaration {service} {label}` \
                     once you have checked that today's file is the one it was built under.",
                    d.loaded_from.display(),
                    &shipped_sha[..12]
                );
            }
            Err(e) => {
                // Refused, by the same logic as a label that is not built: a
                // pin to a version that cannot launch is a refusal deferred to
                // the next `start`, in a different session, with less context.
                return Err(e).with_context(|| {
                    format!(
                        "REFUSED to pin `{service}` to {label}: its declaration snapshot would be \
                         refused at launch, so the pin would be a pin to nothing"
                    )
                });
            }
        }
    }
    ensure_manifest(&env, std::slice::from_ref(&d))?;
    // R9.C7f4 (C-7, ledger R9.C7f3-2): env.toml is re-read UNDER the writers'
    // lock and the lock is held to the save, so a concurrent `cauldron pin`
    // (or the holder clearing a spent override) can no longer load the same
    // file and have one save drop the other's pin.
    let _env_toml = Manifest::lock(&env.manifest())?;
    let mut m = Manifest::load(&env.manifest())?;
    // PIN-RACE: what this pin lands on, read under the lock and BEFORE the
    // save (see `RepinGround`).
    let ground = repin_ground(&env, &service);
    pin_read_mark();
    pin_test_window();
    // ── THE HOSTED ADDRESS, CHECKED AGAINST EVERY ENVIRONMENT IN THE ROOT ──
    //
    // A registering service binds 127.0.0.1:0 and cannot collide, so `--addr`
    // on one is refused rather than written and ignored. A hosted service's
    // effective address — the new `--addr`, else this environment's existing
    // override, else the declaration's — is refused here if any other
    // environment (prod included) claims that port. Before R6.S3 this was
    // discovered by the daemon at launch, in a log, and only while the other
    // environment was up.
    if let Some(a) = &addr {
        if d.registers {
            bail!(
                "REFUSED: `--addr` is for a HOSTED service; `{service}` registers its own seam on                  a kernel-assigned port (127.0.0.1:0), so it has no address to declare"
            );
        }
    }
    // REL-4.S8b: `--addr auto` — prod keeps the declared address only if it is
    // free and unclaimed; every other environment gets its own port, never
    // one of prod's (`envs::auto_hosted_addr`).
    if addr.as_deref() == Some("auto") {
        let declared = d.launch.as_ref().map(|l| l.addr.clone());
        addr = envs::auto_hosted_addr_here(&env, &service, declared.as_deref())?;
        match &addr {
            None => println!(
                "cauldron: `--addr auto`: `{service}` in `{}` keeps its declared address {} (free, \
                 and no other environment claims it)",
                env.name,
                declared.as_deref().unwrap_or("(none)")
            ),
            Some(a) => println!(
                "cauldron: `--addr auto`: `{service}` in `{}` gets its own port {a}{}",
                env.name,
                if env.name == "prod" {
                    format!(
                        " (its declared address {} is in use or claimed by another environment)",
                        declared.as_deref().unwrap_or("(none)")
                    )
                } else {
                    " (only `prod` binds the declared default)".to_string()
                }
            ),
        }
    }
    if let Some(a) = &addr {
        a.parse::<std::net::SocketAddr>()
            .with_context(|| format!("`--addr {a}` is not a host:port socket address"))?;
    }
    if !d.registers {
        let effective = addr
            .clone()
            .or_else(|| m.hosted_addr(&service))
            .or_else(|| d.launch.as_ref().map(|l| l.addr.clone()));
        if let Some(a) = &effective {
            envs::refuse_addr_collision(&env, &service, a)?;
        }
    }
    let was = m.pin(&service);
    m.set_version(&service, &label);
    // R9.C7f: this pin's drain terms, stamped with the pin they belong to.
    // No flags: no override (and any a previous pin carried is cleared).
    let entry = m.services.get_mut(&service).expect("set_version inserts the pin");
    entry.drain = (at_cap.is_some() || drain_cap.is_some()).then(|| decl::PinDrain {
        for_version: label.clone(),
        at_cap: at_cap.map(|a| daemon::at_cap_name(a).to_string()),
        cap_secs: drain_cap,
    });
    if let Some(a) = &addr {
        m.services.get_mut(&service).expect("set_version inserts the pin").addr = Some(a.clone());
        println!("cauldron: `{service}` in `{}` is hosted at {a} (this environment's override)", env.name);
    }
    m.save(&env.manifest())?;
    println!(
        "cauldron: `{service}` in `{}` is pinned {} -> {} ({})",
        env.name,
        // A FIRST PIN IS NOT A RE-PIN, and this line used to say it was: with
        // the old default it printed `latest -> <label>`, naming a prior pin
        // that never existed and implying the environment already ran the
        // service.
        match &was {
            Some(w) => format!("{w}"),
            None => "NOTHING (it was not pinned here, so this environment did not run it)".into(),
        },
        label,
        env.manifest().display()
    );
    if at_cap.is_some() || drain_cap.is_some() {
        println!(
            "cauldron: this pin carries its own drain terms (at_cap {}, cap {}): the holder drains \
             the running `{service}` for it under these in place of its running build's \
             `[quiet.drain]` values",
            at_cap.map(daemon::at_cap_name).unwrap_or("as declared"),
            drain_cap.map(|c| format!("{c}s")).unwrap_or_else(|| "as declared".into())
        );
    }
    println!("cauldron: {}.", repin_consequence(&env, &m, &service, &label, &ground));
    Ok(0)
}

/// **`cauldron rollback <service>` — re-pin this environment to the PREVIOUS
/// version that was good here.**
///
/// # Why this is a two-line function and not a subsystem
///
/// Because the hard part of rollback is not the act, it is knowing what to roll
/// back TO — and once the version identity is a commit and "good" is recorded by
/// the thing that proved it, the target is simply `good[1]`. There is nothing to
/// search, nothing to compare, no artifact to keep in reserve and nothing that
/// can be lost: the label names a commit, the commit names the source, and both
/// survive being copied, which is the property the thing this replaces did not
/// have.
///
/// # What it does NOT do
///
/// It does not stop or start anything. A rollback is a decision about what runs
/// next, and separating it from the restart is what lets the operator roll back
/// now and absorb it at a quiet moment — which on this machine is how every
/// deploy already works.
fn verb_rollback(args: &[String]) -> Result<i32> {
    let o = parse(args)?;
    let env = Environment::resolve()?;
    let decls = owned_targets(&env, &o.services, "rollback")?;
    println!("{}", env.banner());
    // R9.C7f4: held from the load to the save (ledger R9.C7f3-2).
    let _env_toml = Manifest::lock(&env.manifest())?;
    let mut m = Manifest::load(&env.manifest())?;
    let mut changed = false;
    let mut stuck = false;
    for d in &decls {
        let good = m.good(&d.name);
        let Some(prev) = m.previous_good(&d.name) else {
            // NOT an error, and not silent either. "There is nothing to roll
            // back to" is a true and useful answer, and the shape of the
            // history is the explanation.
            eprintln!(
                "cauldron: `{}` in `{}` has no PREVIOUS good version to roll back to. \
                 Versions proven good here, newest first: {}. A version becomes good by \
                 STARTING successfully in this environment — registering its seam and \
                 answering for its warnings shape — so an environment that has run one \
                 version, or none, has nothing behind it. Nothing was changed.",
                d.name,
                env.name,
                if good.is_empty() { "(none)".to_string() } else { good.join(", ") }
            );
            stuck = true;
            continue;
        };
        // `Some(prev)`: an unpinned service is not "already pinned to prev".
        // Reachable in principle — `cauldron rollback <service>` names a
        // service explicitly and does not go through the pinned-only sweep —
        // and with the old default it compared `"latest"` against a label,
        // which happened to be right for the wrong reason.
        let was = m.pin(&d.name);
        if was.as_deref() == Some(prev.as_str()) {
            println!(
                "cauldron: `{}` in `{}` is already pinned to {prev}, which IS the previous good \
                 version. Nothing was changed — a rollback that re-pins what you are already on \
                 and reports success is a rollback that did nothing.",
                d.name, env.name
            );
            continue;
        }
        // The rollback TARGET must still be installed. A label in the history
        // whose build directory has been deleted is a target that cannot be
        // run, and finding that out at the next `start` is finding it out too
        // late.
        let (_, rec) = decl::resolve_version(&env.builds_for(&d.name), &prev, &env.name).with_context(|| {
            format!(
                "`{}`'s previous good version {prev} is in this environment's history but is \
                 NOT installed in this root any more. The history records what ran; it does not \
                 keep the bytes. The label names commit {} — rebuild it from there \
                 (`git checkout`, `cauldron build {}`) and the label will come back identical \
                 if the commit is the same.",
                d.name,
                prev.split('+').next().unwrap_or(&prev),
                d.name
            )
        })?;
        m.set_version(&d.name, &prev);
        changed = true;
        println!(
            "cauldron: ROLLED BACK `{}` in `{}`: {} -> {} (commit {})",
            d.name,
            env.name,
            was.clone().unwrap_or_else(|| "NOTHING (it was not pinned here)".into()),
            prev,
            rec.commit_short()
        );
        println!(
            "cauldron:   that version started successfully in `{}` before. {}.",
            env.name,
            repin_consequence(&env, &m, &d.name, &prev, &repin_ground(&env, &d.name))
        );
    }
    if changed {
        m.save(&env.manifest())?;
    }
    // Exit 7: asked to roll back and there was nowhere to go. Its own code so a
    // script cannot read "no previous version" as "rolled back".
    Ok(if stuck { 7 } else { 0 })
}

// -------------------------------------------------------------------- start

fn verb_start(args: &[String]) -> Result<i32> {
    let o = parse(args)?;
    let env = Environment::resolve()?;
    env.ensure_dirs()?;
    println!("{}", env.banner());
    let decls = pinned_targets(&env, &o.services, "start")?;

    // ── `start` IN A HELD ENVIRONMENT IS A NO-OP THAT SAYS SO ────────────
    //
    // His ruling, verbatim: *"that can just be a no-op that says 'cauldron is
    // already running' or something."*
    //
    // It is not politeness. When a daemon holds this environment, that daemon
    // is the only thing that may fork a service here, and it launches whatever
    // the pin names on its own schedule. A `start` that forked anyway would be
    // a SECOND spawner — and for a hosted service, whose port is declared
    // rather than kernel-assigned, the second spawner's child lands on the
    // same data directory. The pre-fork bind check in `launch_hosted` would
    // catch that one, but a registering service has no such check and does not
    // need one precisely because nothing else spawns it.
    //
    // So the hold decides, and it decides by being a kernel lock rather than
    // by this process asking politely. The check below is a PROBE of the
    // supervisor's own seam, not a read of the lock file: the lock cannot be
    // inspected from outside and its note is advisory.
    if let Some(addr) = held_by_a_daemon(&env)? {
        println!(
            "cauldron: `{}` is ALREADY HELD by a cauldron daemon at {addr}, so there is nothing \
             for `start` to do and it did nothing.",
            env.name
        );
        println!(
            "cauldron:   That daemon is the only thing that forks a service in this \
             environment, and it launches whatever `{}` pins. Nothing was spawned here, because \
             two spawners in one environment is how a service gets started twice on one data \
             directory.",
            env.manifest().display()
        );
        println!(
            "cauldron:   To make it run something: `cauldron pin <service> <version>` — the \
             daemon picks the pin up. To restart one now: `cauldron restart <service>`, which \
             asks the holder over its seam. To see what it is holding: `cauldron status`."
        );
        return Ok(0);
    }

    let mut m = ensure_manifest(&env, &decls)?;
    let mut failed = false;
    let mut promoted: Vec<(String, String)> = Vec::new();
    for d in &decls {
        // THE SAME LAUNCH PATH THE DAEMON USES, so `start` and the daemon
        // cannot disagree about what starting means. It routes on `registers`:
        // a registering service writes its own seam (`start_one`), a hosted
        // one is forked and polled and the supervisor writes the row. And the
        // declaration it routes on is the BUILD's (`resolve_launch`), not the
        // `d` this loop is iterating, which is only the shipped name list.
        match resolve_launch(&env, &m, &d.name).and_then(|l| {
            let launched = launch_for_daemon(&env, &l, supervisions::Why::REQUESTED)?;
            Ok((l.source, launched))
        }).and_then(|(source, (pid, label, addr))| {
            let t = Table::load(&env.name, &env.data_root, &env.seams())?;
            let seam = t.seams.get(&d.name).cloned().with_context(|| {
                format!(
                    "`{}` came up at {addr} as pid {pid} on version {label}, but no row for it \
                     is in {} — which should be impossible, since both launch paths write one \
                     before returning.",
                    d.name,
                    env.seams().display()
                )
            })?;
            Ok((source, seam))
        }) {
            Ok((source, seam)) => {
                println!(
                    "cauldron: started {} in `{}` at {} (pid {}, version {}, commit {})",
                    seam.service,
                    env.name,
                    seam.addr,
                    seam.pid,
                    seam.version,
                    if seam.commit.is_empty() {
                        "-".to_string()
                    } else {
                        seam.commit.chars().take(12).collect()
                    }
                );
                println!("cauldron:   declaration: {}", source.describe());
                // ── "GOOD" IS NOT RECORDED AT START (R9.C4, gap C-4) ──────
                //
                // `start_one` returning `Ok` means the service registered its
                // own seam carrying this start's boot token and answered for its
                // warnings shape; `launch_hosted` returning means it bound. Both
                // are proof the process LAUNCHED, and neither is proof the
                // build WORKS: a build that binds and panics 20 s later passes
                // both, every time. This used to record good right here, so
                // such a build became a rollback target.
                //
                // So `start` applies the same rule as the daemon: good only
                // once the row has been up `stable_after_secs`. At start that
                // is true only for an environment that declares
                // `stable_after_secs = 0`; otherwise the holder records it
                // (`daemon::tend`), when it has seen it stay up — including a
                // holder that re-attaches what this `start` launched.
                let up_secs = secs_since_epoch().saturating_sub(seam.started_at);
                let stable_after = m.supervisor.stable_after_secs;
                if stayed_up(up_secs, stable_after) {
                    let before = m.previous_good(&d.name);
                    m.record_good(&d.name, &seam.version);
                    promoted.push((d.name.clone(), seam.version.clone()));
                    if before.as_deref() != m.previous_good(&d.name).as_deref() {
                        if let Some(prev) = m.previous_good(&d.name) {
                            println!(
                                "cauldron: `{}` in `{}` can now roll back to {} \
                                 (`cauldron rollback {}`)",
                                d.name, env.name, prev, d.name
                            );
                        }
                    }
                } else {
                    println!(
                        "cauldron:   {} is not recorded GOOD yet: a build is good in `{}` once \
                         it has stayed up {stable_after}s (`stable_after_secs`), and the \
                         daemon holding this environment records it when it sees that.",
                        seam.version, env.name
                    );
                }
            }
            Err(e) => {
                eprintln!("cauldron: start {} failed: {e:#}", d.name);
                failed = true;
            }
        }
    }
    if !promoted.is_empty() {
        // Saved once, after the loop, so one service's failure does not lose
        // another service's promotion and a crash mid-loop cannot leave the
        // file half-written. R9.C7f4: re-applied to a fresh read under the
        // writers' lock, not saved from the copy loaded before the launches,
        // which a `pin` made meanwhile would be dropped by.
        Manifest::update(&env.manifest(), |fresh| {
            for (name, version) in &promoted {
                fresh.record_good(name, version);
            }
            Ok(())
        })?;
    }
    Ok(if failed { 1 } else { 0 })
}

/// Launch one service, and do not call it started until it has proved it is a
/// cauldron service.
///
/// # REFUSE, not warn — and what was rejected
///
/// A service that does not serve warnings is refused. The alternative on the
/// table was a loud warning: easier onboarding, no two-step for a new service,
/// nothing blocked. It was rejected for a reason specific to this contract
/// rather than a general preference for strictness.
///
/// **The enforcement for "your warnings reach nobody" must not itself be a
/// warning.** This feature exists because 27,705 warnings went unread for five
/// weeks in a log nobody opened. A warn-only gate would print, into that same
/// stream, a line saying that a service's warnings are going nowhere — and
/// would be the one guard in this system whose failure mode is *identical to
/// the defect it guards*. It would work exactly until somebody stopped reading,
/// which is the event the whole feature is a response to.
///
/// The second reason is that the aggregate would be **silently incomplete**.
/// `cauldron status` prints a total. A warn-only gate means that total can be
/// missing a producer that has been running for a month, and the only record of
/// that is a line that scrolled past at start. A refusal makes the aggregate
/// complete *by construction*: everything running was admitted, and everything
/// admitted answers.
///
/// What the refusal costs, stated rather than waved away: every new service is
/// a two-step — write the declaration, then make the binary answer `WARNINGS`.
/// Three things keep that bounded. The default shape is already correct, so
/// nothing has to be declared. The refusal prints the exact JSON line the
/// service has to answer with, so the second step is a copy. And it is
/// reversible in one line of TOML, where five weeks of unread warnings is not.
fn start_one(env: &Environment, l: &Launchable) -> Result<Seam> {
    // THE DECLARATION IS THE BUILD'S — see `resolve_launch`.
    let d = &l.decl;
    let label = l.label.clone();
    let rec = &l.rec;
    // REFUSAL ONE, BEFORE ANYTHING IS SPAWNED: the declaration confesses that
    // this service serves no warnings shape. There is nothing to ask, so there
    // is no point launching it and asking.
    if d.warnings_shape().is_none() {
        let c = Compliance::RefusedByDeclaration { service: d.name.clone() };
        bail!(
            "REFUSED TO START `{}`: {}\n\
             Nothing was spawned. A cauldron service serves a warnings shape; the supervisor \
             aggregates by subscribing to every producer as an ordinary consumer, so a producer \
             that serves none is a hole in the aggregate and the aggregate cannot tell you it is \
             there.",
            d.name,
            c.missing().unwrap_or_default()
        );
    }

    // Singleton PER ENVIRONMENT, not per machine. The same service live in
    // another environment is none of this environment's business.
    let table = Table::load(&env.name, &env.data_root, &env.seams())?;
    if let Some(existing) = table.seams.get(&d.name) {
        let v = seams::probe(existing);
        match v.state {
            SeamState::Live | SeamState::LiveUnverified => bail!(
                "`{}` is already {} in environment `{}` at {} (pid {}). \
                 Stop it first, or start it in another environment.",
                d.name,
                v.state.as_str(),
                env.name,
                existing.addr,
                existing.pid
            ),
            _ => eprintln!(
                "cauldron: clearing a stale seam for `{}` ({}): {}",
                d.name,
                v.state.as_str(),
                v.detail
            ),
        }
    }

    // A build installed before commits were the identity has no commit to hand
    // the service, and `ServiceEnv::from_env` requires one. Refused here, where
    // the message can say what the build is and what to do, rather than inside
    // the service as a missing-variable complaint.
    if !rec.names_a_commit() {
        bail!(
            "REFUSED to start `{}` on version `{label}`: that build record names no commit, so \
             there is no version identity to hand the service and nothing the registry could be \
             clear about. It was installed before the commit became the version. Rebuild it \
             (`cauldron build {}`) and pin the label that prints — the old build is left where it \
             is.",
            d.name,
            d.name
        );
    }
    let binary = env.build_dir(&d.name, &label).join(&d.build()?.bin);
    if !binary.exists() {
        bail!("version `{label}` of `{}` has no binary at {}", d.name, binary.display());
    }

    let data_dir = env.service_data(&d.name)?;
    std::fs::create_dir_all(&data_dir)?;
    let log = env.log(&d.name)?;
    std::fs::create_dir_all(log.parent().unwrap())?;
    // DG-2: rotated by rename past 64 MiB, 3 generations kept, before it is opened (logrotate.rs).
    let logfile = logrotate::open_log(&log)?;
    let errfile = logfile.try_clone()?;

    let token = mint_token();
    let mut cmd = Command::new(&binary);
    cmd.current_dir(&data_dir)
        .env("CAULDRON_SERVICE", &d.name)
        // CAULDRON_ENV, never ENV: ENV is reserved by POSIX sh, and a service
        // must read the authoritative variable.
        .env("CAULDRON_ENV", &env.name)
        .env("CAULDRON_HOME", &env.root)
        .env("CAULDRON_DATA_ROOT", &env.data_root)
        .env("CAULDRON_SERVICE_DATA_DIR", &data_dir)
        .env("CAULDRON_SEAMS", env.seams())
        .env("CAULDRON_BOOT_TOKEN", &token)
        .env("CAULDRON_VERSION", &label)
        // The version identity, handed to the service so that the seam IT
        // writes carries the commit. The supervisor never writes a seam it did
        // not see a service report, so this is the only route by which a
        // registered row can carry a commit at all.
        .env("CAULDRON_COMMIT", &rec.commit)
        .env("CAULDRON_DIRTY", if rec.dirty { "1" } else { "0" })
        .env("CAULDRON_BINARY", &binary)
        .env("CAULDRON_PROBE", &d.probe)
        // Port shouldn't matter: the kernel picks, the service reports.
        .env("CAULDRON_BIND", "127.0.0.1:0")
        .env_remove("ENV")
        .stdin(Stdio::null())
        .stdout(Stdio::from(logfile))
        .stderr(Stdio::from(errfile));
    for (k, v) in &d.env {
        cmd.env(k, v);
    }
    // Its own process group, so it outlives this shell and a Ctrl-C here does
    // not reach it. Stable std API; no unsafe, no libc.
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {} for service `{}`", binary.display(), d.name))?;
    let pid = child.id();

    // Wait for the service to write its OWN seam, carrying OUR boot token.
    // Nothing else counts as started: the CLI never writes an address it did
    // not see a service report.
    let deadline = Instant::now() + Duration::from_millis(d.start_timeout_ms);
    let seam = loop {
        if let Ok(t) = Table::load(&env.name, &env.data_root, &env.seams()) {
            if let Some(s) = t.seams.get(&d.name) {
                if s.boot_token == token {
                    break s.clone();
                }
            }
        }
        if let Ok(Some(st)) = child.try_wait() {
            let tail = std::fs::read_to_string(&log).unwrap_or_default();
            let tail: Vec<&str> = tail.lines().rev().take(12).collect();
            bail!(
                "`{}` exited with {st} before writing its seam. Last log lines from {}:\n{}",
                d.name,
                log.display(),
                tail.into_iter().rev().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")
            );
        }
        if Instant::now() > deadline {
            let _ = Command::new("/bin/kill").args(["-TERM", &pid.to_string()]).status();
            // R5.S8: handed to the reaper, not dropped, so it is not left
            // `<defunct>` under a holder.
            hand_to_reaper(child);
            bail!(
                "`{}` did not write its seam into {} within {}ms; killed pid {}. Log: {}",
                d.name,
                env.seams().display(),
                d.start_timeout_ms,
                pid,
                log.display()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    // REFUSAL TWO, AFTER THE SEAM AND BEFORE WE CALL IT STARTED.
    //
    // The seam being written is the OLD definition of started. The contract
    // adds one clause to it: a cauldron service is started when it has
    // registered its address AND answered for its warnings shape. Both halves
    // are handshakes with the process, which is what makes neither of them
    // something a declaration can claim.
    //
    // A service that fails here is not left running. The existing timeout path
    // already kills what it spawned; a half-admitted service is worse than
    // either outcome, because it is a producer nothing is aggregating that
    // `status` would list as live.
    let shape = d.warnings_shape().unwrap_or(env::warnings::SHAPE);
    let verdict = seams::probe(&seam);
    let c = warn::check(&seam, &verdict, d.warnings_shape());
    if let Some(missing) = c.missing() {
        signal(pid, "TERM");
        let _ = wait_gone(&seam, Duration::from_secs(2));
        signal(pid, "KILL");
        let _ = clear(env, &d.name);
        let _ = child.wait();
        bail!(
            "`{}` registered a seam at {} and is NOT STARTED: {}\n\
             It was killed (pid {}) and its seam was cleared, so nothing is running that the \
             warnings aggregate cannot see. The supervisor asked it, on its own socket, as an \
             ordinary consumer — it does not take a declaration's word for this.",
            d.name,
            seam.addr,
            missing,
            pid
        );
    }
    // Say the number out loud at start, once. This is the first thing the
    // aggregate will carry, and a producer whose very first export is already
    // non-zero is worth seeing at the moment it is admitted.
    if let Some(e) = c.export() {
        if e.total() > 0 || e.sink_errors > 0 {
            eprintln!(
                "cauldron: `{}` is already reporting {} warn / {} error on its `{shape}` shape \
                 (counting since {})",
                d.name, e.warn, e.error, e.counts_since
            );
        }
    }
    // R5.S8 (D-4): THE HANDLE IS KEPT, as `launch_hosted` keeps it. Dropping it
    // here is what left every registering service a holder stopped (39035, at
    // tts's cut-over) a zombie under it, one per request, until the holder
    // exited. In a one-shot CLI the process exits in a moment and init reaps.
    hand_to_reaper(child);
    Ok(seam)
}

/// Give a forked child to the daemon's reaper (`daemon::FORKED`).
fn hand_to_reaper(child: std::process::Child) {
    if let Ok(mut kids) = daemon::FORKED.lock() {
        kids.push(child);
    }
}

// --------------------------------------------------------------------- stop

fn verb_stop(args: &[String]) -> Result<i32> {
    let o = parse(args)?;
    let env = Environment::resolve()?;
    println!("{}", env.banner());
    let decls = pinned_targets(&env, &o.services, "stop")?;
    // ── C-5: A HELD ENVIRONMENT IS STOPPED BY ITS HOLDER, OR NOT AT ALL ──
    //
    // `restart` and `start` both asked this question and `stop` did not. So in
    // a held environment `stop` signalled the service itself, printed
    // "stopped", and one supervision pass later (`poll_secs × down_after`,
    // ~12 s at the defaults) the holder found it down and relaunched it. An
    // explicit stop the supervisor undoes is not a stop. The holder's own
    // `STOP` request stops the service AND parks it, in the one process that
    // would otherwise relaunch it.
    if let Some(addr) = held_by_a_daemon(&env)? {
        return stop_through_holder(&env, &addr, &decls);
    }
    // NOT reachable as a holder — but "the supervisor's seam does not answer"
    // is not "nothing holds this environment". The kernel lock is the
    // authority, so take it: if it is taken, a holder exists that we cannot
    // talk to (wedged, starting, or SIGSTOPped), and stopping the service
    // behind its back would print "stopped" for something it may relaunch the
    // moment it answers again. If it is free, it is HELD BY THIS PROCESS for
    // the length of the stop, so no holder can start between the check and
    // the signal and relaunch what we are stopping.
    let _hold = match env::hold::try_hold(
        &env.data_root,
        env::hold::note_for(&env.name, "cauldron stop", SELF_COMMIT),
    )? {
        env::hold::Attempt::Held(h) => h,
        env::hold::Attempt::Taken { path, note } => {
            eprintln!("cauldron: {}", env::hold::refusal(&env.name, &path, note.as_ref()));
            println!(
                "cauldron: NOTHING WAS STOPPED in `{}`. The environment is held by a supervisor, \
                 but its seam does not answer, so it cannot be asked to stop (and park) anything; \
                 signalling a service behind its back would only be undone when it answers \
                 again. Find out why the holder is not answering (`cauldron status`), then retry.",
                env.name
            );
            return Ok(1);
        }
    };
    let table = Table::load(&env.name, &env.data_root, &env.seams())?;
    // Set when `stop` declined to signal something it does not own. It is a
    // distinct exit code (5) rather than 0, because "I did not do what you
    // asked" must not look like "done".
    let mut refused = false;
    for d in &decls {
        let Some(seam) = table.seams.get(&d.name) else {
            println!("cauldron: `{}` has no seam in `{}`; nothing to stop", d.name, env.name);
            continue;
        };
        let v = seams::probe(seam);
        // ───────── THE REFUSAL THAT COMES BEFORE THE STALENESS CHECK ─────────
        //
        // **Provenance is checked before liveness, because they are different
        // questions and this one is the stronger.** The staleness refusal
        // below says: *do not signal a pid you cannot prove still owns this
        // address* — it is about DOUBT. This one says: *do not signal a pid
        // that was not established by something that counts as proof* — and no
        // amount of liveness evidence satisfies it.
        //
        // What it can refuse is now narrow, and narrow BY DELETION rather than
        // by judgment. `Seam::signalable` accepts exactly two pid sources —
        // `self` (the service's own getpid, on a row it wrote) and `child`
        // (the value the kernel handed the supervisor at the fork) — and the
        // inferring sources are gone from `PidSource` entirely. So the only
        // rows this can refuse are one written before hosting and one somebody
        // hand-edited.
        //
        // The DECLARATION is no longer consulted here, and that is a real
        // change rather than an oversight. It used to be, because `threads`
        // and `tts` were adopted: two edits to `seams.json` (`adopted: false`
        // plus `pid_source: "self"`) would otherwise have handed `stop` a
        // licence to SIGKILL the live daemon, so the operator's declaration
        // was the authority on what cauldron owned. Cauldron now owns
        // everything it declares, so there is no declaration that would
        // disagree and nothing for a second authority to protect. What remains
        // is the protection that was doing the work: a pid field whose only
        // two legal values are both proof.
        let refusal = seam.refusal_to_signal();
        if let Some(why) = refusal {
            eprintln!("cauldron: NOT signalling pid {} for `{}`: {}", seam.pid, d.name, why);
            println!(
                "cauldron: `{}` is unchanged in `{}` at {}; no signal was sent and the seam was \
                 left in place (it is not a lie — the process really is there). Stopping it is \
                 the owner's job: {}",
                d.name,
                env.name,
                seam.addr,
                "the recorded pid's provenance is neither `self` nor `child`, so nothing \
                 here proves this pid belongs to a process cauldron is responsible for"
            );
            refused = true;
            continue;
        }
        match v.state {
            SeamState::Live | SeamState::LiveUnverified => {
                // We proved the listener is ours before signalling anything.
                // The lesson is on this machine already: the live `threads`
                // daemon ran with PPID 1 and `launchctl bootout` did not end
                // it. A pid is not ownership.
                //
                // THE SIGNAL AND THE ESCALATION ARE THE DECLARATION'S, through
                // `stop_by_seam` — the one stop path. A hardcoded SIGTERM here
                // would kill `tts` without a drain, because `tts` handles only
                // SIGINT. And it is the declaration of the version that is
                // RUNNING (its build's snapshot), not the shipped file's: the
                // signal a daemon handles is a property of the binary that is
                // up. `d` is only how this loop found the row.
                let (sig, esc) = stop_terms(&env, seam);
                if let Err(e) =
                    stop_by_seam(&env, seam, &sig, esc, supervisions::Why::REQUESTED)
                {
                    eprintln!("cauldron: {e:#}");
                    refused = true;
                }
            }
            // C-1 (R9.C1 inspect F1): port quiet, OWNED pid alive — the service
            // is mid-drain. This row is NOT a lie, and clearing it deleted the
            // only evidence the launch backstop reads, so a `start` right after
            // put a second instance on the same data directory. `stop_by_seam`
            // waits for the pid (no second signal) and clears the row only
            // once it is gone.
            //
            // C-1b (second inspection N1): and the same for an owned pid that
            // is alive in ANY other state — `unhealthy`, `unknown:timeout`,
            // `stale:wrong-*`. The probe said the SERVICE is not good; it did
            // not say the PROCESS is gone, and clearing its row unsignalled
            // left it running with nothing recording it. `stop_by_seam`
            // signals it (or, mid-drain, waits) and clears the row only once
            // the pid has exited.
            _ if v.owned_and_running(seam) => {
                let (sig, esc) = stop_terms(&env, seam);
                if let Err(e) =
                    stop_by_seam(&env, seam, &sig, esc, supervisions::Why::REQUESTED)
                {
                    eprintln!("cauldron: {e:#}");
                    refused = true;
                }
            }
            other => {
                // REFUSE to signal. The seam is stale, so the recorded pid may
                // be anyone's — including a reused pid. Clear the lie, signal
                // nothing. (Reached only when the row's own process is NOT
                // running — dead, reused, or not askable — since C-1b.)
                eprintln!(
                    "cauldron: NOT signalling pid {} for `{}`: the seam is {} — {}",
                    seam.pid,
                    d.name,
                    other.as_str(),
                    v.detail
                );
                clear(&env, &d.name)?;
                println!(
                    "cauldron: cleared the stale seam for `{}` in `{}`; no signal was sent",
                    d.name, env.name
                );
            }
        }
    }
    // Exit 5: nothing was stopped because cauldron does not own it. Separate
    // from 0 (stopped), from 1 (an error) and from 3/4 (status's codes), so a
    // script cannot read a refusal as a success.
    Ok(if refused { 5 } else { 0 })
}

/// What the holder did with a `STOP`, read back from its own `STATUS`.
enum HeldStop {
    /// Parked, and the service's row is gone or stale: it is down and the
    /// holder will not relaunch it.
    Down,
    /// Parked, but the service still answers at its row: the holder's stop
    /// failed (see its log) — it is NOT stopped.
    StillUp,
    /// The holder had not reported it parked when the budget ran out.
    NotConfirmed,
}

/// **C-5: `stop` in a held environment — ask the holder, then report what it
/// actually did.**
///
/// Exit codes: 0 every service confirmed down and parked; 1 the holder refused
/// a request, could not be reached, or parked a service that is still up; 6 the
/// holder accepted the stop but had not confirmed it within the budget (it may
/// still be draining) — distinct from 0 so a script cannot read "asked" as
/// "stopped".
fn stop_through_holder(env: &Environment, addr: &str, decls: &[Declaration]) -> Result<i32> {
    let m = Manifest::load(&env.manifest())?;
    let rows = Table::load(&env.name, &env.data_root, &env.seams()).ok();
    let mut rc = 0;
    for d in decls {
        // ── C-5c: THE WAIT IS THE SERVICE'S OWN GRACE, NOT A CONSTANT ──
        //
        // The holder performs asks after its pass (up to one `poll_secs`
        // before it starts), then waits the stop's own grace — the SAME
        // `stop_grace` the holder's `stop_by_seam` uses, from this row's
        // running build and the shipped declaration — and then the SIGKILL
        // wait. A fixed 15 s here reported `threads` (75 s) as "not
        // confirmed" mid-drain. With no row the holder has nothing to signal;
        // the declaration's own key sizes the wait.
        let grace = rows
            .as_ref()
            .and_then(|t| t.seams.get(&d.name))
            .map(|seam| stop_grace(env, seam))
            .unwrap_or_else(|| Duration::from_secs(d.stop_grace_secs));
        let budget = held_stop_budget(m.supervisor.poll_secs, grace);
        let reply = match ask_supervisor(addr, &format!("STOP {}", d.name)) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "cauldron: environment `{}` is HELD by a supervisor at {addr}, and asking it \
                     to stop `{}` failed: {e:#}. `{}` was NOT stopped.",
                    env.name, d.name, d.name
                );
                rc = 1;
                continue;
            }
        };
        if !reply.starts_with("OK") {
            eprintln!(
                "cauldron: the supervisor at {addr} refused to stop `{}`: {reply}. Nothing was \
                 stopped.",
                d.name
            );
            rc = 1;
            continue;
        }
        println!(
            "cauldron: environment `{}` is HELD by a supervisor at {addr}; asked it to stop `{}` \
             -> {reply}",
            env.name, d.name
        );
        match wait_held_stop(env, addr, &d.name, budget) {
            HeldStop::Down => println!(
                "cauldron: stopped `{}` in `{}` — the supervisor stopped it and PARKED it, so it \
                 stays down. `cauldron restart {}` brings it back. (A park belongs to this holder \
                 incarnation: if the holder itself restarts, it launches every pinned service \
                 again.)",
                d.name, env.name, d.name
            ),
            HeldStop::StillUp => {
                eprintln!(
                    "cauldron: the supervisor PARKED `{}` but it is STILL RUNNING — its stop \
                     failed (the holder's log says why). It is not stopped; it will not be \
                     relaunched either.",
                    d.name
                );
                rc = 1;
            }
            HeldStop::NotConfirmed => {
                println!(
                    "cauldron: the supervisor accepted the stop of `{}` but had not confirmed it \
                     within {}s (a draining service can take longer). It was NOT confirmed \
                     stopped; `cauldron status {}` shows where it is.",
                    d.name,
                    budget.as_secs(),
                    d.name
                );
                if rc == 0 {
                    rc = 6;
                }
            }
        }
    }
    Ok(rc)
}

/// **The margin a held `stop` allows past the holder's own work** (C-5c): the
/// holder's 5 s SIGKILL wait after the grace, its reap and row clear, and this
/// process's 200 ms `STATUS` poll — with room to spare.
pub(crate) const HELD_STOP_MARGIN_SECS: u64 = 10;

/// How long a held `stop` waits for the holder to confirm (C-5c): two holder
/// passes (one before it takes the ask, one of slack), the service's declared
/// `grace`, and [`HELD_STOP_MARGIN_SECS`].
fn held_stop_budget(poll_secs: u64, grace: Duration) -> Duration {
    Duration::from_secs(poll_secs * 2 + HELD_STOP_MARGIN_SECS) + grace
}

/// Poll the holder's `STATUS` until it reports `service` parked, then read the
/// service's own row to say whether it is actually down.
fn wait_held_stop(env: &Environment, addr: &str, service: &str, budget: Duration) -> HeldStop {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        let parked = ask_supervisor(addr, "STATUS")
            .ok()
            .and_then(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
            .and_then(|v| {
                v["hosting"].as_array().and_then(|rows| {
                    rows.iter()
                        .find(|r| r["service"] == service)
                        .map(|r| r["parked"].as_bool() == Some(true))
                })
            })
            .unwrap_or(false);
        if parked {
            let up = Table::load(&env.name, &env.data_root, &env.seams())
                .ok()
                .and_then(|t| t.seams.get(service).map(|s| !seams::probe(s).state.is_stale()))
                .unwrap_or(false);
            return if up { HeldStop::StillUp } else { HeldStop::Down };
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    HeldStop::NotConfirmed
}

pub(crate) fn signal(pid: u32, sig: &str) {
    let _ = Command::new("/bin/kill")
        .args([&format!("-{sig}"), &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Has the PROCESS this row describes gone (C-1: its pid, for a row cauldron
/// owns — not its port)? Polled every 50 ms until `budget`.
pub(crate) fn wait_gone(seam: &Seam, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if seams::probe(seam).process_gone(seam) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

pub(crate) fn clear(env: &Environment, service: &str) -> Result<()> {
    seams::update(&env.name, &env.data_root, &env.seams(), |t| {
        t.seams.remove(service);
        Ok(())
    })
}

#[allow(dead_code)]
fn flush() {
    let _ = std::io::stdout().flush();
}

// ═══ `cauldron permissions` ══════════════════════════════════════════════════
//
// Two questions in one report, because they are the two halves of one ask.
//
//   "note that the permissions for the claude commands are given to a system
//    process instead of owned by threads. We should actually make this
//    permissioned to cauldron so permission requests all display 'Cauldron is
//    trying to access ...'."
//
//   "I need a way to see what is actually requesting the permission and give
//    fine-grained access, because macos just says 'Claude is requesting to
//    access data from other apps' over and over, and the truth is more
//    nuanced." / "I need to track which things I am denying access to."
//
// The first half is IDENTITY: which name a prompt would show for each service
// cauldron knows about, declared and then checked against the artifact. The
// second is the RECORD: what has actually been asking, and what he refused.
//
// It grants nothing and it cannot. It reads two things, both without
// privilege: `codesign` on artifacts, and `tccd`'s own log entries. There is
// no code path here that could widen a permission, which is deliberate --
// the standing ruling on this subject is that broad grants are refused
// permanently and are never to be proposed.

/// Read `tccd`'s records for the last `hours` hours.
fn tcc_records(hours: u32) -> Result<Vec<env::tccd::Record>> {
    let out = std::process::Command::new("/usr/bin/log")
        .args([
            "show",
            "--last",
            &format!("{hours}h"),
            "--predicate",
            "subsystem == \"com.apple.TCC\" AND category == \"access\"",
            "--style",
            "compact",
        ])
        .output()
        .context("running `log show` to read tccd's own records")?;
    if !out.status.success() {
        bail!(
            "`log show` failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(env::tccd::parse_log(&String::from_utf8_lossy(&out.stdout)))
}

fn verb_permissions(argv: &[String]) -> Result<i32> {
    let mut hours: u32 = 8;
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--hours" => {
                i += 1;
                hours = argv
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("`--hours` needs a number of hours"))?
                    .parse()
                    .context("`--hours` takes a whole number of hours")?;
                if hours == 0 {
                    bail!(
                        "REFUSED: `--hours 0` is a window containing nothing. A report over no \
                         records would print zero denials, which reads exactly like a machine \
                         with no problem."
                    );
                }
            }
            other => bail!("cauldron permissions: unknown argument `{other}`"),
        }
        i += 1;
    }

    let env = Environment::resolve()?;
    println!("{}", env.banner());
    println!();

    // ── Half one: the identity each service's prompts would name ────────────
    println!("IDENTITY — which name a permission prompt from each service would show");
    println!();
    println!(
        "  {:<10} {:<14} {:<24} {}",
        "service", "responsibility", "declared", "on the artifact"
    );

    let mut defects = 0;
    let scan = env::decl::all(&env.service_dir());
    // A declaration this audit cannot read is a DEFECT IN THE AUDIT'S SCOPE,
    // counted as one: a prompt from that service would name something this
    // report cannot vouch for, which is exactly what the report exists to
    // catch. It is not a reason to print nothing about the other services.
    for (name, e) in &scan.broken {
        println!("  {:<10} {:<14} {:<24} DEFECT: declaration does not load — {e:#}", name, "?", "?");
        defects += 1;
    }
    let decls = scan.ok;
    // The pins, read once: a cargo-built service is audited on the version
    // THIS environment would launch, which only the manifest can say.
    // A malformed `env.toml` is ONE defect row, not the end of the audit: only
    // the cargo rows need the pins (R3.S2.10 inspect note, DEPLOY-8).
    let manifest = Manifest::load(&env.manifest()).map_err(|e| format!("{e:#}"));
    if let Err(e) = &manifest {
        println!("  {:<10} {:<14} {:<24} DEFECT: env.toml does not load — {e}", "(env)", "?", "?");
        defects += 1;
    }
    for d in &decls {
        let resp = d.permissions.responsibility()?;
        let claimed = d.permissions.claimed_identity()?;
        let (found, unread, read_off) = match service_binary(&env, &manifest, d) {
            ServiceBinary::Artifact(p) => {
                (env::identity::Subject::read(&p).ok(), None, String::new())
            }
            ServiceBinary::Built { label, path } => match env::identity::Subject::read(&path) {
                Ok(f) => (Some(f), None, format!(" [read off version {label}]")),
                Err(e) => (None, Some(format!("cargo-built, version {label}: {e:#}")), String::new()),
            },
            ServiceBinary::Unbuilt(why) => (None, Some(why), String::new()),
            ServiceBinary::NoSource => (None, None, String::new()),
        };

        let verdict = match &found {
            Some(f) => {
                let c = env::identity::Claim {
                    service: d.name.clone(),
                    responsibility: resp,
                    claimed: claimed.clone(),
                };
                let v = c.check(f);
                if v.is_defect() {
                    defects += 1;
                }
                format!("{}{read_off}", v.describe())
            }
            None => unread.unwrap_or_else(|| "not built here, so nothing to read".to_string()),
        };

        println!(
            "  {:<10} {:<14} {:<24} {}",
            d.name,
            resp.as_str(),
            claimed.as_ref().map(|i| i.identifier()).unwrap_or("(none)"),
            verdict
        );
    }

    println!();
    let mine = env::identity::Identity::for_env(&env.name)?;
    println!(
        "  Under `inherit` the prompt names THIS environment's cauldron, `{}`",
        mine.identifier()
    );
    println!(
        "  (process `{}`). One identity PER ENVIRONMENT is what makes the grant\n  \
         divisible: approving `prod` does not approve a throwaway `test` env.",
        env::identity::process_name(&env.name)
    );
    println!(
        "  NOTHING HONOURS THAT YET. The responsible process is whatever launchd\n  \
         started, and today that is `/bin/bash` running the external supervisor\n  \
         script — a system binary, which is the defect the operator reported.\n  \
         Cauldron has to BE what launchd starts, and host the services itself,\n  \
         before a prompt can name it. That is the supervisor's work, not this\n  \
         verb's, and a signature here is an IDENTITY, never a gate: nothing\n  \
         below refuses a launch."
    );

    // ── Half two: what has actually been asking ─────────────────────────────
    println!();
    println!("WHAT IS ACTUALLY ASKING — tccd's own records, last {hours}h");
    let records = tcc_records(hours)?;
    let s = env::tccd::Summary::of(&records);
    println!();
    println!("  {} authorisation requests recorded.", s.total);
    println!();
    println!(
        "  {:>6}  {:<10} {:<24} {:<26} {}",
        "count", "outcome", "prompt names (subject)", "process that wanted it", "permission"
    );
    for g in s.groups.iter().take(12) {
        println!(
            "  {:>6}  {:<10} {:<24} {:<26} {}",
            g.count,
            g.value.as_str(),
            truncate(&g.subject, 24),
            truncate(&g.accessing, 26),
            g.service.trim_start_matches("kTCCService")
        );
    }

    // ── The record he asked for: what HE refused ────────────────────────────
    println!();
    println!("DENIED BY HIM — the requests he was shown and refused");
    let denials = s.denial_groups();
    println!();
    if denials.is_empty() {
        println!("  none in this window.");
    } else {
        println!(
            "  {:>6}  {:<24} {:<26} {}",
            "count", "prompt named", "process that wanted it", "permission"
        );
        for g in &denials {
            println!(
                "  {:>6}  {:<24} {:<26} {}",
                g.count,
                truncate(&g.subject, 24),
                truncate(&g.accessing, 26),
                g.service.trim_start_matches("kTCCService")
            );
        }
        println!();
        println!("  {} interruptions in {hours}h.", s.live_denials);
    }

    // ── The durability observation ──────────────────────────────────────────
    println!();
    println!("DURABILITY — whether an answer he gave is ever reused");
    println!();
    println!("  {}", s.durability());
    println!(
        "  This is the one measurement that says whether a NAME change would reduce the\n  \
         prompts or only relabel them. It is read from records that already exist; no\n  \
         prompt was provoked to produce it."
    );

    // Exit 4 on a contradicted claim, matching `status`'s use of 4 for a
    // violation. A gap is not a defect and does not change the exit code.
    Ok(if defects > 0 { 4 } else { 0 })
}

/// Where [`service_binary`] found the bytes a service's signature is read off.
enum ServiceBinary {
    /// `[build] artifact`: the image produced elsewhere, read where it lies.
    Artifact(PathBuf),
    /// A cargo-form declaration (`workspace`/`package`): the binary of the
    /// version this environment would LAUNCH, in its build directory.
    Built { label: String, path: PathBuf },
    /// A cargo-form declaration whose version cannot be read here — no build
    /// of the pin, or the build has no binary. Says why; not a defect.
    Unbuilt(String),
    /// No `[build]` source at all: nothing cauldron makes or copies.
    NoSource,
}

/// Where a service's binary is, for reading its real signature off.
///
/// **The declaration is the only source of truth about this, and there is no
/// second list.** An `artifact` declaration is read at the artifact path, as
/// it always was.
///
/// ADOPT IS GONE, AND THIS WAS THE SINGLE EDIT IT COST. The permission work
/// was written against `[adopt].binary` with a note saying this function would
/// be the one line to change when the supervisor's declaration shape landed —
/// the operator: *"We don't adopt, we host the services. that's why I've been
/// calling it a supervisor."* That shape is now `[build].artifact`, which names
/// the same thing for the same reason: the image produced elsewhere that
/// cauldron copies into a version directory and reads a commit off. Nothing in
/// `identity.rs` or `tccd.rs` moved, which is what that note predicted.
///
/// **A cargo-form declaration (`workspace`/`package`) is read off the build
/// cauldron made of it** (R3.S2.10, ledger line 188). It used to answer "not
/// built here, so nothing to read" — true of the declaration, false of the
/// machine: cauldron DOES build it, and a durable signature landing on that
/// build would have gone unseen. The version is resolved by
/// [`resolve_launch`], the resolver every launch goes through (the pin, else
/// `latest`, and the build's own declaration snapshot), and the file is the
/// one `launch_hosted`/`start_one` exec: `build_dir(service, label)` joined
/// with that declaration's `bin`. There is deliberately no second resolver.
fn service_binary(env: &Environment, m: &Result<Manifest, String>, d: &Declaration) -> ServiceBinary {
    let Some(b) = d.build.as_ref() else { return ServiceBinary::NoSource };
    if let Some(a) = b.artifact.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        return ServiceBinary::Artifact(PathBuf::from(a));
    }
    let m = match m {
        Ok(m) => m,
        Err(e) => return ServiceBinary::Unbuilt(format!("cargo-built, but env.toml does not load: {e}")),
    };
    let l = match resolve_launch(env, m, &d.name) {
        Ok(l) => l,
        Err(e) => {
            return ServiceBinary::Unbuilt(format!(
                "cargo-built, but no version of it can be read here: {e:#}"
            ))
        }
    };
    let bin = match l.decl.build() {
        Ok(b) => b.bin.clone(),
        Err(e) => {
            return ServiceBinary::Unbuilt(format!(
                "cargo-built, version {}: its declaration names no binary: {e:#}",
                l.label
            ))
        }
    };
    let path = env.build_dir(&d.name, &l.label).join(bin);
    if !path.exists() {
        return ServiceBinary::Unbuilt(format!(
            "cargo-built, but version {} has no binary at {}",
            l.label,
            path.display()
        ));
    }
    ServiceBinary::Built { label: l.label, path }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let keep: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{keep}…")
    }
}

// ------------------------------------------------------------- supervisions

/// **`cauldron supervisions` — what the supervisor DID, from its own ledger.**
///
/// The read side of [`env::supervisions`]. It exists because a proof cannot
/// answer "did cauldron restart this" by looking at the process table: the
/// daemon re-pins on purpose, and `threads` self-updates by `execv` and keeps
/// its pid across a real restart. So the question is asked of the only party
/// that knows, and this is the verb that asks it.
///
/// # The exit codes, and the one that matters
///
/// `0` with records and `0` with none are the same kind of answer: the ledger
/// was read and this is what it says. **`5` is NO LEDGER AT ALL**, and it is a
/// separate code rather than an empty result on purpose. An absent file read
/// as zero acts would make "the supervisor restarted nothing" and "no
/// supervisor has ever written here" the same answer — a check that cannot
/// run, reporting as a result, which is the defect class this whole file is
/// careful about. A caller that wants a window to be empty must first know
/// that somebody was writing in it.
///
/// `6` is a ledger with lines that would not parse. Also separate, because a
/// ledger that under-reports is worse than one that is missing: the missing
/// one announces itself.
fn verb_supervisions(args: &[String]) -> Result<i32> {
    let mut f = supervisions::Filter::default();
    let mut json = false;
    let mut i = 0;
    // Its OWN parser rather than the shared `parse`, deliberately: `--since`
    // and `--why` belong to this verb, and adding them to the shared options
    // would make every other verb silently accept them.
    while i < args.len() {
        let a = args[i].as_str();
        let mut take = |name: &str| -> Result<String> {
            i += 1;
            args.get(i)
                .cloned()
                .with_context(|| format!("`{name}` needs a value after it"))
        };
        match a {
            "--json" => json = true,
            "--since" => f.since = Some(take("--since")?),
            "--service" => f.service = Some(take("--service")?),
            "--root" => f.root = Some(take("--root")?),
            "--why" => {
                let w = take("--why")?;
                if !supervisions::Why::ALL.contains(&w.as_str()) {
                    bail!(
                        "`{w}` is not a reason the supervisor can have. The set is closed: {}. \
                         A typo'd filter that matched nothing would print an empty ledger and \
                         look like an answer.",
                        supervisions::Why::ALL.join(", ")
                    );
                }
                f.why = Some(w);
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(0);
            }
            s => bail!("unknown option `{s}` for `cauldron supervisions`\n\n{USAGE}"),
        }
        i += 1;
    }
    let env = Environment::resolve()?;
    let l = supervisions::read(&env.data_root, &f)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "environment": { "name": env.name, "data_root": env.data_root },
                "ledger": l.ledger,
                "exists": l.exists,
                "unreadable": l.unreadable,
                "matched": l.records.len(),
                "records": l.records,
            }))?
        );
        // THE SAME EXIT CODE IN BOTH SHAPES. A `--json` caller that got 0 for
        // an absent ledger while a human got 5 would be the discrimination
        // existing only on the surface nobody automates against.
        if !l.exists {
            return Ok(5);
        }
    } else {
        println!("{}", env.banner());
        if !l.exists {
            println!(
                "cauldron: THERE IS NO SUPERVISION LEDGER at {}.\n\
                 cauldron: That is not the same as \"the supervisor did nothing\". No supervisor \
                 has ever recorded an act in `{}` — either none has run here, or the one that \
                 did predates this ledger. Nothing can be concluded about what was or was not \
                 restarted in this environment, and this verb exits 5 rather than printing an \
                 empty list so that a caller cannot read the absence as a green.",
                l.ledger, env.name
            );
            return Ok(5);
        }
        println!("ledger {} ({} matching)", l.ledger, l.records.len());
        println!(
            "{:<28} {:<10} {:<8} {:<10} {:<9} {}",
            "AT", "SERVICE", "ACT", "WHY", "PID", "VERSION"
        );
        for r in &l.records {
            println!(
                "{:<28} {:<10} {:<8} {:<10} {:<9} {}{}",
                r.at,
                r.service,
                r.act,
                r.why,
                r.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                if r.was_version.is_empty() || r.was_version == r.version {
                    String::new()
                } else {
                    format!("{} -> ", r.was_version)
                },
                if r.version.is_empty() { "-" } else { &r.version },
            );
        }
        if l.records.is_empty() {
            println!(
                "(no act in this ledger matched. The ledger EXISTS, so this is an answer: in \
                 this window, with this filter, the supervisor did nothing.)"
            );
        }
        for r in &l.records {
            println!(
                "\n{} {} `{}` ({})\n  by pid {} (incarnation {}, commit {}, root {})\n  \
                 instance {}\n  {}",
                r.at,
                r.act,
                r.service,
                r.why,
                r.by.pid,
                if r.by.since.is_empty() { "none — a one-shot CLI" } else { &r.by.since },
                if r.by.commit.is_empty() { "-" } else { &r.by.commit },
                r.by.root,
                if r.boot.is_empty() {
                    "NO BOOT TOKEN — this binary cannot echo one, so the pid is the only \
                     handle and it is not an identity"
                        .to_string()
                } else {
                    format!("boot token {}", r.boot)
                },
                r.detail
            );
        }
        if l.unreadable > 0 {
            println!(
                "\ncauldron: {} line(s) in this ledger DID NOT PARSE and are not in the count \
                 above. A ledger that under-reports is worse than one that is absent, because \
                 the absent one announces itself — so this is said here and the exit code says \
                 it too.",
                l.unreadable
            );
        }
    }
    if l.unreadable > 0 {
        return Ok(6);
    }
    Ok(0)
}

#[cfg(test)]
mod self_stamp_refusal {
    use super::*;

    /// `cauldron build` of cauldron ITSELF — an artifact declaration with
    /// `stamp_args = ["--build-stamp"]` — refuses `dirty=unknown` exactly as it
    /// refuses any other service's: `read_stamp` returns `None` and
    /// `install_artifact` bails with "REFUSED to install".
    #[test]
    fn cauldron_refuses_its_own_stamp_when_its_dirt_is_unknown() {
        let c = "0123456789abcdef0123456789abcdef01234567";
        let line = |d: &str| {
            format!("commit={c} provenance=git dirty={d} dirt_scope=Cargo.lock,Cargo.toml,cli,libs/env")
        };
        assert_eq!(parse_stamp(&line("0")).map(|s| s.dirty), Some(false));
        assert_eq!(parse_stamp(&line("1")).map(|s| s.dirty), Some(true));
        assert!(parse_stamp(&line("unknown")).is_none(), "dirty=unknown must be REFUSED, not read as clean");

        // The real stamp of THIS binary parses iff its dirt is known.
        let own = self_stamp();
        let known = own.contains(" dirty=0 ") || own.contains(" dirty=1 ");
        assert_eq!(parse_stamp(&own).is_some(), known && !SELF_COMMIT.is_empty(), "{own}");
    }

    #[test]
    fn the_version_suffix_never_renders_unknown_dirt_as_clean() {
        assert_eq!(dirt_suffix("0"), "");
        assert_eq!(dirt_suffix("1"), " (dirty)");
        assert_eq!(dirt_suffix("unknown"), " (dirt unknown)");
        assert_eq!(dirt_suffix(""), " (dirt unknown)", "an empty value is not clean");
    }
}

/// REL-4.S8: what `scripts/install` relies on when it brings up `threads` for
/// a stranger — the label it pins comes out of `cauldron build --json`, and the
/// pin carries `--addr auto` (REL-4.S8b), so the shipped declaration's address
/// is the port a stranger's prod binds when it is free, and no other
/// environment ever gets it (`envs::auto_hosted_addr`).
#[cfg(test)]
mod install_contract {
    use super::*;

    fn record(label: &str) -> BuildRecord {
        BuildRecord {
            service: "threads".into(),
            label: label.into(),
            bin: "threads".into(),
            package: "threads".into(),
            profile: "release".into(),
            workspace: "/w".into(),
            built_at: 0,
            commit: "feedc0de1234feedc0de1234feedc0de1234feed".into(),
            dirty: false,
            content: "0123456789abcdef".into(),
            source_commit: None,
            declaration_sha256: String::new(),
            declaration_source: String::new(),
        }
    }

    /// The installer's extraction, `grep -oE '"label"[[:space:]]*:[[:space:]]*"[^"]*"'`,
    /// done here on the real renderer's output: exactly one match, and it is
    /// the record's label — not the service, not the commit.
    #[test]
    fn build_json_names_the_label() {
        let out = build_json(&[record("feedc0de1234-release")]).unwrap();
        let hits: Vec<&str> = out
            .lines()
            .filter_map(|l| {
                let t = l.trim_start();
                let rest = t.strip_prefix("\"label\"")?.trim_start().strip_prefix(':')?.trim_start();
                rest.strip_prefix('"')?.split('"').next()
            })
            .collect();
        assert_eq!(hits, vec!["feedc0de1234-release"], "{out}");
        // Opposing arm: a different label is read back as itself.
        let out2 = build_json(&[record("0000aaaa1111-release")]).unwrap();
        assert!(out2.contains("\"label\": \"0000aaaa1111-release\""), "{out2}");
        assert!(!out2.contains("feedc0de1234-release\""), "{out2}");
    }

    /// The shipped `threads` declaration, read with no root override (a fresh
    /// root's `services/` is empty), is a cargo build of `-p threads` at
    /// release whose hosted address is the daemon's own default chassis port.
    #[test]
    fn shipped_threads_declaration_is_buildable_on_default_ports() {
        let empty = std::env::temp_dir().join(format!("cauldron-s8-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let d = decl::load(&empty, "threads").unwrap();
        let _ = std::fs::remove_dir_all(&empty);
        assert!(
            d.loaded_from.ends_with("services/threads.toml"),
            "loaded from the shipped file, not an override: {}",
            d.loaded_from.display()
        );
        let b = d.build.as_ref().expect("threads declares [build]");
        assert_eq!(b.package.as_deref(), Some("threads"));
        assert_eq!(b.bin, "threads");
        assert_eq!(b.profile, "release");
        assert!(b.artifact.is_none(), "a cargo source, built from this tree");
        let l = d.launch.as_ref().expect("threads is hosted: [launch]");
        assert_eq!(l.addr, "127.0.0.1:3653", "a stranger's prod binds the default chassis port");
        assert_eq!(
            d.env.get("THREADS_CHASSIS_PORT").map(String::as_str),
            Some("${CAULDRON_HOSTED_PORT}"),
            "the port follows the pin's address, which install leaves at the default for a prod whose default port is free"
        );
    }
}
