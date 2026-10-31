//! `threads-dev` — **A THROWAWAY `threads` DAEMON, SEEDED WITH DUMMY THREADS.**
//!
//! The operator: *"You can maybe add an optional daemon path to do some testing
//! on the daemon with dummy threads before messing with our prod."*
//!
//! ```text
//!   lib/threads/scripts/dev-daemon.sh                 # start it (builds if needed)
//!   lib/threads/scripts/dev-daemon.sh --teardown      # delete its state
//! ```
//!
//! It serves `:13653` (the chassis door, its only one) out of `~/.threads-dev`,
//! with three synthetic accounts and eleven synthetic threads. It is the only
//! `threads` daemon on this machine it is safe to break.
//!
//! # WHY A SEPARATE BINARY AND NOT A `--dev` FLAG ON `threads`
//!
//! The trade-off is real and it goes both ways:
//!
//! * A **flag** shares the argv parsing and the config loading, so the dev path
//!   and the real path cannot drift in how they come up.
//! * A **separate binary** cannot be aimed at production by a typo, because the
//!   production defaults are not reachable from it at all.
//!
//! This is the separate binary, for three reasons that the flag could not
//! answer:
//!
//! 1. **The dangerous thing should take effort, and a flag inverts that.** With
//!    `threads --dev`, forgetting the flag is one keystroke away from the live
//!    port and the live store, and the failure is silent — `threads` with no
//!    arguments *is* "start the daemon in the foreground", which has already
//!    started an unsupervised second daemon on 3651 once (see `scripts/threads`,
//!    which made that meaning explicit for exactly this reason). With a separate
//!    name, the production path is a different word.
//! 2. **The sharing a flag would buy is sharing this binary already has.** The
//!    drift risk is in the BOOT, not the argv, and `threads-dev` does not boot
//!    anything: it builds a [`Config`] and calls [`threads::run_daemon`] — the
//!    same function `main.rs` calls. The bind-before-anything-writes rule,
//!    `update.on_boot()`, `recover()`, both servers, the WAL keeper and the
//!    drain-and-checkpoint shutdown are the real ones, not copies.
//! 3. **The self-update watcher arms only for a binary literally named
//!    `threads`.** `ThreadManager::start_watchers` says so, and that is a lock
//!    this binary gets for free simply by being called something else. A `--dev`
//!    flag would have had to disable the watcher by hand, which is a check that
//!    can be forgotten; a name cannot be.
//!
//! The one thing the separate binary gives up is the `--config` / `$THREADS_CONFIG`
//! file surface. That is deliberate: a dev daemon that read a config file could
//! read the PRODUCTION config file, and there is nothing in it a dev run wants.
//!
//! # WHAT MAKES IT IMPOSSIBLE TO CONFUSE WITH PRODUCTION
//!
//! * **A different port.** `13653`, a constant and not a flag
//!   ([`dev::DEV_CHASSIS_PORT`]). There is no argument that makes this serve 3653.
//! * **A different data dir**, `~/.threads-dev`, which this process creates and
//!   marks as its own.
//! * **A guard, not a comment.** [`dev::claim_dev_dir`] refuses the production
//!   store — by `$THREADS_HOME`, by `~/.threads`, by the cauldron home, by being
//!   inside a cargo source tree (which is where production actually lives on this
//!   machine), and by the absence of its own marker file. `--teardown` runs the
//!   identical guard before it deletes anything.
//! * **A banner that says what it is**, on stdout, before anything is served.
//!
//! # THE ENVIRONMENT IT REFUSES TO INHERIT
//!
//! Seven variables — six that are each a way the operator's own shell could have
//! reached production from here, plus one that only makes the log readable. They
//! are overridden before the manager is built; see [`isolate`] for the reason on
//! each.

use std::path::{Path, PathBuf};

use threads::dev;

const USAGE: &str = "\
threads-dev — a THROWAWAY threads daemon with dummy threads. Never production.

  threads-dev                       seed ~/.threads-dev and serve :13653
  threads-dev --scale 10            ten times as much synthetic data
  threads-dev --no-seed             serve what is already there, seed nothing
  threads-dev --seed-only           seed and exit without serving
  threads-dev --teardown            delete ~/.threads-dev and exit
  threads-dev --home <dir>          use <dir> instead of ~/.threads-dev
  threads-dev --help

  THREADS_DEV_HOME                  same as --home (NOT THREADS_HOME, which is
                                    how production is pointed at its store)

