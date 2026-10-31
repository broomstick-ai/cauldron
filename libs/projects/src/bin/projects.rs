//! # `cauldron` — the agent-facing CLI over the project registry
//!
//! `0071` §13 names this binary: "an agent-facing `cauldron register` CLI".
//! The crate was written expecting it — [`projects::Registry::register`]'s
//! `None` name arm says so in as many words ("what an agent-facing
//! `cauldron register <path>` wants"), and [`projects::Snapshot`] exists
//! *because* `~/.cauldron/projects.json` has writers the AUI process does not
//! contain. This is that writer.
//!
//! ## WHY A SECOND PROCESS MAY WRITE AT ALL
//!
//! `ui-server` is the registry's only writer **inside the AUI**
//! (`apps/aui/src/dispatch/mod.rs:226`, "THE ONLY WAY BYTES REACH
//! `~/.cauldron/projects.json` FROM THIS PROCESS"). That is a per-process
//! discipline, not a machine-wide one, and the machine-wide question has two
//! halves that are answered separately:
//!
//! 1. **On disk.** [`projects::Registry::update`] is lock → load → apply →
//!    write-temp → fsync → rename → unlock. It is proven across REAL PROCESSES
//!    at N >= 2 by `tests/concurrency.rs` and by F4/F5 in `force_failures.py`.
//!    A second writer cannot lose an update and cannot tear the document.
//! 2. **In his running face.** `apps/aui`'s `Config::registry` is a
//!    [`projects::Snapshot`], every `get()` is a `stat`, and
//!    `state::refresh_projects_if_changed` is called on the way into the
//!    project root (`apps/aui/src/v2/store.rs:147`). So a registration made
//!    here is his project "the moment the file says so, not the moment the AUI
//!    next restarts" — that comment is about this binary.
//!
//! So this CLI writes DIRECTLY and speaks to no socket.
//!
//! ## THE ONE THING IT WILL NOT DO
//!
//! It will not MINT the registry. Minting is `ui-server`'s act — `main.rs:157`
//! calls `bootstrap_if_absent(home, "ui-server")`, once, ever, and that is the
//! only code path that reads the `~/minds` relic. `Registry::update` on an
//! absent file starts from an EMPTY registry, which is correct on a machine
//! that has nothing to lose and catastrophic on one that has not booted the
//! AUI yet but still has 32 symlinks in `~/minds`: the first
//! `cauldron register` would create a one-entry registry, and the boot
//! bootstrap — whose trigger is the file's ABSENCE — would then never run.
//! Thirty-two projects would be replaced by one, silently, and the only
//! surviving evidence would be `~/minds` itself.
//!
//! [`refuse_if_unminted`] is that guard. It uses the crate's OWN derivation to
//! count what is at stake, so there is no second `~/minds` predicate anywhere.
//! When nothing would be lost it stands aside and the write proceeds exactly as
//! the crate documents.
//!
//! ## Exit status
//!
//! `0` did it · `1` refused, or could not read/write · `2` I did not understand
//! the command line. Refusals and usage go to stderr; everything a caller would
//! parse goes to stdout.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use projects::{home_dir, minds_root, registry_path, Registry, Row, Source};

/// The root every path in this process derives from.
///
/// `$CAULDRON_HOME` overrides `$HOME` — this is the crate's "THE ROOT IS A
/// PARAMETER" discipline reaching the one place that is allowed to look a root
/// up. It is what lets `tests/cli.rs` drive the real binary against a
/// `tempfile` root instead of against his registry.
const HOME_ENV: &str = "CAULDRON_HOME";

