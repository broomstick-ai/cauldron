//! The `threads` binary.
//!
//! - `threads` — run the daemon (config: argv `--config <path>` →
//!   `$THREADS_CONFIG` → `~/.threads/config.toml`).
//! - `threads import <path>` — full import of AUI thread stores (Q8). `<path>`
//!   is a project root (holding `.mind/threads.json`) or a minds root whose
//!   children are project roots. Idempotent.
//! - `threads reconcile <path>` — the post-cutover delta pass: identical to
//!   `import` (the watermark makes import itself incremental), named
//!   separately so operational runbooks read honestly.
//!
//! # THE PROMOTION PATH — how a change here actually goes live
//!
//! **`cargo build` is NOT a deploy for this daemon.** It leaves an ad-hoc
//! signature, the watcher ignores it, and it says so only at `debug!` while the
//! daemon runs at INFO — so a change that never went live looks exactly like one
//! that did. Five days of committed work sat unabsorbed that way.
//!
//! **The promotion path is `core/tools/promote` in the harness core plugin:**
//!
//! ```text
//! core-plugins/core/tools/promote publish threads   # build, sign, verify, publish, clean up
//! core-plugins/core/tools/promote status  threads   # is the live process running this image?
//! ```
//!
//! **Read the skill `core:signed-service-deploy` before you use it** — it covers
//! why the signature is the deploy signal, what the absorb path checks, how the
//! supervisor and the quiet-moment restart interact, and how to tell whether a
//! running service is executing a given commit. Never restart the daemon to make
//! a deploy land; the quiet-moment path decides.
//!
//! (`scripts/build-threads.sh` is the older threads-only form of the same cycle
//! and still works. `promote` is the general one and is the path to use.)

use threads::{accounts, config, import, Budgets, Config, Store};

fn usage() -> ! {
    eprintln!(
        "usage:\n  threads [--config <path>]          run the daemon\n  threads --self-check                       preflight: prove this binary loads and is this daemon (reads/writes nothing)\n  threads --build-stamp                      WHAT COMMIT IS THIS FILE BUILT FROM: the commit compiled into this image, whether it was PINNED by whoever shipped it or sniffed from git at build time, and whether the tree was dirty. Reads/writes nothing; `promote` reads it back off the product it signed\n  threads import <path> [--config <path>]    full import of AUI thread stores\n  threads reconcile <path> [--config <path>]  delta reconciliation (same as import)\n  threads image [--json] [--config <path>]   WHICH BINARY IS ACTUALLY RUNNING: the running image's cdhash, signing identity and whether it is signed or ad-hoc, its text inode vs the inode at the launch path (a divergence means the file was replaced under the running process), its build, and whether the on-disk image would be absorbed. Asks the daemon; exit 3 = diverged, exit 4 = daemon unreachable (prints the on-disk half)\n  threads cutover <thread_id> [--config <path>]  cut a thread over to the projected ledger (offline; refuses until a raw per-account copy is staged; exit 2 = refused)\n  threads accounts add <name> --clone-session [--config <path>]    SNAPSHOT the current login into <name> by CLONING its session (shared refresh token; refused without the flag)\n  threads accounts adopt <name> [--config <path>]  register an already-captured <data_dir>/accounts/<name>/ (no re-capture)\n  threads accounts list [--config <path>]          list registered accounts\n  threads accounts show <name> [--config <path>]   print an account's restore command\n  threads accounts status [--json] [--config <path>]  per-account activity: executing / parked / queued threads\n  threads accounts health [--json] [--config <path>]  per-account LOGIN health: live / clone / expired, days left, rotation plan\n  threads accounts rename <old> <new> [--config <path>]  rename a slot: dir, keychain item, manifest and registry together\n  threads accounts delete <name> [--force] [--config <path>]  remove a slot, its keychain item and its registry entry (local only; revokes nothing)\n  threads accounts recheck <name> [--json] [--config <path>]  ask the daemon to re-check every flag on <name> now (trusted identity read + slot probe); exit 4 = daemon unreachable\n\n  (there is no `logout`, deliberately — see `threads accounts logout`)\n  For the interactive account manager, run `scripts/claude-accounts`."
    );
    std::process::exit(2);
}

/// Consume a valueless boolean flag anywhere in `args`.
fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    match args.iter().position(|a| a == flag) {
        Some(i) => {
            args.remove(i);
            true
        }
        None => false,
    }
}

fn parse_flag(args: &mut Vec<String>, flag: &str) -> Option<String> {
    if let Some(i) = args.iter().position(|a| a == flag) {
        if i + 1 < args.len() {
            let v = args.remove(i + 1);
            args.remove(i);
            return Some(v);
        }
        usage();
    }
    None
}

/// `threads --self-check` — the PREFLIGHT VERB (intent item 64).
///
/// Answered before logging is initialised, before any config is resolved and
/// before any file is opened, because it is run against a CANDIDATE binary that
/// is about to become the live daemon: it must prove the image loads and is this
/// program, and it must not be able to touch the operator's data on the way. It
/// reads nothing, writes nothing, and creates nothing.
///
/// `update::self_check` requires exit 0 and [`threads::update::SELF_CHECK_OK`]
/// on stdout; anything else means DO NOT restart into this file.
fn self_check() -> ! {
    println!(
        "{} version {} proto {}",
        threads::update::SELF_CHECK_OK,
        env!("CARGO_PKG_VERSION"),
        threads::protocol::PROTO,
    );
    std::process::exit(0);
}