Exit codes: 0 fine · 2 bad usage · 3 the production guard refused · 1 other error.
";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return;
    }
    let teardown = take_flag(&mut args, "--teardown");
    let no_seed = take_flag(&mut args, "--no-seed");
    let seed_only = take_flag(&mut args, "--seed-only");
    let home_arg = parse_flag(&mut args, "--home");
    let scale_arg = parse_flag(&mut args, "--scale");
    if !args.is_empty() {
        eprintln!("threads-dev: unexpected argument: {}\n", args[0]);
        eprint!("{USAGE}");
        std::process::exit(2);
    }

    let scale = match scale_arg.as_deref() {
        None => dev::Scale::default(),
        Some(s) => match s.parse::<usize>() {
            Ok(n) if n >= 1 => dev::Scale::times(n),
            _ => {
                eprintln!("threads-dev: --scale wants a positive integer, got {s:?}");
                std::process::exit(2);
            }
        },
    };

    let home: PathBuf = match home_arg {
        Some(h) => PathBuf::from(h),
        None => match dev::dev_home() {
            Ok(h) => h,
            Err(e) => {
                eprintln!("threads-dev: {e}");
                std::process::exit(1);
            }
        },
    };

    if teardown {
        match dev::teardown_dev_dir(&home) {
            Ok(true) => println!("threads-dev: removed {}", home.display()),
            Ok(false) => println!("threads-dev: nothing to remove at {}", home.display()),
            Err(e) => {
                eprintln!("threads-dev: {e}");
                std::process::exit(3);
            }
        }
        return;
    }

    // ── THE GUARD, FIRST, BEFORE ANY ENVIRONMENT IS TOUCHED ──────────────
    //
    // Nothing above this line has read or written a byte of state, and the guard
    // itself only stats paths. A refusal therefore leaves the machine exactly as
    // it was, which is the property that makes it safe to aim this binary at
    // something and find out.
    if let Err(e) = dev::claim_dev_dir(&home) {
        eprintln!("threads-dev: {e}");
        std::process::exit(3);
    }

    isolate(&home);
    threads_dev_main(home, scale, no_seed, seed_only);
}