const USAGE: &str = "\
projects — the project registry at ~/.cauldron/projects.json

    projects list [--json]
    projects register <path> [--name <name>]
    projects remove <name>

  list                 print the registry as the faces draw it: the registered
                       projects in registry order, then the `miscellaneous`
                       bucket, always last. `--json` for a machine.
  register <path>      register an existing directory. The name defaults to the
                       basename of the canonicalized path, VERBATIM — `type_3`
                       stays `type_3`.
  remove <name>        de-register. Removes the ENTRY and nothing else: not the
                       directory, not its `.mind/`, not its threads.

  $CAULDRON_HOME       use this instead of $HOME as the root. For tests.

  exit 0 did it · 1 refused · 2 bad command line";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match parse(&argv) {
        Ok(c) => c,
        Err(why) => {
            eprintln!("{why}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let home = match resolve_home() {
        Ok(h) => h,
        Err(why) => {
            eprintln!("{why}");
            return ExitCode::FAILURE;
        }
    };

    let done = match cmd {
        Cmd::Help => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Cmd::List { json } => list(&home, json),
        Cmd::Register { path, name } => register(&home, &path, name.as_deref()),
        Cmd::Remove { name } => remove(&home, &name),
    };

    match done {
        Ok(said) => {
            if !said.is_empty() {
                println!("{said}");
            }
            ExitCode::SUCCESS
        }
        Err(sentence) => {
            eprintln!("{sentence}");
            ExitCode::FAILURE
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  The command line
// ──────────────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum Cmd {
    List { json: bool },
    Register { path: PathBuf, name: Option<String> },
    Remove { name: String },
    Help,
}

/// Hand-rolled, deliberately.
///
/// The workspace has no argument-parsing crate in it and this crate's whole
/// dependency list is six lines long for a reason. Three verbs, two flags and
/// one positional do not justify a new external dependency in a tree other
/// agents are compiling.
fn parse(argv: &[String]) -> Result<Cmd, String> {
    let Some(verb) = argv.first() else {
        return Err("I need a command.".to_string());
    };
    let rest = &argv[1..];
    match verb.as_str() {
        "-h" | "--help" | "help" => Ok(Cmd::Help),
        "list" => {
            let mut json = false;
            for a in rest {
                match a.as_str() {
                    "--json" => json = true,
                    other => return Err(format!("`list` does not take `{other}`.")),
                }
            }
            Ok(Cmd::List { json })
        }
        "register" => {
            let mut path: Option<PathBuf> = None;
            let mut name: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--name" => {
                        let Some(v) = rest.get(i + 1) else {
                            return Err("`--name` needs a name after it.".to_string());
                        };
                        if name.is_some() {
                            return Err("I was given `--name` twice.".to_string());
                        }
                        name = Some(v.clone());
                        i += 2;
                    }
                    other if other.starts_with("--") => {
                        return Err(format!("`register` does not take `{other}`."));
                    }
                    other => {
                        if path.is_some() {
                            return Err(format!(
                                "`register` takes one path; I was also given `{other}`."
                            ));
                        }
                        path = Some(PathBuf::from(other));
                        i += 1;
                    }
                }
            }
            let Some(path) = path else {
                return Err("`register` needs the path of the project to register.".to_string());
            };
            Ok(Cmd::Register { path, name })
        }
        "remove" => {
            if rest.len() != 1 {
                return Err("`remove` takes exactly one name.".to_string());
            }
            Ok(Cmd::Remove {
                name: rest[0].clone(),
            })
        }
        other => Err(format!("I don't have a command called `{other}`.")),
    }
}

/// **The one `$HOME` lookup a binary is allowed** (`cauldron`'s module note).
/// Everything below takes `home: &Path`.
fn resolve_home() -> Result<PathBuf, String> {
    if let Some(v) = std::env::var_os(HOME_ENV) {
        let p = PathBuf::from(v);
        if p.as_os_str().is_empty() {
            return Err(format!("`${HOME_ENV}` is set to nothing."));
        }
        return Ok(p);
    }
    home_dir().ok_or_else(|| "I could not find your home directory.".to_string())
}

// ──────────────────────────────────────────────────────────────────────────
//  list
// ──────────────────────────────────────────────────────────────────────────

/// `load_or_derive`, so that on a machine the AUI has not booted this prints
/// byte-for-byte the list the faces would draw — the coexistence window
/// (`0071` §10). It never writes, and it never mints.
fn list(home: &Path, json: bool) -> Result<String, String> {
    let (reg, source) = Registry::load_or_derive(home).map_err(|e| e.sentence())?;
    let rows = reg.view(home);
    if json {
        Ok(as_json(&reg, &rows, source, home))
    } else {
        Ok(as_table(&reg, &rows, source, home))
    }
}

fn as_table(reg: &Registry, rows: &[Row], source: Source, home: &Path) -> String {
    let header = match source {
        Source::File => format!(
            "{} project{} — {}",
            reg.len(),
            if reg.len() == 1 { "" } else { "s" },
            registry_path(home).display()
        ),
        // The TUI draws this difference in its header for the same reason.
        Source::DerivedFromMinds => format!(
            "registry not created yet — {} project{} derived from {}",
            reg.len(),
            if reg.len() == 1 { "" } else { "s" },
            minds_root(home).display()
        ),
    };
    let width = rows.iter().map(|r| r.name.chars().count()).max().unwrap_or(0);
    let mut out = String::from(&header);
    for r in rows {
        let note = if r.bucket {
            "  (bucket)"
        } else if r.missing {
            "  (missing)"
        } else {
            ""
        };
        out.push_str(&format!(
            "\n  {:width$}  {}{}",
            r.name,
            r.path.display(),
            note,
            width = width
        ));
    }
    out
}

fn as_json(reg: &Registry, rows: &[Row], source: Source, home: &Path) -> String {
    let projects: Vec<serde_json::Value> = rows
        .iter()
        .filter(|r| !r.bucket)
        .map(|r| {
            // `registered_at` is on the ENTRY, not the row; the row is the
            // face's view. A row that is not the bucket always has an entry.
            let at = reg.get(&r.name).map(|e| e.registered_at.to_rfc3339());
            serde_json::json!({
                "name": r.name,
                "path": r.path,
                "registered_at": at,
                "missing": r.missing,
            })
        })
        .collect();
    let bucket = rows.iter().find(|r| r.bucket).map(|r| {
        serde_json::json!({ "name": r.name, "path": r.path })
    });
    let doc = serde_json::json!({
        "source": match source {
            Source::File => "file",
            Source::DerivedFromMinds => "derived-from-minds",
        },
        "registry": registry_path(home),
        "count": reg.len(),
        "projects": projects,
        "bucket": bucket,
    });
    serde_json::to_string_pretty(&doc).expect("a plain data document")
}