/// `threads --build-stamp` — WHAT COMMIT IS THIS FILE BUILT FROM?
///
/// Answered beside `--self-check`, for the same reason and under the same
/// constraints: it is run against a file on disk that may be about to become the
/// live daemon, so it resolves no config, opens nothing and writes nothing.
///
/// This exists because `promote` cannot establish the answer from outside. It
/// runs `cargo build` and signs the product, but cargo relinks nothing when its
/// fingerprint is already satisfied — so the bytes it signs may have been built
/// from a commit nobody promoted, and an exit code of 0 from the compiler says
/// otherwise. `promote` reads this line back off the product after signing it and
/// refuses to publish unless it names the commit it was asked to ship.
fn build_stamp() -> ! {
    println!("{}", threads::image::build_stamp_line());
    std::process::exit(0);
}

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == threads::update::SELF_CHECK_FLAG) {
        self_check();
    }
    if args.iter().any(|a| a == "--build-stamp") {
        build_stamp();
    }
    tracing_subscriber_init();
    let config_path = parse_flag(&mut args, "--config");
    let as_json = take_flag(&mut args, "--json");
    // The ONLY producer of `CloneConsent::CloneLiveSession` in this binary.
    // Without it every credential-copying capture path refuses (F-1).
    let clone_consent = if take_flag(&mut args, "--clone-session") {
        accounts::CloneConsent::CloneLiveSession
    } else {
        accounts::CloneConsent::Refuse
    };
    let config = load_config(config_path.as_deref());

    match args.first().map(String::as_str) {
        None => {
            if let Err(e) = threads::run_daemon(config).await {
                eprintln!("threads daemon: {e}");
                std::process::exit(1);
            }
        }
        Some("import") | Some("reconcile") => {
            let Some(path) = args.get(1) else { usage() };
            let path = std::path::PathBuf::from(path);
            let data_dir = match config.resolved_data_dir() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("threads: {e}");
                    std::process::exit(1);
                }
            };
            let store = match threads::Store::open(data_dir.join("threads.db")) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("threads: opening store: {e}");
                    std::process::exit(1);
                }
            };
            // The importer needs the SAME account registry the daemon runs on:
            // a thread is placed on the account whose store actually holds its
            // conversation, and that can only be answered by looking in every
            // registered config dir.
            // NO `ensure_default()` HERE either. It used to hoist the machine
            // default to the front of the transcript search AND make it
            // `ImportContext::default_account` — the account a thread whose
            // conversation could not be located is placed on. So every import
            // wrote NEW rows naming `default` into `threads.account`. Without
            // it the context falls to the first real account, which is the
            // honest answer and the only one placement can act on.
            //
            // Existing history is NOT rewritten and is not this commit's to
            // rewrite; this only stops more of it being created.
            let registry = match accounts::Registry::load(&data_dir.join("accounts.toml")) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("threads: reading the account registry: {e}");
                    std::process::exit(1);
                }
            };
            let ctx = import::ImportContext::from_registry(&registry);
            // A project root imports directly; anything else is treated as a
            // minds root of project roots.
            let result = if path.join(".mind").join("threads.json").exists() {
                import::import_project(&store, &path, &ctx)
            } else {
                import::import_minds_root(&store, &path, &ctx)
            };
            match result {
                Ok(report) => {
                    println!("{report}");
                    for line in &report.skipped_unsafe {
                        println!("NOT IMPORTED  {line}");
                    }
                }
                Err(e) => {
                    eprintln!("threads import: {e}");
                    std::process::exit(1);
                }
            }
        }
        // **WHICH BINARY IS ACTUALLY RUNNING** — the CLI form of `daemon.image`.
        //
        // The CLI is what somebody reaches for when the daemon is suspect, so
        // this verb exists precisely for the moment the WS surface is the thing
        // in doubt.
        //
        // IT ASKS THE DAEMON. It does not compute the answer locally, and that
        // is not an implementation convenience — a CLI that inspected its OWN
        // process would be reporting on the CLI, confidently, under a heading
        // that says "the running image". That is the exact failure this whole
        // endpoint replaces, rebuilt in a new place.
        //
        // When the daemon cannot be reached it says so and prints ONLY the
        // disk half, every running-image field marked undeterminable. A
        // process that is not answering has no observable running image, and
        // the honest report of that is not an empty one — the on-disk signing
        // state is usually what explains a stuck deploy.
        Some("image") | Some("binary") => {
            image_verb(&config, as_json).await;
        }
        // **THE CUTOVER VERB'S CALLER** (R4.S4b, HOLDS R17). Offline: it reads
        // the store for the thread's account and asks the verb, which refuses
        // until a raw per-account copy is staged (H115) and, past that, names
        // the part of the act that is not built. Every refusal exits 2.
        Some("cutover") => {
            let Some(thread_id) = args.get(1) else { usage() };
            let data_dir = match config.resolved_data_dir() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("threads: {e}");
                    std::process::exit(1);
                }
            };
            // **CUTOVER NEVER CREATES OR MIGRATES A STORE** (R4.S4b.fix, HOLDS
            // R25). It only reads the thread's account, so a missing store is
            // a wrong data dir — refused here, before anything opens it — and
            // an existing one is opened read-only with no schema pass.
            let db = data_dir.join("threads.db");
            if !db.is_file() {
                eprintln!(
                    "threads cutover: no store at {} — refusing to create one (wrong data dir?)",
                    db.display()
                );
                std::process::exit(2);
            }
            let store = match threads::Store::open_read_only(&db) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("threads: opening store read-only: {e}");
                    std::process::exit(1);
                }
            };
            match threads::ledger::cutover::cutover_thread(&store, &data_dir, thread_id) {
                Ok(never) => match never {},
                Err(refused) => {
                    eprintln!("threads cutover: {refused}");
                    std::process::exit(2);
                }
            }
        }
        Some("accounts") => {
            let data_dir = match config.resolved_data_dir() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("threads: {e}");
                    std::process::exit(1);
                }
            };
            match args.get(1).map(String::as_str) {
                Some("add") => {
                    let Some(name) = args.get(2) else { usage() };
                    accounts_add(&data_dir, name, clone_consent);
                }
                Some("adopt") => {
                    let Some(name) = args.get(2) else { usage() };
                    accounts_adopt(&data_dir, name);
                }
                Some("list") => accounts_list(&data_dir),
                Some("show") => {
                    let Some(name) = args.get(2) else { usage() };
                    accounts_show(&data_dir, name);
                }
                Some("status") => accounts_status(&data_dir, as_json),
                Some("health") => accounts_health(&data_dir, as_json),
                Some("pool-share") | Some("share") => {
                    accounts_pool_share(
                        &config,
                        &data_dir,
                        args.get(2),
                        args.get(3),
                        &args,
                        as_json,
                    )
                    .await
                }
                Some("pressure") => accounts_pressure(&data_dir, as_json),
                Some("recheck") => {
                    let Some(name) = args.get(2) else { usage() };
                    accounts_recheck(&config, name, as_json).await
                }
                Some("rename") => {
                    let (Some(old), Some(new)) = (args.get(2), args.get(3)) else { usage() };
                    accounts_rename(&data_dir, old, new);
                }
                Some("delete") | Some("remove") | Some("rm") => {
                    let Some(name) = args.get(2) else { usage() };
                    let force = args.iter().any(|a| a == "--force");
                    accounts_delete(&data_dir, name, force);
                }
                // `logout` is not a missing feature — it is a refused one.
                // `claude auth logout` asks the server to REVOKE the refresh
                // token (CONFIRMED-CODE, v2.1.220: `performLogout` POSTs to
                // `/oauth/revoke` when `preserveInProcessTokens` is false), and
                // on a cloned credential that revokes the operator's LIVE
                // session, irreversibly. Nothing in this repo may offer the
                // verb, so the verb answers for itself.
                Some("logout") | Some("signout") | Some("sign-out") => {
                    eprintln!(
                        "threads accounts logout: REFUSED — this tool does not log accounts out, \
                         ever.\n\n  `claude auth logout` asks the server to REVOKE the refresh \
                         token. Because\n  several of these slots share one credential with your \
                         live login, a logout in\n  the wrong place kills the session you are \
                         reading this in, and it cannot be\n  undone.\n\n  You never need it: \
                         each account has its own CLAUDE_CONFIG_DIR and its own\n  keychain entry, \
                         so they are all signed in at once.\n\n  To retire a slot, drop its \
                         registry entry and delete its keychain item —\n  `threads accounts \
                         health` prints the exact `security delete-generic-password`\n  command \
                         for each slot. That is local-only and revokes nothing."
                    );
                    std::process::exit(2);
                }
                _ => usage(),
            }
        }
        Some(_) => usage(),
    }
}