/// **OVERRIDE EVERY VARIABLE BY WHICH THIS PROCESS COULD REACH PRODUCTION.**
///
/// Each of these is a real path from the operator's shell into the live daemon's
/// state, found by reading what the crate reads. The dev daemon does not trust
/// its inherited environment on any of them.
fn isolate(home: &Path) {
    // 1. `THREADS_HOME` — THE production pointer on this machine.
    //    `~/.threads-supervisor/paths.env` exports it as
    //    `/Users/testuser/code/substrate-interface-layer/lib/threads`, and
    //    `config::state_home()` honours it. `dev_config` passes an explicit
    //    `data_dir` so `resolved_data_dir()` never consults it — but
    //    `config::resolve_config_path`, `login.rs`'s two child-process `.env`
    //    calls and `threads-stress` all read it independently, so it is
    //    REDIRECTED rather than relied upon to be unused.
    std::env::set_var("THREADS_HOME", home);

    // 2. `CAULDRON_HOME` — the chassis CONSUMER LEDGER's home, which defaults to
    //    `~/.cauldron` and is shared by every producer on the machine.
    //    `run_daemon` attaches `consumers::producer_ledger_path("threads")`
    //    unconditionally, so without this the dev daemon would append its boots
    //    and its subscribers to `~/.cauldron/chassis/threads.producer.jsonl` —
    //    the production file, under the production daemon's own name. The
    //    override is the one `shapewire::ledger::cauldron_home` documents for
    //    exactly this case: *"a second chassis run side by side"*.
    std::env::set_var("CAULDRON_HOME", home.join("cauldron"));

    // 3. `THREADS_CLAUDE_HOME` — the transcript home. Unset, the machine-default
    //    account's transcript store is `$HOME/.claude`, which is the OPERATOR'S
    //    OWN login. Every test in the crate points this at scratch for the same
    //    reason.
    std::env::set_var("THREADS_CLAUDE_HOME", home.join("claude-home"));

    // 4. `THREADS_CLAUDE_BIN` — what gets spawned for a turn. The dev daemon
    //    must never invoke the real CLI: that would spend the operator's quota
    //    from a daemon he started to look at a bug, and against the synthetic
    //    config dirs it would fail authentication anyway. The crate's own
    //    `fake-claude` fixture is the right answer — canned stream-json, no
    //    network — and it is reused rather than reinvented.
    let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude");
    if fake.is_file() {
        std::env::set_var("THREADS_CLAUDE_BIN", &fake);
    } else {
        // The fixture is not beside this binary (it was built elsewhere, or the
        // tree moved). Point at something that is guaranteed to fail rather than
        // fall through to whatever `claude` is on PATH — a dev daemon that
        // silently reached the real CLI is the one outcome worth refusing.
        std::env::set_var("THREADS_CLAUDE_BIN", "/nonexistent/threads-dev-no-claude");
    }

    // 5. `THREADS_CHASSIS_PORT` — `Config::effective_chassis_port` lets the
    //    environment WIN over the config value, so a stray export in the
    //    operator's shell would move the dev chassis door or (at `0`) silently
    //    switch it off. Pinned to the dev port so the banner cannot be wrong.
    std::env::set_var("THREADS_CHASSIS_PORT", dev::DEV_CHASSIS_PORT.to_string());

    // 6. `THREADS_SECRET` — `Config::effective_secret` prefers the environment.
    //    If production's secret is exported here, the dev daemon would demand an
    //    auth frame nobody pointed a TUI at it would know to send. Removed, so
    //    the dev socket is open — it is bound to 127.0.0.1 on a port nothing else
    //    uses, and the whole point is that it is trivial to connect to.
    std::env::remove_var("THREADS_SECRET");

    // 7. `THREADS_RESUME_TICK_MS` — the BACKSTOP period: how often a placement
    //    pass runs when no event has woken one. (It is no longer the mechanism;
    //    placement re-runs on the bell — see `ThreadManager::placement_wake`.) At
    //    the default 5000 the seeded fixture produces three `RUNNING ANYWAY …
    //    keeping the floor` WARN pairs every five seconds forever, which buries
    //    the lines an operator is actually reading. Nothing in the fixture can
    //    make progress on a sweep — every parked thread holds a retry floor two
    //    hours out — so the backstop has nothing to do and 60s is plenty. It is
    //    the same value the crate's own tests use. `0` switches the backstop off
    //    entirely, which is how the placement-event tests prove the events alone
    //    do the work.
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    // 8. `THREADS_ROSTER_RELOAD_MS` — how often the account roster is re-read
    //    from disk by the noticer task (default 30000). Left at the default here:
    //    the dev fixture does not rewrite `accounts.toml` underneath itself, two
    //    file reads every thirty seconds is nothing, and a dev daemon that DOES
    //    want to see an account adopted in another terminal wants it on the
    //    production cadence rather than a special one.

    // And the self-update watcher OFF. It is already inert — it arms only for a
    // binary literally named `threads` — but this is the lock that does not
    // depend on the file name staying what it is. A dev daemon must never
    // `execv` itself or promote a `threads.last-good`.
    std::env::set_var("THREADS_UPDATE_WATCH", "off");
}

#[tokio::main]
async fn threads_dev_main(home: PathBuf, scale: dev::Scale, no_seed: bool, seed_only: bool) {
    tracing_init();

    let config = dev::dev_config(&home);

    // ── SEED FIRST, ON A STORE OF OUR OWN, THEN DROP IT ──────────────────
    //
    // `run_daemon` opens its own `Store` and `recover()`s against it, so the
    // fixture has to be on disk before that happens or boot recovery would be
    // reasoning about an empty store. The handle is scoped so it is closed
    // before the daemon's own is opened.
    let report = if no_seed {
        None
    } else {
        let db = home.join("threads.db");
        match threads::Store::open(&db).and_then(|s| dev::seed(&s, scale)) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("threads-dev: seeding {}: {e}", db.display());
                std::process::exit(1);
            }
        }
    };

    banner(&home, &config, report.as_ref(), scale);

    if seed_only {
        println!("--seed-only: not serving. Run without it to start the daemon.");
        return;
    }

    // THE ONE BOOT PATH. Everything from here is the production daemon's own
    // startup, against a config that points somewhere else.
    if let Err(e) = threads::run_daemon(config).await {
        eprintln!("threads-dev: {e}");
        std::process::exit(1);
    }
}