// ──────────────────────────────────────────────────────────────────────────
//  register / remove
// ──────────────────────────────────────────────────────────────────────────

/// `None` for the name means "the basename, verbatim" — the crate's own
/// contract, and the reason this binary does not normalize: the ONE normalizer
/// is `libs/aui-state/src/server/mod.rs:377` and a second copy here would let
/// the name he hears confirmed differ from the name on disk (D21a).
fn register(home: &Path, path: &Path, name: Option<&str>) -> Result<String, String> {
    refuse_if_unminted(home)?;
    let (name, at) = Registry::update(home, |r| {
        r.register(path, name).map(|e| (e.name.clone(), e.path.clone()))
    })
    .map_err(|e| e.sentence())?;
    Ok(format!("Registered `{name}` at `{}`.", at.display()))
}

/// Removes the ENTRY. `projects::Registry::remove` has no filesystem in it, so
/// this cannot do otherwise even by accident.
fn remove(home: &Path, name: &str) -> Result<String, String> {
    refuse_if_unminted(home)?;
    let entry = Registry::update(home, |r| r.remove(name)).map_err(|e| e.sentence())?;
    Ok(format!(
        "Removed `{}`. `{}` is untouched.",
        entry.name,
        entry.path.display()
    ))
}

/// **THE GUARD: this binary is not the minter.** See the module note.
///
/// Absent registry + a populated `~/minds` is the one state where a write here
/// is destructive, because `bootstrap_if_absent`'s trigger is the file's
/// absence and this write would remove that trigger. The count comes from
/// `Registry::derive_from_minds`, which is the crate's ONE `~/minds` predicate
/// and writes nothing — a second `read_dir` here would be the two-derivations
/// failure this crate exists to close.
///
/// Absent registry + nothing in `~/minds` is the case the crate blesses
/// (`Registry::update`'s doc: "the agent-facing `cauldron register` on a
/// machine the AUI has not booted yet, and an empty document with his one
/// project in it is correct"), and this returns `Ok` for it.
fn refuse_if_unminted(home: &Path) -> Result<(), String> {
    if registry_path(home).exists() {
        return Ok(());
    }
    let at_stake = Registry::derive_from_minds(home, "cauldron-cli-guard").len();
    if at_stake == 0 {
        return Ok(());
    }
    let them = if at_stake == 1 {
        "that project".to_string()
    } else {
        format!("those {at_stake} projects")
    };
    Err(format!(
        "Your project registry isn't there yet, and `{}` still holds {at_stake} project{}. \
         The registry is minted from it once, at the AUI's startup, and the trigger is the \
         file being absent — so writing it now would leave {them} out of your list for good. \
         Start the AUI once, then run this again.",
        minds_root(home).display(),
        if at_stake == 1 { "" } else { "s" },
    ))
}

#[cfg(test)]
mod parse_tests {
    //! The command line only. Everything that touches a filesystem is in
    //! `tests/cli.rs`, which drives the real binary against a scratch root.
    use super::*;

    #[test]
    fn the_three_verbs_and_their_flags() {
        let p = |s: &str| parse(&s.split(' ').map(str::to_string).collect::<Vec<_>>());
        assert_eq!(p("list"), Ok(Cmd::List { json: false }));
        assert_eq!(p("list --json"), Ok(Cmd::List { json: true }));
        assert_eq!(
            p("register /a/b"),
            Ok(Cmd::Register {
                path: PathBuf::from("/a/b"),
                name: None
            })
        );
        assert_eq!(
            p("register /a/b --name q"),
            Ok(Cmd::Register {
                path: PathBuf::from("/a/b"),
                name: Some("q".to_string())
            })
        );
        assert_eq!(
            p("remove q"),
            Ok(Cmd::Remove {
                name: "q".to_string()
            })
        );
        assert_eq!(p("--help"), Ok(Cmd::Help));
    }

    #[test]
    fn what_it_will_not_accept() {
        let p = |s: &str| parse(&s.split(' ').map(str::to_string).collect::<Vec<_>>());
        assert!(p("register").is_err(), "a path is not optional");
        assert!(p("register /a --name").is_err(), "--name needs a value");
        assert!(p("register /a /b").is_err(), "one path, not two");
        assert!(p("register /a --wat").is_err());
        assert!(p("remove").is_err());
        assert!(p("remove a b").is_err());
        assert!(p("list --wat").is_err());
        assert!(p("frobnicate").is_err());
        assert!(parse(&[]).is_err());
    }
}