/// `threads accounts add <name> --clone-session` — snapshot the currently
/// authenticated claude config into `<data_dir>/accounts/<name>/` and register
/// it.
///
/// THIS IS THE CLONE VERB. It copies the live login's stored session, which is
/// not a second account; without `--clone-session` the library refuses before
/// it creates or reads anything (see [`accounts::CLONE_REFUSAL`]).
///
/// SECURITY: prints paths, key names, and keychain SERVICE names only —
/// never credential values.
fn accounts_add(data_dir: &std::path::Path, name: &str, consent: accounts::CloneConsent) {
    let source = match accounts::CaptureSource::from_env() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("threads accounts add: {e}");
            std::process::exit(1);
        }
    };
    let outcome = match accounts::capture_snapshot(
        &source,
        data_dir,
        name,
        accounts::KeychainMode::Auto,
        consent,
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("threads accounts add: {e}");
            std::process::exit(1);
        }
    };

    // Register in accounts.toml so the daemon/spawner can select it.
    let registry_path = data_dir.join("accounts.toml");
    let mut registry = match accounts::Registry::load(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("threads accounts add: {e}");
            std::process::exit(1);
        }
    };
    // NO `ensure_default()` HERE. This path SAVES, and a save that carried the
    // machine default would write `[accounts.default]` back into
    // `accounts.toml` — undoing the state migration on the next capture.
    // `Registry::save` filters it regardless; the call is removed as well so
    // the call site does not read as if it intended to persist it.
    let dir_str = outcome.dir.to_string_lossy().to_string();
    registry.add(name, Some(dir_str.clone()));
    if let Err(e) = registry.save(&registry_path) {
        eprintln!("threads accounts add: saving registry: {e}");
        std::process::exit(1);
    }

    println!("account {name:?} captured → {}", outcome.dir.display());
    println!("  copied: {}", outcome.copied.join(", "));
    match &outcome.credentials {
        accounts::CredentialOutcome::PlaintextCopied => {
            println!("  credentials: .credentials.json COPIED from the live login (plaintext mode)");
            println!("  SHARED SESSION — this slot and the login it was copied from now hold");
            println!("  the SAME refresh token. Claude rotates that token on use, and whichever");
            println!("  side refreshes second is stranded and has to log in again. This is a");
            println!("  snapshot, not a second account.");
            println!("  spawns with CLAUDE_CONFIG_DIR={dir_str}");
        }
        accounts::CredentialOutcome::KeychainCloned { service } => {
            println!("  credentials: keychain entry CLONED from the live login → service {service:?}");
            println!("  SHARED SESSION — this slot and the login it was cloned from now hold");
            println!("  the SAME refresh token. Claude rotates that token on use, and whichever");
            println!("  side refreshes second is stranded and has to log in again. This is a");
            println!("  snapshot, not a second account.");
            println!("  spawns with CLAUDE_CONFIG_DIR={dir_str}");
        }
        accounts::CredentialOutcome::ManualLoginRequired { service, reason } => {
            println!("  credentials: NOT copied ({reason})");
            println!("  next step: run  CLAUDE_CONFIG_DIR={dir_str} claude");
            println!("  then /login once; tokens will persist under keychain service {service:?}");
        }
    }
    println!("  registered in {}", registry_path.display());
}

/// `threads accounts list` — **THE POOL** + snapshot status. No secrets.
///
/// The pool, not the slot registry (2026-08-27). This used to print one line
/// per `accounts.toml` row, machine default included (`default: machine default
/// (CLAUDE_CONFIG_DIR unset)`). It prints pool entries now, so that line is not
/// produced rather than filtered. The slot view — the one where he chooses which
/// login sits in the machine-default slot — is `threads accounts health`, which
/// is what `claude-accounts` reads and which keeps it.
fn accounts_list(data_dir: &std::path::Path) {
    let pool = match load_pool(data_dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("threads accounts list: {e}");
            std::process::exit(1);
        }
    };
    for entry in &pool.entries {
        let name = &entry.name;
        let kind = entry.resource.type_name();
        let share = entry
            .share
            .map(|s| format!("{:.0}% of the pool", s * 100.0))
            .unwrap_or_else(|| "100% of the pool (no share set)".to_string());
        match entry.login_dir() {
            Some(dir) => {
                let manifest = std::path::Path::new(dir).join("manifest.toml");
                let status = match std::fs::read_to_string(&manifest)
                    .ok()
                    .and_then(|s| toml::from_str::<accounts::SnapshotManifest>(&s).ok())
                {
                    Some(m) => format!("captured {} ({})", m.captured_at, m.credential_mode),
                    None => "no snapshot manifest".to_string(),
                };
                println!("{name} [{kind}] — {share}");
                println!("  login stored at CLAUDE_CONFIG_DIR={dir} — {status}");
            }
            // Unreachable for `claude_account`, which cannot exist without a
            // login dir. It is the shape a SECOND resource type takes when one
            // is added, and it prints rather than panicking.
            None => println!("{name} [{kind}] — {share}"),
        }
    }
    if pool.entries.is_empty() {
        println!("the pool is empty — no account has been given to it");
    }
}

/// Load the pool the daemon would load: `pool.json`, else derived from the
/// account registry. One function so a CLI view and the daemon can never
/// disagree about what the pool is.
fn load_pool(data_dir: &std::path::Path) -> threads::Result<threads::pool::Pool> {
    let registry = accounts::Registry::load(&data_dir.join("accounts.toml"))?;
    threads::pool::Pool::load(&data_dir.join("pool.json"), &registry)
}

/// `threads accounts adopt <name>` — register an ALREADY-captured snapshot
/// at `<data_dir>/accounts/<name>/` into the registry, without capturing
/// anything new (no filesystem copy, no keychain read/write). Exists so a
/// snapshot produced any other way (e.g. migrated from a prior capture tool)
/// can join the registry exactly once, instead of being re-captured.
///
/// SECURITY: reads only `manifest.toml` (non-secret) to confirm the snapshot
/// is real; never touches `.claude.json`, `.credentials.json`, or the
/// keychain.
fn accounts_adopt(data_dir: &std::path::Path, name: &str) {
    if let Err(e) = accounts::validate_account_name(name) {
        eprintln!("threads accounts adopt: {e}");
        std::process::exit(1);
    }

    let dir = data_dir.join("accounts").join(name);
    let manifest_path = dir.join("manifest.toml");
    let manifest: accounts::SnapshotManifest = match std::fs::read_to_string(&manifest_path) {
        Ok(s) => match toml::from_str(&s) {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "threads accounts adopt: {} does not look like a snapshot manifest: {e}",
                    manifest_path.display()
                );
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!(
                "threads accounts adopt: no snapshot at {} ({e}) — capture it first \
                 (`threads accounts add {name}`) or place one at that path",
                dir.display()
            );
            std::process::exit(1);
        }
    };

    let registry_path = data_dir.join("accounts.toml");
    let mut registry = match accounts::Registry::load(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("threads accounts adopt: {e}");
            std::process::exit(1);
        }
    };
    // NO `ensure_default()` HERE, and this is the call site that mattered most:
    // `claude-accounts login` runs `accounts adopt`, so this was the path that
    // would have restored `[accounts.default]` the first time the operator
    // logged an account in after the state migration.
    registry.add(name, Some(dir.to_string_lossy().to_string()));
    if let Err(e) = registry.save(&registry_path) {
        eprintln!("threads accounts adopt: saving registry: {e}");
        std::process::exit(1);
    }

    println!(
        "account {name:?} adopted → {} (captured {}, {})",
        dir.display(),
        manifest.captured_at,
        manifest.credential_mode
    );
    println!("  registered in {}", registry_path.display());
}