/// **SAY LOUDLY WHAT THIS IS**, on stdout, before a single connection is
/// accepted. Logs go to stderr, so this survives `2>/dev/null` and is the first
/// thing in a scrollback.
fn banner(
    home: &Path,
    config: &threads::Config,
    report: Option<&dev::SeedReport>,
    scale: dev::Scale,
) {
    println!();
    println!("╔══════════════════════════════════════════════════════════════════════════╗");
    println!("║           T H R E A D S   D E V   D A E M O N  —  NOT PRODUCTION         ║");
    println!("║                                                                          ║");
    println!("║  Every account, thread and message below is SYNTHETIC. Nothing here is   ║");
    println!("║  the operator's. Breaking it costs nothing; `--teardown` deletes it all. ║");
    println!("╚══════════════════════════════════════════════════════════════════════════╝");
    println!();
    println!("  data dir     {}", home.display());
    println!(
        "  API          ws://{}:{}/ws        (production is :{})",
        config.host,
        config.chassis_port,
        threads::config::DEFAULT_CHASSIS_PORT
    );
    println!("  health       http://{}:{}/health", config.host, config.chassis_port);
    println!("  auth         none");
    println!("  claude       {}", claude_bin_note());
    println!();
    println!("  POINT A CONSUMER AT IT");
    println!(
        "    the board   THREADS_TUI_WS_PORT={} lib/threads/scripts/threads",
        config.chassis_port
    );
    println!(
        "    the TUI     TUI_THREADS_URL=ws://127.0.0.1:{}/ws  harness/scripts/tui",
        config.chassis_port
    );
    println!(
        "    thread-msgs THREADS_WS_URL=ws://127.0.0.1:{}/ws   core/tools/thread-messages",
        config.chassis_port
    );
    println!(
        "    anything    ws://127.0.0.1:{}/ws",
        config.chassis_port
    );
    println!();
    match report {
        None => println!("  seeding      SKIPPED (--no-seed)"),
        Some(r) => {
            println!(
                "  seeded       {} accounts · {} threads created, {} already present",
                r.accounts, r.threads_created, r.threads_already_present
            );
            println!(
                "               {} events · {} queue waiting · {} queue FAILED",
                r.events, r.queue_waiting, r.queue_failed
            );
            println!(
                "               {} parked (each holding a retry floor: {}) · {} locked",
                r.parked, r.retrying, r.locked
            );
            println!(
                "               scale: {} threads max ({} parked, {} queue-bearing, {} silent, {} idle)",
                scale.threads(),
                scale.parked,
                scale.with_queue,
                scale.silent,
                scale.idle
            );
        }
    }
    println!();
    println!("  THE SEEDED SCENARIOS");
    println!(
        "    accounts    {} (healthy) · {} (rate-limited) · {} (DISABLED + dead login)",
        dev::ACCOUNT_HEALTHY,
        dev::ACCOUNT_LIMITED,
        dev::ACCOUNT_DEAD
    );
    println!("    dev-parked-*   parked on an unavailable account, prompts held verbatim");
    println!("    dev-queued-*   waiting AND failed queue rows behind a held thread lock");
    println!("    dev-silent-*   NEVER spoken in — absent from `threads/recent-updates`");
    println!("    dev-idle-*     ordinary threads with history (the control arm)");
    println!();
    println!("  Ctrl-C to stop. `--teardown` to delete {}.", home.display());
    println!();
}

fn claude_bin_note() -> String {
    match std::env::var("THREADS_CLAUDE_BIN") {
        Ok(p) if p.ends_with("fake-claude") => format!("{p}  (the test fixture — never the real CLI)"),
        Ok(p) => format!("{p}  (deliberately unusable — no fixture found)"),
        Err(_) => "<unset>".to_string(),
    }
}

fn tracing_init() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .init();
}

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
        eprintln!("threads-dev: {flag} wants a value\n");
        eprint!("{USAGE}");
        std::process::exit(2);
    }
    None
}