/// `threads accounts show <name>` — print the restore command and any
/// notes for a registered account. Read-only: never invokes `claude`, never
/// touches the keychain, never writes anywhere.
fn accounts_show(data_dir: &std::path::Path, name: &str) {
    let registry_path = data_dir.join("accounts.toml");
    let registry = match accounts::Registry::load(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("threads accounts show: {e}");
            std::process::exit(1);
        }
    };
    // Read-only, and it no longer invents the row: `threads accounts show
    // default` now answers "no such account", which is the honest answer. The
    // machine login is `claude-accounts`'s to describe, not this binary's.
    let Some(acct) = registry.get(name) else {
        eprintln!("threads accounts show: no account {name:?} in {}", registry_path.display());
        std::process::exit(1);
    };
    let Some(dir) = &acct.config_dir else {
        println!("account {name:?}: machine default (CLAUDE_CONFIG_DIR unset) — restore: claude");
        return;
    };

    let manifest_path = std::path::Path::new(dir).join("manifest.toml");
    let manifest: accounts::SnapshotManifest = match std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
    {
        Some(m) => m,
        None => {
            eprintln!(
                "threads accounts show: no manifest at {} — has `threads accounts add/adopt {name}` been run?",
                manifest_path.display()
            );
            std::process::exit(1);
        }
    };

    println!("account {name:?} — captured {}", manifest.captured_at);
    println!("  snapshot dir: {dir}");
    println!("  credential mode: {}", manifest.credential_mode);
    println!();
    match manifest.credential_mode.as_str() {
        "keychain-cloned" | "plaintext-file" => {
            // NOT "independent". This slot's credential was COPIED from another
            // login, so the two share one refresh token and a rotation on
            // either side strands the other (F-1: the old wording here claimed
            // the opposite of what was measured).
            println!("SHARED SESSION — this slot's token is a COPY of another login's.");
            println!("Both hold the same refresh token; when either side refreshes it, the");
            println!("other is stranded and must log in again. Use it as a snapshot, not as");
            println!("a second account (for that: `scripts/add-account --login <name>`).");
            println!();
            println!("RESTORE:");
            println!();
            println!("    CLAUDE_CONFIG_DIR={dir} claude");
            println!();
            if manifest.credential_mode == "keychain-cloned" {
                println!(
                    "  Token lives in the login keychain under service {:?},",
                    manifest.keychain_service
                );
                println!("  not inside {dir}. Deleting the folder does NOT delete the token");
                println!(
                    "  (use `security delete-generic-password -s {:?}` to do that).",
                    manifest.keychain_service
                );
            } else {
                println!("  Token is the plaintext .credentials.json inside {dir}.");
            }
        }
        "manual-login-required" => {
            println!(
                "NOT restorable yet — credentials were not captured ({}).",
                manifest.note.as_deref().unwrap_or("reason unknown")
            );
            println!("One-time operator action required:");
            println!();
            println!("    CLAUDE_CONFIG_DIR={dir} claude");
            println!();
            println!("  then run /login once inside that session. Tokens will then persist");
            println!(
                "  under keychain service {:?}, independent of every other captured account.",
                manifest.keychain_service
            );
        }
        other => {
            eprintln!("threads accounts show: unrecognized credential_mode {other:?} in manifest");
            std::process::exit(1);
        }
    }
}

/// `threads accounts status [--json]` — the per-account ACTIVITY surface
/// (operator ask, msg #690: see how many threads are active on each account so
/// an agent can test without getting in the way of the operator's live use).
///
/// Reads the DURABLE store only: no daemon connection, no `/usage` poll, no
/// `claude` invocation, no keychain access, no writes. Gauge readings are
/// whatever is already recorded (`usage_observations` / `limit_estimates`);
/// nothing here can cost a poll.
///
/// Emits EXACTLY the WS `accounts.status` payload under `--json` — same two
/// functions, same `limit`, same empty live-account map — and
/// `tests/ws_protocol.rs` asserts the two are byte-for-byte identical over the
/// same store, so neither surface can drift from the other.
///
/// One consequence of being a store reader rather than a daemon client remains,
/// and it is deliberate (the CLI must work from a terminal with no client at
/// all): a GATE park that has not been written to `parked_turns` shows up as
/// queued work rather than as a park.
///
/// **That gap is now the LAST one in the fleet, and it is worth naming
/// precisely.** `threads.status` and `threads.list` DO see a gate park, because
/// `ThreadManager::thread_states` reads the daemon's in-memory `park_reason`
/// latch alongside the durable rows. Both `accounts.status` surfaces — this one
/// and the WS one — cannot, because both are pure store readers and the latch
/// is process memory. They still agree on the question the operator actually
/// asked ("is this thread running": no, on every surface); they differ on
/// *why* it is not.
///
/// The root defect is that a GATE PARK IS NOT DURABLE. Fixing it means writing
/// the gate's refusal to `parked_turns` the way a killed turn's is (with
/// `prompt_parked: false`, since the prompt stays queued) — at which point
/// every reader sees it with no in-memory special case at all, and
/// `thread_states` can drop the latch read. Do NOT close the gap by handing a
/// manager to `collect_thread_activity`: that would make this CLI and the WS
/// arm answer differently, and `tests/ws_protocol.rs` holds them byte-for-byte
/// identical on purpose.
///
/// Account ATTRIBUTION is no longer such a case — since `switch_account` persists `threads.account` (IS-A) the durable
/// column mirrors the daemon's in-memory serving account, so a store reader
/// resolves it exactly as the daemon would.
fn accounts_status(data_dir: &std::path::Path, as_json: bool) {
    let db = data_dir.join("threads.db");
    if !db.exists() {
        // Do NOT create a store just to answer a status query.
        if as_json {
            println!(
                "{}",
                serde_json::json!({
                    "accounts": [], "totals": {
                        "executing": 0, "parked": 0, "queued_messages": 0,
                        "threads": 0, "busy": false,
                    },
                    "note": format!("no thread store at {}", db.display()),
                })
            );
        } else {
            println!("no thread store at {} — nothing has run here yet", db.display());
        }
        return;
    }

    let store = match Store::open(&db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("threads accounts status: opening {}: {e}", db.display());
            std::process::exit(1);
        }
    };
    let registry_path = data_dir.join("accounts.toml");
    let registry = match accounts::Registry::load(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("threads accounts status: {e}");
            std::process::exit(1);
        }
    };
    let pool = match threads::pool::Pool::load(&data_dir.join("pool.json"), &registry) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("threads accounts status: {e}");
            std::process::exit(1);
        }
    };

    // Already-known gauge readings only — `gauge_state` is a pure table read.
    let gauge = Budgets::new(store.clone())
        .gauge_state(&pool.names())
        .unwrap_or(serde_json::Value::Null);

    let rows = match accounts::collect_activity(
        &store,
        &pool,
        &std::collections::BTreeMap::new(),
        Some(&gauge),
        1000,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("threads accounts status: {e}");
            std::process::exit(1);
        }
    };

    if as_json {
        // Byte-identical to the `accounts.status` WS payload — one shape.
        println!("{}", accounts::activity_json(&rows));
        return;
    }

    println!(
        "{:<24} {:>7} {:>5} {:>9} {:>8} {:>5} {:>6}",
        "ACCOUNT", "THREADS", "EXEC", "QUEUED", "RETRYING", "IDLE", "AVAIL"
    );
    // No PARKED column: a parked turn belongs to the POOL, not to the account it
    // last sat on (H179). The pool's parked list is printed under the totals.
    for r in &rows {
        println!(
            "{:<24} {:>7} {:>5} {:>9} {:>8} {:>5} {:>6}",
            r.account,
            r.threads,
            r.executing,
            format!("{}/{}", r.queued_threads, r.queued_messages),
            r.retrying,
            r.idle,
            if r.available { "yes" } else { "NO" },
        );
    }

    let totals = accounts::activity_json(&rows);
    println!(
        "\ntotal: {} executing, {} parked, {} queued message(s) across {} thread(s) — {}",
        totals["totals"]["executing"],
        totals["totals"]["parked"],
        totals["totals"]["queued_messages"],
        totals["totals"]["threads"],
        if totals["totals"]["busy"] == serde_json::json!(true) {
            "SOMETHING IS RUNNING"
        } else {
            "the pool is quiet"
        }
    );

    // PARKED IS A POOL STATE (H179): a parked turn waits in the pool and is
    // re-placed onto whichever account admission picks, so it is listed once
    // here rather than under the account it last sat on (which is shown only as
    // attribution).
    if let Some(threads) = totals["pool_parked"]["threads"].as_array().filter(|t| !t.is_empty()) {
        println!("\nparked in the pool (re-placed by admission on the next placement event):");
        if let Some(reasons) = totals["pool_parked"]["reasons"].as_object() {
            for (reason, n) in reasons {
                println!("  x{n}: {reason}");
            }
        }
        for t in threads {
            println!(
                "  {} (last on {})",
                t["thread_id"].as_str().unwrap_or("?"),
                t["last_account"].as_str().unwrap_or("?")
            );
        }
    }

    // Detail only for accounts that have something to say.
    for r in &rows {
        let axes = r
            .gauge_axes
            .as_ref()
            .and_then(|g| g.as_object())
            .map(|m| {
                m.iter()
                    .map(|(axis, v)| match v["observed_pct"].as_f64() {
                        Some(p) => format!("{axis} {p:.1}%"),
                        None => format!("{axis} —"),
                    })
                    .collect::<Vec<_>>()
                    .join(" · ")
            })
            .filter(|s| !s.is_empty());
        let has_detail = r.busy()
            || !r.registered
            || r.limit_until.is_some()
            || axes.as_ref().map(|s| s.contains('%')).unwrap_or(false);
        if !has_detail {
            continue;
        }
        println!("\n{}", r.account);
        if !r.registered {
            println!("  NOT REGISTERED — threads point at it but `accounts.toml` does not list it");
        }
        if let Some(d) = &r.config_dir {
            println!("  config_dir: {d}");
        } else if r.registered {
            println!("  config_dir: (machine default — CLAUDE_CONFIG_DIR unset)");
        }
        if !r.executing_thread_ids.is_empty() {
            println!("  executing: {}", r.executing_thread_ids.join(", "));
        }
        if let Some(until) = &r.limit_until {
            // ── WHERE HE IS TOLD TO RE-AUTHENTICATE ──────────────────────────
            //
            // This is the surface that carries it, and it is deliberately this
            // one rather than a new channel. A `limit_until` is rendered as a
            // time window, which for a quota is the whole truth — wait, and it
            // comes back. For `limit_axis = "login"` a bare window is a lie in
            // the most expensive direction: it tells him to wait for something
            // that will never happen on its own, when the only thing that
            // resolves it is thirty seconds of his attention.
            //
            // So the axis is printed, and `login` gets the imperative. The
            // window is still shown, because it is true and it is when the
            // daemon will next find out — but it is framed as a re-check rather
            // than as a recovery.
            //
            // Note what this is NOT: a notification system. It is the line
            // `accounts status` already printed, saying which kind of bar it is.
            // `AccountActivity::to_json` carries the same `limit_axis` field, so
            // the WS `accounts.status` surface and the AUI get it for free and by
            // construction cannot disagree with this text.
            match r.limit_axis.as_deref() {
                Some("login") => {
                    println!(
                        "  LOGIN EXPIRED — run `claude /login` for this account{}. \
                         Re-checking at {until}; a completed turn clears it immediately.{}",
                        r.config_dir
                            .as_deref()
                            .map(|d| format!(" (CLAUDE_CONFIG_DIR={d})"))
                            .unwrap_or_default(),
                        r.last_limit_prose
                            .as_deref()
                            .map(|p| format!("\n    the CLI said: {p:?}"))
                            .unwrap_or_default()
                    );
                }
                Some("credit") => {
                    println!(
                        "  CREDIT EXHAUSTED — top this account up. Re-checking at {until}.{}",
                        r.last_limit_prose
                            .as_deref()
                            .map(|p| format!("\n    the CLI said: {p:?}"))
                            .unwrap_or_default()
                    );
                }
                axis => {
                    println!(
                        "  limit until {until}{}{}",
                        axis.map(|a| format!(" [{a}]")).unwrap_or_default(),
                        r.last_limit_prose
                            .as_deref()
                            .map(|p| format!(" — {p:?}"))
                            .unwrap_or_default()
                    );
                }
            }
        }
        if let Some(a) = axes {
            println!("  gauge (already known, not polled): {a}");
        }
    }
}

fn load_config(explicit: Option<&str>) -> Config {
    let path = match config::resolve_config_path(explicit) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("threads: {e}");
            std::process::exit(1);
        }
    };
    match Config::load(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("threads: {e}");
            std::process::exit(1);
        }
    }
}

/// `threads accounts health [--json]` — the LOGIN surface, as opposed to
/// `accounts status`'s ACTIVITY surface.
///
/// Answers, per slot: who it is signed in as (by hash), whether the login is
/// live / expired / dead / a clone of another slot, how many days it has left
/// before the ~30-day refresh-token expiry, and what the daily rotation pass
/// would do with it. This is what `scripts/claude-accounts` renders.
///
/// READ-ONLY, and free. It opens `.claude.json` and `manifest.toml` files and
/// asks `security(1)` for credential blobs (a read that leaves the keychain
/// item's `mdat` untouched). It never invokes `claude`, never spends a token,
/// and cannot write a keychain item.
///
/// SECURITY: every identifying field is an 8-hex sha256 prefix, and the
/// credential blob is parsed in-process for two integers and a hash and then
/// dropped. Asserted by `credential_facts_carry_no_token_material` and
/// `build_slot_reports_finds_the_clone_across_the_fleet`.
fn accounts_health(data_dir: &std::path::Path, as_json: bool) {
    let home = match accounts::home_dir() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("threads accounts health: {e}");
            std::process::exit(1);
        }
    };
    let now_ms = chrono::Utc::now().timestamp_millis();

    if as_json {
        // Byte-identical to the `accounts.health` WS payload — ONE builder, so
        // the interface can stop spawning this process to learn these facts.
        match accounts::health_json(data_dir, &home, now_ms) {
            Ok(v) => println!("{v}"),
            Err(e) => {
                eprintln!("threads accounts health: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let registry_path = data_dir.join("accounts.toml");
    let mut registry = match accounts::Registry::load(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("threads accounts health: {e}");
            std::process::exit(1);
        }
    };
    // KEPT, and read-only: this path never saves. The CREDENTIAL fleet is the
    // one stated exception — the machine store has a refresh-token deadline
    // and a rotation verdict like any other slot, and rotation must keep
    // seeing it. `Registry::save` is what guarantees this in-memory row never
    // becomes a persisted registration.
    registry.ensure_default();
    let mut reports = accounts::collect_slot_reports(
        &registry,
        data_dir,
        &home,
        now_ms,
        accounts::HEALTH_WARN_BELOW_DAYS,
    );
    // Which real account IS the machine login. The `default` row stays in the
    // payload — rotation reasons about that store and must keep seeing it — but
    // it is now tagged `folds_into`, and the CLI stops printing it as a second
    // account.
    let pinned = accounts::read_live_login_pointer(data_dir);
    accounts::mark_live_login(&mut reports, pinned.as_deref());

    println!(
        "{:<20} {:<10} {:>9} {:<10} {}",
        "SLOT", "SECRET", "DAYS LEFT", "ROTATION", "STATE"
    );
    for r in &reports {
        let days = match r.days_left {
            Some(d) if d > 0.0 => format!("{d:.1}"),
            Some(_) => "expired".to_string(),
            None => "?".to_string(),
        };
        println!(
            "{:<20} {:<10} {:>9} {:<10} {}{}",
            r.label,
            r.secret_sha8.as_deref().unwrap_or("-"),
            days,
            r.rotation.kind(),
            r.health.summary(),
            match (&r.folds_into, r.is_live_login) {
                (Some(l), _) => format!("  (the machine login — this is account {l})"),
                (None, true) => "  ← LIVE LOGIN".to_string(),
                (None, false) => String::new(),
            }
        );
    }
}

/// Open the store, pool and budgets the way every read-only accounts verb
/// needs them — the pool attached, so a share DECLARED in `pool.json` is
/// visible and not just one overridden in the store.
fn budgets_for_cli(data_dir: &std::path::Path) -> (Budgets, Vec<String>) {
    let db = data_dir.join("threads.db");
    let store = match Store::open(&db) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("threads accounts: opening {}: {e}", db.display());
            std::process::exit(1);
        }
    };
    let registry = match accounts::Registry::load(&data_dir.join("accounts.toml")) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("threads accounts: {e}");
            std::process::exit(1);
        }
    };
    let pool = match threads::pool::Pool::load(&data_dir.join("pool.json"), &registry) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("threads accounts: {e}");
            std::process::exit(1);
        }
    };
    let names = pool.names();
    (
        Budgets::new(store).with_pool(std::sync::Arc::new(pool)),
        names,
    )
}

/// `threads accounts pool-share [<account> [<share>]] [--note …] [--json]`
///
/// **The CLI half of closing the `set_pool_share` hole.** That setter existed
/// with no caller but a test, so the number could only be changed by editing
/// SQLite by hand. Bare, this lists every pool member's effective share and
/// where it came from; with an account and a fraction, it sets one.
///
/// The share is a FRACTION in (0.0, 1.0] — 0.6 for "lend the pool 60% of this
/// account" — never a percentage point and never a token count. Out-of-range
/// values are refused by `Store::set_pool_share`, not clamped.
///
/// **A share of 0 IS REFUSED, and is not how you take an account out** — that
/// was true between 2026-09-08 and 2026-09-20 and is not any more. Whether an
/// account is in the pool is a separate fact with its own door
/// (`accounts.disable` / `accounts.enable`), and it leaves this number exactly
/// as set: an account disabled on a 0.6 share is still lent 0.6, still listed
/// here at 0.6, and comes back at 0.6. The `STATE` column below says which
/// accounts are out.
/// **P10 — THE WRITE PATH GOES THROUGH THE DAEMON, AND REFUSES IF IT IS
/// DOWN.**
///
/// `Store::set_pool_share` is called here directly no longer. The reason is not
/// tidiness: the daemon holds the pool cell placement reads its shares off, and
/// a share written behind its back is a share placement does not use until
/// something happens to reload. It also rings `account_picture_changed_for`
/// and now emits `Fact::PoolShare`, so a direct write is a write no board and
/// no shape ever hears about.
///
/// **Daemon down ⇒ REFUSE, naming the daemon and the URL.** There is no
/// fallback direct write, deliberately: one writer, or two answers. The bare
/// LISTING verb keeps reading the store directly — it is read-only and owes
/// nobody an event.
/// `threads accounts recheck <name>` — R12.10. Ask the RUNNING daemon to
/// re-check every flag on `<name>` now (`accounts/{account}/recheck`): the
/// trusted identity read and the slot's credential probe, a pass clearing each
/// flag and a fail keeping it with a fresh reason. `claude-accounts` calls this
/// as the last act of a login that passed its identity guard. Exit 4 = the
/// daemon could not be reached (nothing was re-checked; the daemon's own
/// sample re-reads the slot within its period).
async fn accounts_recheck(config: &Config, account: &str, as_json: bool) {
    let url = daemon_url(config);
    let out = match ask_daemon(
        &url,
        config.effective_secret().as_deref(),
        &format!("accounts/{account}/recheck"),
        serde_json::json!({}),
    )
    .await
    {
        Ok(v) => v,
        Err(why) => {
            eprintln!(
                "threads accounts recheck: COULD NOT REACH THE `threads` DAEMON at {url} — \
                 {why}\nNothing was re-checked. The daemon re-reads the slot on its own \
                 credential sample; run this again once it is up to re-check now."
            );
            std::process::exit(4);
        }
    };
    if as_json {
        println!("{out}");
        return;
    }
    let list = |k: &str| {
        out[k]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default()
    };
    if let Some(s) = out["skipped"].as_str() {
        println!("{account}: not re-checked — {s}");
        return;
    }
    println!(
        "{account}: identity {}{}",
        out["identity_state"].as_str().unwrap_or("?"),
        out["identity_reason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default()
    );
    let cleared = list("cleared");
    if !cleared.is_empty() {
        println!("  cleared: {cleared} — {}", out["evidence"].as_str().unwrap_or(""));
    }
    let kept = list("kept");
    if kept.is_empty() {
        println!("  no flag left: available");
    } else {
        println!("  kept: {kept} — {}", out["reason"].as_str().unwrap_or(""));
    }
}

async fn accounts_pool_share(
    config: &Config,
    data_dir: &std::path::Path,
    account: Option<&String>,
    share: Option<&String>,
    args: &[String],
    as_json: bool,
) {
    let (budgets, pool_names) = budgets_for_cli(data_dir);

    if let (Some(account), Some(share_str)) = (account, share) {
        let Ok(share) = share_str.parse::<f64>() else {
            eprintln!(
                "threads accounts pool-share: {share_str:?} is not a number. The share is a \
                 fraction in [0.0, 1.0] — 0.6 means 60% of this account, and 0 DISABLES it \
                 (out of placement selection, still registered and still visible)."
            );
            std::process::exit(2);
        };
        let note = args
            .iter()
            .position(|a| a == "--note")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str);
        let url = daemon_url(config);
        let mut params = serde_json::json!({ "share": share });
        if let Some(n) = note {
            params["note"] = serde_json::Value::String(n.to_string());
        }
        let out = match ask_daemon(
            &url,
            config.effective_secret().as_deref(),
            &format!("accounts/{account}/pool_share_set"),
            params,
        )
        .await
        {
            Ok(v) => v,
            Err(why) => {
                eprintln!(
                    "threads accounts pool-share: COULD NOT REACH THE `threads` DAEMON at \
                     {url} — {why}\n\
                     \n\
                     NOTHING WAS WRITTEN, deliberately. The daemon is the only writer of a \
                     pool share: it holds the pool that placement reads shares off, and it \
                     is what tells every connected client the share moved. A share written \
                     around it would decide nothing until something happened to reload, and \
                     no interface would know it had changed.\n\
                     \n\
                     Start the daemon and run this again. To READ the current shares \
                     without it, run this verb with no arguments."
                );
                std::process::exit(4);
            }
        };
        if as_json {
            println!("{out}");
        } else {
            let row = &out["shares"][0];
            println!(
                "{}: share {} (was set here; source {}){}",
                account,
                row["share"],
                row["source"].as_str().unwrap_or("?"),
                // SAY IT OUT LOUD. Since 2026-09-20 the share he just set and
                // the account's membership of the pool are independent, so it is
                // now possible to set a perfectly ordinary 60% share on an
                // account that is OUT — and a surface that printed "share 0.6"
                // and stopped would have him believe he had just changed
                // something that decides nothing until he enables it.
                if row["disabled"].as_bool().unwrap_or(false) {
                    "  — NOTE: this account is DISABLED, so this share decides nothing until you \
                     enable it. The share is kept exactly as set and is what it comes back at. \
                     Nothing lifts a disable on its own."
                } else {
                    ""
                }
            );
            if !pool_names.iter().any(|n| n == account) {
                println!(
                    "  NOTE: {account:?} is not currently a pool member, so this share decides \
                     nothing until it is one. The row is kept — a share is a statement about what \
                     you are lending the pool, and it survives the account leaving and rejoining."
                );
            }
        }
        return;
    }

    let names = match account {
        Some(a) => vec![a.clone()],
        None => pool_names,
    };
    let out = budgets.pool_share_rows(&names);
    if as_json {
        println!("{out}");
        return;
    }
    println!(
        "{:<24} {:>8}  {:<8}  {:<9} {}",
        "ACCOUNT", "SHARE", "STATE", "SOURCE", "NOTE"
    );
    for row in out["shares"].as_array().into_iter().flatten() {
        println!(
            "{:<24} {:>8}  {:<8}  {:<9} {}",
            row["account"].as_str().unwrap_or("?"),
            row["share"],
            // A COLUMN, not a footnote. A disabled account has to be legible in
            // the listing itself — this is the surface the operator scans to
            // find out what his pool currently is.
            if row["disabled"].as_bool().unwrap_or(false) {
                "DISABLED"
            } else {
                "in pool"
            },
            row["source"].as_str().unwrap_or("?"),
            row["note"].as_str().unwrap_or(""),
        );
    }
}

/// `threads accounts pressure [--json]` — the ranked required burn rate, the
/// share it is divided by, and the reset window it is measured against.
///
/// UNRANKED accounts print `—`, never `0.00`. Zero pressure is the busiest
/// possible reading and "we have never measured this account" is the absence of
/// evidence; a surface that renders both the same way is lying about one.
fn accounts_pressure(data_dir: &std::path::Path, as_json: bool) {
    let (budgets, names) = budgets_for_cli(data_dir);
    let out = budgets.pressure_rows(&names, chrono::Utc::now());
    if as_json {
        println!("{out}");
        return;
    }
    println!(
        "{:<24} {:>5} {:>10} {:>7} {:>7}  {:<10} {}",
        "ACCOUNT", "RANK", "PRESSURE", "SHARE", "USED%", "BASIS", "RESETS / WHY NOT"
    );
    for r in out["accounts"].as_array().into_iter().flatten() {
        let pressure = match r["pressure"].as_f64() {
            Some(v) => format!("{v:.3}"),
            None => "—".to_string(),
        };
        let rank = match r["rank"].as_u64() {
            Some(v) => v.to_string(),
            None => "—".to_string(),
        };
        let used = match r["observed_pct"].as_f64() {
            Some(v) => format!("{v:.1}"),
            None => "—".to_string(),
        };
        let tail = match r["unranked_reason"].as_str() {
            Some(why) => format!("UNRANKED: {why}"),
            None => r["resets_at"].as_str().unwrap_or("—").to_string(),
        };
        println!(
            "{:<24} {:>5} {:>10} {:>7} {:>7}  {:<10} {}",
            r["account"].as_str().unwrap_or("?"),
            rank,
            pressure,
            r["share"],
            used,
            r["reset_basis"].as_str().unwrap_or("—"),
            tail,
        );
    }
}

/// `threads accounts rename <old> <new>` — move a slot's directory, keychain
/// item, manifest and registry entry together. The keychain service name is
/// derived from the directory path, so a rename that skipped it would orphan
/// the credential; see `accounts::rename_account` for the ordering argument.
fn accounts_rename(data_dir: &std::path::Path, old: &str, new: &str) {
    let registry_path = data_dir.join("accounts.toml");
    let registry = match accounts::Registry::load(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("threads accounts rename: {e}");
            std::process::exit(1);
        }
    };
    let mut existing = registry.names();
    if let Ok(rd) = std::fs::read_dir(data_dir.join("accounts")) {
        for e in rd.flatten() {
            if let Some(n) = e.file_name().to_str() {
                if e.path().is_dir() && !existing.iter().any(|x| x == n) {
                    existing.push(n.to_string());
                }
            }
        }
    }
    if let Some(why) = accounts::rename_refusal(old, new, &existing) {
        eprintln!("threads accounts rename: {why}");
        std::process::exit(2);
    }
    match accounts::rename_account(data_dir, old, new, accounts::KeychainMode::Auto) {
        Ok(o) => {
            println!("renamed {} → {}", o.old_label, o.new_label);
            println!("  dir      {} → {}", o.old_dir.display(), o.new_dir.display());
            println!(
                "  keychain {}",
                if o.credential_moved {
                    "the slot's credential moved to the service name derived from the new path"
                } else {
                    "nothing to move (this slot had no keychain item)"
                }
            );
            println!("  registry accounts.toml updated");
            println!(
                "\n  NOTE: threads already recorded against {:?} keep that name in the\n  \
                 thread store and in budget history. New work runs under {:?}.",
                o.old_label, o.new_label
            );
        }
        Err(e) => {
            eprintln!("threads accounts rename: {e}");
            std::process::exit(1);
        }
    }
}

/// `threads accounts delete <name>` — remove a slot, its keychain item and its
/// registry entry. LOCAL ONLY: no `logout`, nothing revoked server-side, and
/// the machine-default credential is never a side effect (asserted in
/// `accounts::delete_account`).
fn accounts_delete(data_dir: &std::path::Path, label: &str, force: bool) {
    let mut registry = match accounts::Registry::load(&registry_path_of(data_dir)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("threads accounts delete: {e}");
            std::process::exit(1);
        }
    };
    // KEPT, and read-only: `delete_refusal` needs the machine slot present to
    // REFUSE deleting the account the live login is signed in as. Removing it
    // here would not purge a concept, it would remove a guard.
    registry.ensure_default();
    let home = match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => std::path::PathBuf::from(h),
        _ => {
            eprintln!("threads accounts delete: HOME is unset");
            std::process::exit(1);
        }
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    // R9.A2c-r2: the shared lookup — a DISABLED slot exists (unread) and can
    // be deleted (CON-28 bars reading it, not removing it).
    if let Some(why) = accounts::delete_lookup(&registry, data_dir, &home, label, force, now_ms) {
        eprintln!("threads accounts delete: {why}");
        std::process::exit(2);
    }
    match accounts::delete_account(data_dir, label, accounts::KeychainMode::Auto) {
        Ok(o) => {
            println!("deleted {}", o.label);
            println!(
                "  dir      {}",
                if o.dir_removed { o.dir.display().to_string() } else { "(none)".into() }
            );
            println!(
                "  keychain {}",
                if o.keychain_removed {
                    "the slot's keychain item was removed — local only, nothing was revoked"
                } else {
                    "(no keychain item for this slot)"
                }
            );
            println!(
                "  registry {}",
                if o.registry_removed { "entry removed" } else { "(was not registered)" }
            );
        }
        Err(e) => {
            eprintln!("threads accounts delete: {e}");
            std::process::exit(1);
        }
    }
}

fn registry_path_of(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join("accounts.toml")
}

/// Env-filtered stderr logging (`RUST_LOG`, default `info`), **plus the warning
/// counter**.
///
/// ## Why this stopped being `fmt().init()`
///
/// It was one line and it is now four, because a `tracing_subscriber::fmt()`
/// builder is a whole subscriber and cannot have a second layer added to it.
/// `registry()` can. The `fmt` layer below is the SAME stderr writer and the SAME
/// env filter as before — nothing about the log file changes — and
/// [`threads::warnings::WarningCounter`] rides beside it.
///
/// **Beside, never instead of.** `~/.aui/threads-daemon.log` remains the full
/// record at every level; the counter and the warnings-only file under
/// `~/.cauldron/threads.warnings/` are additions. The failure being fixed is that
/// the log was the ONLY output, not that it exists.
///
/// ## The filter runs FIRST, and that is deliberate
///
/// A layer sees only what the filter passes, so `RUST_LOG=error` hides `WARN` from
/// the counter exactly as it hides it from the file. That keeps the number and the
/// log in agreement — the alternative (filtering per layer so the counter sees
/// everything) would make the counter disagree with the file beside it, and *"the
/// count says 40 and the file holds 12"* is the specific failure this feature
/// exists to make impossible. `threads::warnings`' module doc states the
/// consequence.
///
/// ## The sink is attached here, after the layer
///
/// So that a daemon whose `$HOME` and `$CAULDRON_HOME` are both unset still
/// COUNTS, and only loses the disk half. `sink_attached` on the shape says which
/// happened rather than leaving a reader to assume a file exists.
fn tracing_subscriber_init() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(threads::warnings::WarningCounter)
        .init();
    threads::warnings::attach_disk_record();
}

// ──────────────────────────────────────────────────────────────────────────
//  `threads image` — which binary is actually running
// ──────────────────────────────────────────────────────────────────────────

/// Ask the DAEMON what image it is executing, and render the answer.
///
/// Exit codes, so this is usable from a script and from a supervisor:
///
/// * `0` — the daemon answered.
/// * `3` — the daemon answered and **the launch path has diverged from the
///   running image**. Not an error; a distinct, actionable state that a caller
///   should be able to branch on without parsing prose.
/// * `4` — the daemon could not be reached. The disk half is still printed.
///
/// The payload printed under `--json` is byte-identical to the `daemon.image`
/// WS payload, and the human rendering is built from that same payload by
/// [`threads::image::render`]. One question, one answer, one shape.
async fn image_verb(config: &Config, as_json: bool) {
    use serde_json::{json, Value};

    let url = daemon_url(config);
    let secret = config.effective_secret();

    // The launch path we fall back to reporting on if the daemon is not
    // answering. It is THIS process's executable, which is the daemon's launch
    // path only when the same binary was invoked — stated in the payload
    // rather than assumed, since a wrong path asserted confidently is the bug
    // this command exists to correct.
    let own_exe = std::env::current_exe().ok();
    let data_dir = config.resolved_data_dir().unwrap_or_else(|_| ".".into());
    let cx = threads::image::Context {
        launch_path: own_exe.as_deref(),
        data_dir: &data_dir,
        boot_identity: None,
        boot_id: None,
        image_booted_at: None,
    };

    let payload = match ask_daemon(&url, secret.as_deref(), "daemon/image", json!({})).await {
        Ok(v) => v,
        Err(why) => {
            let v = threads::image::unreachable_report(&cx, &why);
            emit_image(&v, as_json);
            eprintln!(
                "\nthreads image: COULD NOT REACH THE DAEMON at {url} — {why}\n\
                 Everything above the divergence section is what is ON DISK. Nothing here \
                 describes a running image, because none was observable."
            );
            std::process::exit(4);
        }
    };

    emit_image(&payload, as_json);

    let diverged = payload
        .get("divergence")
        .and_then(|d| d.get("diverged"))
        .and_then(|d| {
            if d.get("known").and_then(Value::as_bool) == Some(true) {
                d.get("value").and_then(Value::as_bool)
            } else {
                None
            }
        });
    if diverged == Some(true) {
        std::process::exit(3);
    }
}

fn emit_image(v: &serde_json::Value, as_json: bool) {
    if as_json {
        // Byte-identical to the `daemon.image` WS payload — one shape.
        println!("{v}");
    } else {
        print!("{}", threads::image::render(v));
    }
}

/// One WS round trip: connect, authenticate if required, call `daemon.image`.
type ImageWs = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// The next decodable JSON frame, or a reason we gave up. Frames that are not
/// JSON text (pings, binary) are skipped rather than mistaken for an answer.
async fn next_frame(
    ws: &mut ImageWs,
    deadline: std::time::Instant,
) -> std::result::Result<serde_json::Value, String> {
    use futures::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err("the daemon stopped answering".to_string());
        }
        match tokio::time::timeout(left, ws.next()).await {
            Err(_) => return Err("the daemon stopped answering".to_string()),
            Ok(None) => return Err("the daemon closed the connection".to_string()),
            Ok(Some(Err(e))) => return Err(format!("{e}")),
            Ok(Some(Ok(Message::Text(t)))) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                    return Ok(v);
                }
            }
            Ok(Some(Ok(_))) => continue,
        }
    }
}

/// **ONE-SHOT RPC TO THE RUNNING DAEMON** — connect, auth if asked, send one
/// request, wait for the answer on its id.
///
/// It was hard-wired to `daemon.image`: the method name, the correlation id
/// and the refusal prose were all literals, so the second caller that wanted
/// this transport (P10, the CLI share write) would have had to copy it. Now
/// `method` and `params` are arguments and the correlation id is derived from
/// the method, so there is one implementation of "talk to the daemon" rather
/// than one per verb.
/// **THE DAEMON'S ONE DOOR.** The chassis port — the legacy `:3651` listener
/// these verbs used to dial was retired at R2.S4. Same request/response frames
/// (`welcome`, `auth`, `request` → `response.outcome`), so [`ask_daemon`] is
/// unchanged; only the address moved. A `THREADS_CHASSIS_PORT` that is not a
/// port is REFUSED (exit 2), for the reason `Config::effective_chassis_port`
/// gives: quietly dialling the file value instead is the misdirection that
/// refusal exists to prevent.
fn daemon_url(config: &Config) -> String {
    match config.effective_chassis_port() {
        Ok(port) => format!("ws://{}:{}/ws", config.host, port),
        Err(e) => {
            eprintln!("threads: {e}");
            std::process::exit(2);
        }
    }
}

async fn ask_daemon(
    url: &str,
    secret: Option<&str>,
    method: &str,
    params: serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    use futures::SinkExt;
    use serde_json::{json, Value};
    use tokio_tungstenite::tungstenite::Message;

    let connect = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio_tungstenite::connect_async(url),
    );
    let (mut ws, _) = connect
        .await
        .map_err(|_| "the connection attempt timed out after 10s".to_string())?
        .map_err(|e| format!("{e}"))?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);

    // The Welcome frame says whether auth is required.
    let welcome = next_frame(&mut ws, deadline).await?;
    if welcome.get("auth_required").and_then(Value::as_bool) == Some(true) {
        let Some(s) = secret else {
            return Err(
                "the daemon requires a shared secret and none is configured (set `secret` in \
                 the config or $THREADS_SECRET)"
                    .to_string(),
            );
        };
        ws.send(Message::Text(json!({"type":"auth","secret":s}).to_string().into()))
            .await
            .map_err(|e| format!("{e}"))?;
        let ack = next_frame(&mut ws, deadline).await?;
        if ack.get("type").and_then(Value::as_str) == Some("auth_failed") {
            return Err("the daemon refused the shared secret".to_string());
        }
    }

    let correlate = format!("{method}-1");
    ws.send(Message::Text(
        json!({"type":"request","id":correlate,"method":method,"params":params})
            .to_string()
            .into(),
    ))
    .await
    .map_err(|e| format!("{e}"))?;

    loop {
        let f = next_frame(&mut ws, deadline).await?;
        if f.get("type").and_then(Value::as_str) == Some("response")
            && f.get("correlate").and_then(Value::as_str) == Some(correlate.as_str())
        {
            let outcome = f.get("outcome").cloned().unwrap_or(Value::Null);
            if outcome.get("type").and_then(Value::as_str) == Some("success") {
                return Ok(outcome.get("payload").cloned().unwrap_or(Value::Null));
            }
            return Err(format!(
                "the daemon refused `{method}`: {}",
                outcome.get("error").and_then(Value::as_str).unwrap_or("unknown error")
            ));
        }
    }
}
