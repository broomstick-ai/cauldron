//! # threads — the thread system-of-record daemon
//!
//! **Supersedes `lib/ccw` (substrate-ccw)** per the operator's decisions in
//! the CCW-migration design doc §10–§12 (harness workspace `aui-tweaks`,
//! artifact `ccw-migration-design-input`): "the application is named
//! `threads`" (Q13); "it's the ledger of record for our thread… store
//! everything about our threads" (§11). Clients — AUI today, the operating
//! system tomorrow — reach Claude Code exclusively through this daemon's WS
//! contract ([`protocol`]).
//!
//! ## Module map
//!
//! - **[`server`]** — the method table the chassis serves (default port
//!   [`config::DEFAULT_CHASSIS_PORT`], the only port the daemon binds):
//!   threads (create/send/update/history/messages/cancel/queue/event),
//!   accounts, the spend ledger. [`chassis`] is the door; the envelope, the
//!   boot-id welcome and the optional secret auth are `shapewire`'s.
//! - **[`runtime`]** — the [`runtime::ThreadManager`]: queue dispatch with two
//!   send intents, the durable-output turn runner with pid re-adoption,
//!   persisted retries with account failover, transient liveness, compaction
//!   bookkeeping, boot recovery.
//! - **[`store`]** — SQLite (WAL): the event ledger (dense `seq` + spoken
//!   `message_id` + `ts`), queue, retries, running turns, accounts, usage
//!   ledger, import watermarks.
//! - **[`event`]** / **[`protocol`]** — the persisted taxonomy, and the
//!   transport proto version (the legacy frames went with `:3651`, R2.S4).
//! - **[`settings`]** — the live per-thread settings blob (model, cwd, plugin
//!   attachments, retry policy) — changeable mid-thread.
//! - **[`spawn`]** / **[`stream`]** — the canonical claude recipe and the
//!   stream-json → event mapping (ported from CCW).
//! - **[`retry`]** / **[`reset`]** — CCW+AUI merged failure classification and
//!   reset-prose parsing.
//! - **[`plugins`]** / **[`context`]** — plugin rollup + hot reload, and the
//!   agent-facing context injection (message-id headers, post-compaction
//!   restatement).
//! - **[`import`]** — full import + delta reconciliation of AUI's existing
//!   `.mind/threads` stores.
//! - **[`update`]** — THE DAEMON'S OWN UPDATE PROTOCOL (intent item 64): it
//!   watches its own binary by code identity, queues a restart durably, and
//!   `execv`s the new build at the first instant with no turn executing, no WS
//!   request in flight and admission provably closed. Nothing else can deploy
//!   this daemon: every agent that could is hosted BY it.
//! - **[`accounts`]** — the account registry + tracked limit state
//!   (multi-account scaffolding).
//! - **[`scrub`]** — THE TRANSCRIPT SCRUB: before every spawn, the details of
//!   every API error on the transcript the child is about to `--resume` from
//!   (and on its sidecar subagent transcripts) are replaced with the literal
//!   `<scrubbed>`. A model that reads a usage limit off its own history plans
//!   against a resource it does not control; on 2026-08-14 one silently
//!   downgraded itself to conserve the operator's quota. Per-thread flag
//!   `scrub_api_error_details`, default true.
//! - **[`budgets`]** / **[`gauge`]** — the budgets module (§15): keyed,
//!   account-SCOPED budgets with a meter on every axis (session, weekly
//!   all-model and weekly fable in percent; cash in pooled dollars),
//!   per-thread threshold gates on the same four gauges, an admission gate
//!   that PARKS rather than drops and FAILS CLOSED on an internal error
//!   (bypassable per thread with `budgeting_off`), and the layered gauge
//!   (`rate_limit_event` push + `/usage` poll + 429 backstop) feeding a
//!   continuously re-estimated per-account per-axis token limit.
//!
//! ## Binaries
//!
//! - `threads` — the daemon (plus `import` / `reconcile` / `accounts`).
//! - `threads-stress` — the stress-test driver: one exclusive execution with a
//!   threshold or budget value set under it, reported as `gated` / `parked` /
//!   `tripped` / `killed` / `finished`. Wrapper: `scripts/stress-test.sh`.

pub mod accounts;
/// **THE `agents` FAÇADE** — `docs/managed-subagents.md` §2 on the MCP door:
/// one act has one method path, and the tool binds the subject into it.
pub mod agents;
pub mod budgets;
/// `threads` as a `shapewire::server::Producer` — the API's producer half.
pub mod chassis;
/// **The wall-clock seam** — the one door to an instant this crate later
/// COMPARES, and the rule for which side a new call site belongs on.
pub mod clock;
pub mod commands;
pub mod config;
pub mod consumers;
pub mod context;
/// **THE THROWAWAY DEV DAEMON** — its ports, its production guard, and the
/// synthetic scenarios it seeds. Nothing in [`run_daemon`] or [`build_manager`]
/// calls into this; it is reached only by the `threads-dev` binary and by
/// `tests/dev_daemon.rs`.
pub mod dev;
pub mod error;
pub mod event;
pub mod gauge;
pub mod image;
pub mod import;
/// **`/health` ANSWERED OFF THE RUNTIME** (R9.C7i): a std listener on its own
/// OS thread that answers from a lag stamp and the last cached verdict.
pub mod health_door;
/// **THE PERIODIC SUBAGENT HEALTH INVARIANT** (R10.H2, HEALTH F3): I1–I5,
/// every 5 min, warned on the `warnings` shape with the ids named.
pub mod health_check;
/// **`/health` THAT CAN SAY NO** (R9.C6): a store-liveness heartbeat and the
/// layer that answers `/health` 503 when it goes stale.
pub mod liveness;
pub mod ledger;
pub mod login;
pub mod managed;
/// R5.S2 (DEC-34): the `/mcp` façade routes supervisor-declared tools through
/// the cauldron supervisor, which owns every MCP tool process.
pub mod mcp_route;
/// **MESSAGING NETWORKS** — `docs/agent-networks.md`: home networks derived
/// from `parent_id`, created networks with per-member flags, mail through the
/// daemon's durable queue. The rules; `store::networks` is the rows.
pub mod networks;
pub mod operations;
/// **The protoledger adapter** (PL-1): `PlStore` and the writers for what
/// cauldron itself records. The only module that may `use protoledger::…`.
pub mod pl;
pub mod plugins;
pub mod pool;
pub mod protocol;
pub mod recovery;
pub mod reset;
pub mod retry;
pub mod runtime;
pub mod scrub;
pub mod server;
pub mod shape;
pub mod shapes;
pub mod settings;
pub mod spawn;
pub mod startup;
pub mod store;
pub mod stream;
pub mod update;
/// The daemon's count of its OWN warnings, the `tracing` layer that keeps it,
/// and the warnings-only disk record under `~/.cauldron/threads.warnings/`.
pub mod warnings;

use std::net::SocketAddr;

pub use budgets::{BudgetConfig, Budgets, Thresholds, TripAction};
pub use config::Config;
pub use error::{Error, Result};
pub use event::{SendIntent, StoredEvent, ThreadEvent};
pub use runtime::ThreadManager;
pub use settings::ThreadSettings;
pub use store::Store;

/// Build a [`ThreadManager`] from a config: open the store + account registry
/// at the config's data dir, merge inline account seeds, apply `claude_bin`.
pub fn build_manager(config: Config) -> Result<ThreadManager> {
    build_manager_noticing(config, &|_| {})
}

/// [`build_manager`], telling `notice` before each store migration that has
/// real work to do (R9.C7h; see `Store::open_noticing` and `crate::startup`).
pub fn build_manager_noticing(config: Config, notice: &dyn Fn(&'static str)) -> Result<ThreadManager> {
    config.apply_claude_bin();
    let data_dir = config.resolved_data_dir()?;
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| Error::Config(format!("creating {}: {e}", data_dir.display())))?;
    let store = Store::open_noticing(data_dir.join("threads.db"), notice)?;
    let accounts_path = data_dir.join("accounts.toml");
    let mut registry = accounts::Registry::load(&accounts_path)?;
    // `registry.ensure_default()` USED TO RUN HERE, writing an `[accounts.default]`
    // row into `accounts.toml` on every boot. It is gone — 2026-08-27, on the
    // operator's ruling that the machine default is *"a completely separate
    // system, not even visible to placement"*. The daemon does not create that
    // slot, does not need it to exist, and no longer holds the concept; the
    // `claude-accounts` CLI owns the slot and creates it when he uses it. An
    // `accounts.toml` that already carries the row (his does) is left exactly
    // as it stands — nothing here reads it, so nothing has to remove it.
    let mut changed = false;
    for (name, dir) in &config.accounts {
        let cd = if dir.is_empty() { None } else { Some(dir.clone()) };
        // **A CONFIG SEED CANNOT REINTRODUCE THE MACHINE DEFAULT.**
        //
        // `config.toml` has shipped an inline `default = ""` seed since before
        // any of this — an empty string is exactly how that file spells "the
        // machine default" — and this loop would faithfully `add` it, flip
        // `changed`, and save the row back into `accounts.toml` on the next
        // boot after anyone removed it. That is the reappearance path the
        // sibling's state migration would have lost to, arriving from a
        // direction no call site audit would have found, because no call site
        // is wrong.
        //
        // `Registry::save` refuses to persist it anyway. This is the second
        // lock, not a substitute for the first: without it the daemon would
        // still hold the row in memory for the rest of the process, and the
        // pool derivation would be reasoning about a slot the operator has
        // said is not the daemon's to know.
        if name == accounts::DEFAULT_ACCOUNT && cd.is_none() {
            tracing::debug!(
                "ignoring the `default` account seed in config.toml — the machine default is \
                 not a registered account and the daemon holds no concept of one"
            );
            continue;
        }
        if registry.get(name).map(|a| a.config_dir.clone()) != Some(cd.clone()) {
            registry.add(name, cd);
            changed = true;
        }
    }
    if changed {
        let _ = registry.save(&accounts_path);
    }
    // THE POOL — `pool.json` when it exists, else derived from the registry,
    // which is today's behaviour exactly. See `pool.rs` for why an absent file
    // is not materialised.
    let pool = pool::Pool::load(&data_dir.join("pool.json"), &registry)?;
    tracing::info!(
        entries = pool.entries.len(),
        names = ?pool.names(),
        "pool"
    );
    Ok(ThreadManager::new(store, registry, pool, config, data_dir))
}

/// Run the daemon to completion (until Ctrl-C): TAKE THE PORT, then build,
/// SERVE, and RECOVER behind the served port (re-adopt turns, re-arm retries,
/// re-dispatch queues — the boot half of G1/G10/Q4; R9.C7g moved it behind
/// the serve, see `crate::recovery`).
pub async fn run_daemon(config: Config) -> Result<()> {
    let host = config.host.clone();
    // **ONE PORT, THE CHASSIS PORT (R2.S4).** The daemon used to bind the
    // legacy door (`config.port`, 3651) here and stand the chassis up beside it
    // on a second port that was "never allowed to fail the boot". The legacy
    // door is gone, so the chassis port is THE port: it is taken first, it is
    // the singleton, and failing to take it fails the boot exactly as the
    // legacy bind did. `?`: a `THREADS_CHASSIS_PORT` that is not a port fails
    // the boot rather than quietly taking the file value — see
    // `Config::effective_chassis_port`.
    let port = config.effective_chassis_port()?;
    if port == 0 {
        return Err(Error::Config(
            "chassis_port = 0 asks this daemon to serve nothing: the chassis door is the \
             only door it has (the legacy :3651 listener was retired at R2.S4). Set \
             `chassis_port` (default 3653) or THREADS_CHASSIS_PORT to a real port."
                .into(),
        ));
    }
    // ── THE PORT, BEFORE EVERYTHING ──────────────────────────────────────
    //
    // The bind is this crate's ONLY mutual exclusion between two daemon
    // processes — no pidfile, no advisory lock on `threads.db`, no `daemons`
    // table. So it has to be the first thing a daemon does, ahead of every
    // write a second daemon would also make.
    //
    // It has been moved up twice. The first move put it ahead of `recover()`,
    // because the loser of a two-daemon race was consuming the operator's
    // messages and marking real queue rows delivered before discovering it was
    // not supposed to exist. But `build_manager` and `update.on_boot()` were
    // still above it, and those are not passive either: `build_manager` opens
    // the LIVE `threads.db` read-write and runs `apply_schema()` and may
    // rewrite `accounts.toml`, and `on_boot()` clears the durable
    // queued-restart record, promotes `threads.last-good` and appends to
    // `restarts.jsonl`.
    //
    // That is not hypothetical. The daemon's own self-update `execv`s, leaving
    // the port unheld for ~25 ms; AUI's health probe lands in that gap and
    // spawns a second daemon, which loses. It has fired on 12 of the last 13
    // self-updates, and on 2026-08-20 the loser (pid 79138) and the incumbent
    // (pid 2082) appended their `boot` lines to `restarts.jsonl` at the same
    // instant and destroyed both of them.
    //
    // With the bind first, a losing daemon exits having touched nothing — no
    // store, no registry, no pending record, no last-good, no ledger line. The
    // race becomes harmless rather than merely rare.
    //
    // R9.C7g: serving NO LONGER waits for recovery. It used to — the listener
    // was HELD and not accepted on until `recover()` returned — and on
    // 2026-10-20 that was ~60 s of 252 children resuming with `/health`
    // unreachable. Recovery now runs behind a served port; see `crate::recovery`.
    //
    // Nothing that writes, anywhere, may move above this line.
    // `tests/daemon_race.rs::a_daemon_that_loses_the_port_race_writes_nothing`
    // is what says so.
    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .map_err(|e| Error::Config(format!("invalid bind addr {host}:{port}: {e}")))?;
    let listener = server::bind(addr).await?;
    let bound = listener
        .local_addr()
        .map_err(|e| Error::Config(format!("serving on {host}:{port}: {e}")))?;

    // ── R9.C7i: THE OFF-RUNTIME HEALTH DOOR, BOUND BEFORE ANYTHING SLOW ────
    //
    // AFTER the chassis bind (a daemon that loses the port race must not hold
    // the door either) and BEFORE the store opens, so the holder's probe gets
    // an answer from the first moment. A set `THREADS_HEALTH_ADDR` that cannot
    // be bound fails the boot. See `crate::health_door`.
    let door = match health_door::addr_from_env().map_err(Error::Config)? {
        Some(a) => {
            let d = health_door::Door::start(a, health_door::stall_from_env()).map_err(|e| {
                Error::Config(format!("binding the health door {a} ({}): {e}", health_door::ADDR_ENV))
            })?;
            tracing::info!(
                addr = %d.addr(),
                stall_after_secs = d.stall_after().as_secs(),
                "health door: /health answered off the runtime; 503 `stalled` once the runtime lag is past this"
            );
            Some(d)
        }
        None => None,
    };

    // ── R9.C7h: SERVE THE PORT NOW; OPEN (AND MIGRATE) THE STORE BEHIND IT ──
    //
    // C7g bound first and then ran `build_manager` before anything accepted
    // on the listener, so a store migration (R10.M1's queue backfill: ~72 s on
    // the 4.3 GB prod store, DEPLOY-9h) was 72 s of a bound port answering
    // nothing. The listener is served from HERE, by `crate::startup`'s router:
    // 503 `migrating` while a migration announced by `Store::open_noticing`
    // runs (bounded: `recovery_stuck` past `recovery::STUCK_ENV`), and
    // otherwise each request waits for the chassis and is handed to it. The
    // serve writes nothing, so the port-race invariant below still holds.
    let startup = std::sync::Arc::new(startup::Startup::new());
    // R9.C7i: the door's answer is computed on the runtime, every stamp:
    // `Startup`'s until the chassis is ready, then the chassis verdict.
    let door_ready: std::sync::Arc<std::sync::OnceLock<(liveness::Heartbeat, liveness::Extras)>> =
        std::sync::Arc::new(std::sync::OnceLock::new());
    let door_stamp = door.as_ref().map(|d| {
        let (st, ready) = (std::sync::Arc::clone(&startup), std::sync::Arc::clone(&door_ready));
        d.spawn_stamp(move || match ready.get() {
            Some((hb, extras)) => health_door::ready_answer(hb, extras()),
            None => st.health_answer(),
        })
    });
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let pre_chassis = startup.router();
    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, pre_chassis)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
        {
            tracing::error!(error = %e, daemon = server::DAEMON_NAME, "chassis server error");
        }
    });
    // Off the reactor: `Store::open` is synchronous SQLite and a migration can
    // hold it for a minute.
    let manager = {
        let st = std::sync::Arc::clone(&startup);
        tokio::task::spawn_blocking(move || build_manager_noticing(config, &|step| st.notice(step)))
            .await
            .map_err(|e| Error::Internal(format!("opening the store: {e}")))??
    };
    startup.opened();
    // ── THE CONSUMER LEDGER (2026-09-22) ─────────────────────────────────
    //
    // *"By default all chassis should do this"* — so it is wired here, with no
    // configuration and nothing to switch on, for every daemon that is going to
    // serve consumers.
    //
    // **HERE and not in `ThreadManager::new`.** A manager built by a CLI verb
    // or by a test serves nobody, and a producer with no consumers has no
    // consumer history to record; wiring it in the constructor would have every
    // `cargo test` in the crate appending to the operator's real ledger and
    // interleaving with every other test binary. `run_daemon` is the chassis.
    //
    // **AFTER the bind, like everything else that writes.** A daemon that loses
    // the port race must exit having touched nothing, and
    // `tests/daemon_race.rs::a_daemon_that_loses_the_port_race_writes_nothing`
    // says so. (In fact nothing is written even here: the file half is lazy and
    // creates neither directory nor file until a consumer actually subscribes.)
    if let Some(path) = consumers::producer_ledger_path(server::DAEMON_NAME) {
        manager.hub.consumers().attach(
            path,
            server::DAEMON_NAME,
            &manager.boot_id,
            env!("CARGO_PKG_VERSION"),
        );
    } else {
        tracing::warn!(
            "neither CAULDRON_HOME nor HOME is set; the consumer ledger has no file \
             half. `shapes/list` still reports who is subscribed — only the history \
             is not recorded"
        );
    }
    // THE UPDATE PROTOCOL, next (intent item 64). Before anything is served:
    // reconcile the durable pending-restart record against the image we just
    // booted from, and promote that image to last-good. `start_watchers` (from
    // `recover`/`serve`) arms the binary watch.
    manager.update.on_boot().await;
    // ── R9.C7g: RECOVERY IS MARKED, NOT AWAITED ───────────────────────────
    //
    // `begin` BEFORE the server is built, so the very first `/health` already
    // reads `recovering` and the very first request that needs recovery state
    // already waits for it. The recovery itself is spawned below, after the
    // serve task, so the port is answering before the first child resumes.
    manager.update.recovery().begin_after(startup.migrated_for());
    // Cheap handles kept back for the shutdown path: `Store` wraps an `Arc` and
    // `update` already IS one. `manager` itself moves into the server below.
    let store = manager.store.clone();
    let gate = std::sync::Arc::clone(&manager.update);
    // The drain door (R9.C7c) needs the manager after it moves into the server.
    let drain_door = spawn_drain_signals(manager.clone());
    let recovery_manager = manager.clone();
    // SH-A4: the attached-session pid reaper (`liveness::spawn_attach_reaper`).
    if let Some(period) = liveness::attach_reap_period_from_env() {
        let _reaper = liveness::spawn_attach_reaper(manager.clone(), period);
    }
    // R10.H2 (HEALTH F3): the subagent health invariant, every
    // `health_check::PERIOD`. Read-only over the store, the ledgers and the
    // process table; never the managed close lock.
    let health_task = health_check::spawn(manager.clone(), health_check::Health::new());

    // ── THE SERVER ────────────────────────────────────────────────────────
    //
    // The chassis over the manager, on the listener taken first. `ThreadManager`
    // is `Clone` over `Arc` fields — `hub`, `store`, `update` — so this is the
    // same hub, store and update gate everything else in this function holds.
    // ── R9.C6: `/health` REFLECTS STORE LIVENESS ──────────────────────────
    //
    // Shapewire's own `/health` answers from identity alone, so it stays 200
    // while every store-bound task is stuck behind the write mutex — the wedge
    // that has actually happened. The heartbeat pings the store off the
    // reactor; the layer answers 503 once the last good ping is older than its
    // bound. See `liveness`. Served here (rather than through
    // `Chassis::serve_on_with_shutdown`) only so the layer can wrap the router;
    // the serve itself is the same `axum::serve` with the same graceful stop.
    let heartbeat = liveness::Heartbeat::from_env();
    let pinged = store.clone();
    let heartbeat_task = liveness::spawn(heartbeat.clone(), liveness::HEARTBEAT_PERIOD, move || {
        pinged.ping().map_err(|e| e.to_string())
    });
    tracing::info!(
        stale_after_secs = heartbeat.stale_after().as_secs(),
        "liveness: /health answers 503 once the store heartbeat is older than this"
    );
    // R9.C7d: a stale `/health` still says whether turns are running and
    // whether a drain is under way, so the supervisor can drain before it
    // relaunches a wedged daemon.
    let gate_503 = manager.update.clone();
    let extras: liveness::Extras = std::sync::Arc::new(move || gate_503.health_extras());
    let _ = door_ready.set((heartbeat.clone(), std::sync::Arc::clone(&extras)));
    let app = liveness::layer(chassis::chassis(std::sync::Arc::new(manager)).into_router(), heartbeat, extras);
    // R9.C7h: the port has been served since the bind; from here the chassis
    // answers every request.
    startup.ready(app);
    tracing::info!("threads daemon listening on ws://{bound}/ws (the chassis door)");

    // ── R9.C7g: BOOT RECOVERY, BEHIND THE SERVED PORT ─────────────────────
    //
    // The same `recover()` as before, in the same order relative to everything
    // that writes (after the bind, after `on_boot`) — only no longer ahead of
    // the serve. It holds a REQUEST PERMIT for its whole run, so the update
    // gate's quiet condition (and cauldron's quiet re-pin, which reads
    // `restart_gate` off `/health`) treats a recovering daemon as busy: a
    // self-update or a re-pin never lands in the middle of a resume.
    let recovery_task = {
        let mgr = recovery_manager;
        // r37 (doctrine 10): the recovery runs in its OWN task so its panic
        // is seen here — a panicked recovery used to leave `/health` at 200
        // `recovering` forever. Now the process exits non-zero and the holder
        // relaunches it. A recovery that never returns is bounded on the
        // `/health` side instead (`recovery::STUCK_ENV` → 503).
        let work = {
            let mgr = mgr.clone();
            tokio::spawn(async move {
                let _busy = mgr.update.enter_request();
                crate::recovery::fault_seam().await;
                mgr.recover().await;
            })
        };
        let abort = work.abort_handle();
        let watcher = tokio::spawn(async move {
            let t0 = std::time::Instant::now();
            match work.await {
                Ok(()) => {
                    mgr.update.recovery().finish();
                    tracing::info!(
                        elapsed_ms = t0.elapsed().as_millis() as u64,
                        "boot recovery done; /health reports ok"
                    );
                }
                Err(e) if e.is_panic() => {
                    tracing::error!(
                        exit = crate::recovery::PANIC_EXIT_CODE,
                        "boot recovery PANICKED; exiting so the supervisor relaunches this daemon \
                         (recovery re-derives from durable state on the next boot)"
                    );
                    std::process::exit(crate::recovery::PANIC_EXIT_CODE);
                }
                Err(_) => {} // cancelled: the stop path aborted it
            }
        });
        (watcher, abort)
    };

    // Bound the WAL's high-water mark while we run, rather than only on the way
    // out. Started AFTER the port is held (and, since R9.C7g, while boot
    // recovery may still be running behind the served port), so it can never
    // be the thing a losing daemon does to a store it does not own.
    let wal_keeper = spawn_wal_keeper(store.clone());

    let first = wait_for_stop_signal(|| {
        if let Some(d) = &door {
            d.watch_sigterm();
        }
    })
    .await;
    if let Some(d) = &door {
        d.stop_heard();
    }
    tracing::info!(signal = %first, "stop requested — draining before exit");
    drain_door.abort();

    // Stop ACCEPTING. Connections already in flight are left to finish; this is
    // the whole difference between asking and killing.
    let _ = shutdown_tx.send(());

    let outcome = drain(&gate, SHUTDOWN_DRAIN_SECS).await;
    tracing::info!(outcome = %outcome, "drain complete");

    // The serve task resolves once the last in-flight connection is done. Bound
    // it: a wedged connection must not hold the port open forever, because the
    // supervisor's next rung is a signal and the one after that is a kill.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;

    // Stop the interval keeper before the shutdown checkpoint, so the two are
    // not both trying to fold the WAL as we exit. NOTE what this does and does
    // not do: `abort()` cancels the TASK, not the `spawn_blocking` closure
    // inside it, so a keeper checkpoint already in flight keeps running and
    // keeps holding the store mutex. That is one of the ways the shutdown
    // checkpoint below can find the primary connection busy, and it is why that
    // call has a SECOND DOOR rather than a longer wait.
    wal_keeper.abort();
    heartbeat_task.abort();
    health_task.abort();
    if let Some(t) = door_stamp {
        t.abort();
    }
    // A stop that lands mid-recovery: recovery is idempotent by design (every
    // pass re-derives from durable state), so the next boot finishes it.
    recovery_task.1.abort();
    recovery_task.0.abort();

    // BOUNDED, and off the reactor. The old call was `store.checkpoint()`,
    // which waits for the store mutex with no ceiling at all — so a wedged
    // store did not degrade the clean shutdown, it DEFEATED it, and the
    // supervisor's ladder ran all the way to SIGKILL with the WAL unfolded.
    // See `Store::checkpoint_for_shutdown` for the ladder, and for why the
    // bound is on WAITING rather than on the fold.
    let closing = store.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        closing.checkpoint_for_shutdown(std::time::Duration::from_secs(
            SHUTDOWN_CHECKPOINT_BUDGET_SECS,
        ))
    })
    .await;
    match outcome {
        Ok(store::ShutdownCheckpoint::Refused { waited, error }) => tracing::error!(
            waited_ms = waited.as_millis() as u64,
            %error,
            "store checkpoint found NO way in before its budget ran out; exiting anyway. \
             The next boot recovers the WAL, which is what WAL recovery is for — but a \
             store that could not be reached at shutdown is a store that was WEDGED at \
             shutdown, and that is the news here."
        ),
        Ok(ref o) if o.report().is_some_and(|r| r.fully_reclaimed()) => {
            let r = o.report().expect("matched on Some");
            tracing::info!(
                door = o.door(),
                waited_ms = o.waited().as_millis() as u64,
                log_frames = r.log_frames,
                wal_bytes_before = r.wal_bytes_before,
                "store checkpointed; WAL folded back and truncated"
            )
        }
        // A shutdown checkpoint that did NOT fully reclaim is the exact state
        // that used to leave an 87 MB WAL for the next boot to recover from.
        // It is not fatal — we still exit — but it is never again silent.
        Ok(ref o) => {
            let r = o.report().expect("Refused is handled above");
            tracing::warn!(
                door = o.door(),
                waited_ms = o.waited().as_millis() as u64,
                busy = r.busy,
                log_frames = r.log_frames,
                checkpointed_frames = r.checkpointed_frames,
                wal_bytes_after = r.wal_bytes_after,
                "store checkpoint did NOT fully reclaim the WAL; the next boot will \
                 recover from what is left"
            )
        }
        // The blocking pool is gone, which happens during shutdown. Nothing to
        // report and nothing to fix — and never a reason to refuse to exit: a
        // daemon that will not exit is a held port, which is the actual outage.
        Err(e) => tracing::error!(error = %e, "store checkpoint could not be run; exiting anyway"),
    }

    tracing::info!("threads daemon shut down cleanly");
    Ok(())
}

/// How long a clean shutdown will wait for in-flight work before giving up on
/// it. Must stay comfortably below the supervisor's own rung-2 wait, or the
/// supervisor escalates to a signal while the daemon is still draining and the
/// two mechanisms fight. `~/.threads-supervisor/supervise-service.sh` waits 60s.
const SHUTDOWN_DRAIN_SECS: u64 = 45;

/// How long the shutdown checkpoint will WAIT FOR A WAY IN before giving up.
///
/// It has to fit inside what is left of the supervisor's rung-2 wait after the
/// drain: `supervise-service.sh` waits 60s, [`SHUTDOWN_DRAIN_SECS`] takes up to
/// 45 of them and the bounded wait on the serve task takes up to 5 more, so 5s
/// here leaves 5s of margin. A larger number would not buy a better outcome —
/// the store either comes free in a moment or is genuinely wedged, and the
/// SECOND DOOR (a connection of the checkpoint's own) is the answer to the
/// second case, not a longer wait.
///
/// Half of it is spent on the write mutex and the rest becomes the second
/// connection's `busy_timeout`. It bounds WAITING, never the fold itself — see
/// [`Store::checkpoint_for_shutdown`].
const SHUTDOWN_CHECKPOINT_BUDGET_SECS: u64 = 5;

/// How often the WAL keeper folds the write-ahead log back and truncates it.
///
/// Five minutes is chosen against the cost of the operation, not against a
/// growth rate: at the steady-state size (~4 MB, which is `wal_autocheckpoint`
/// at 1000 pages) a TRUNCATE is milliseconds, so running it often is cheap and
/// running it rarely is what let 87 MB accumulate.
const WAL_CHECKPOINT_INTERVAL_SECS: u64 = 300;

/// Below this, there is nothing worth taking the store mutex for.
///
/// Auto-checkpoint already holds the steady state near 4 MB. The keeper exists
/// for the file that has grown PAST that and would otherwise stay grown, so it
/// skips the ordinary case entirely rather than adding a log line every five
/// minutes that says nothing.
const WAL_CHECKPOINT_FLOOR_BYTES: u64 = 8 * 1024 * 1024;

/// A WAL still above this AFTER a truncate is a real problem, not a blip.
///
/// It means something is holding a read snapshot across checkpoints. Logged at
/// WARN with the frame counts, because the whole lesson of the 87 MB episode is
/// that a control which refuses quietly is a control nobody can act on.
const WAL_ALARM_BYTES: u64 = 32 * 1024 * 1024;

/// Fold the WAL back on an interval, and say so when it does not work.
///
/// **Why this exists.** `Store::checkpoint` ran only on the way out of
/// [`run_daemon`], so the `-wal` file's high-water mark was permanent for the
/// daemon's uptime. SQLite's own auto-checkpoint keeps the FRAME COUNT near
/// `wal_autocheckpoint` but resets the write cursor in place — it never shrinks
/// the file. One long transaction is therefore enough to inflate the file for
/// days, and every WAL-index operation pays for the size.
///
/// On 2026-09-11 that cost three days of growth to 87 MB, a health endpoint at
/// 2.1s against tts's 0.03s, and finally a daemon too wedged to answer the
/// SIGTERM whose checkpoint would have fixed it — so the supervisor escalated
/// to a kill, the checkpoint never ran, and the oversized WAL survived into the
/// next boot. The loop below is the part that stops that from being silent.
pub fn spawn_wal_keeper(store: Store) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let period = std::time::Duration::from_secs(WAL_CHECKPOINT_INTERVAL_SECS);
        loop {
            tokio::time::sleep(period).await;

            // `checkpoint` is blocking and takes the store mutex; keep it off
            // the async worker so a large fold cannot stall the reactor.
            let store = store.clone();
            let report =
                match tokio::task::spawn_blocking(move || {
                    if store.wal_bytes() < WAL_CHECKPOINT_FLOOR_BYTES {
                        return Ok(None);
                    }
                    store.checkpoint().map(Some)
                })
                .await
                {
                    Ok(r) => r,
                    // The blocking pool is gone, which happens during shutdown.
                    // Nothing to report and nothing to fix.
                    Err(_) => return,
                };

            match report {
                Ok(None) => {}
                Ok(Some(r)) if r.fully_reclaimed() && r.wal_bytes_after < WAL_ALARM_BYTES => {
                    tracing::info!(
                        log_frames = r.log_frames,
                        wal_bytes_before = r.wal_bytes_before,
                        wal_bytes_after = r.wal_bytes_after,
                        "wal keeper: folded the WAL back and truncated it"
                    );
                }
                Ok(Some(r)) => tracing::warn!(
                    busy = r.busy,
                    log_frames = r.log_frames,
                    checkpointed_frames = r.checkpointed_frames,
                    wal_bytes_before = r.wal_bytes_before,
                    wal_bytes_after = r.wal_bytes_after,
                    "wal keeper: the WAL did not fully reclaim — something is \
                     holding a read snapshot across checkpoints. This is how the \
                     file grows without bound and the daemon slows down."
                ),
                Err(e) => tracing::warn!(error = %e, "wal keeper: checkpoint failed"),
            }
        }
    })
}

/// **THE DRAIN DOOR** (R9.C7c, gap C-7): `SIGUSR1` begins a drain, `SIGUSR2`
/// cancels it. See [`ThreadManager::set_draining`].
///
/// A signal rather than a WS method, because the asker is the supervisor that
/// launched this process and already signals it by pid to stop it; it holds no
/// socket client and needs no answer — what the drain is doing is read back off
/// `/health` (`restart_gate.draining_since`, `restart_gate.turns`). Both are
/// idempotent, so the supervisor may repeat one every pass.
///
/// **The danger, and why it cannot be reached:** the default disposition of
/// `SIGUSR1` is to TERMINATE. A build without this door would die of the
/// request instead of draining. The supervisor therefore sends it only when the
/// RUNNING build's declaration declares `[quiet.drain]` AND that process's own
/// `/health` carries `restart_gate.draining_since` — two witnesses that this
/// door exists (`cli/src/daemon.rs`, `read_gate`).
fn spawn_drain_signals(manager: ThreadManager) -> tokio::task::JoinHandle<()> {
    use tokio::signal::unix::{signal, SignalKind};
    tokio::spawn(async move {
        let (mut on, mut off) = match (
            signal(SignalKind::user_defined1()),
            signal(SignalKind::user_defined2()),
        ) {
            (Ok(a), Ok(b)) => (a, b),
            (a, b) => {
                tracing::error!(
                    usr1 = a.is_ok(),
                    usr2 = b.is_ok(),
                    "could not install the drain door's signal handlers; a re-pin cannot drain \
                     this process"
                );
                return;
            }
        };
        loop {
            tokio::select! {
                Some(()) = on.recv() => { manager.set_draining(true); }
                Some(()) = off.recv() => { manager.set_draining(false); }
                else => return,
            }
        }
    })
}

/// Block until someone asks this process to stop.
///
/// **Before 2026-09-02 this was `ctrl_c()` alone, which on Unix is SIGINT and
/// ONLY SIGINT.** Nothing sends this daemon a SIGINT — it runs under launchd,
/// supervised by a bash script, and every stop that has ever actually happened
/// to it arrived as SIGTERM. So the one shutdown path the daemon had was
/// unreachable in production, and SIGTERM fell through to its default
/// disposition: immediate termination, no unwinding, no `Drop`, no checkpoint.
/// The supervisor's "terminate it politely" rung was therefore identical in
/// effect to its "kill it" rung, and the ladder between them was decorative.
///
/// R9.C7i: `after_install` runs once tokio's SIGTERM handler is installed, so
/// a second watcher (the health door's) never replaces the default disposition
/// while nothing on the runtime is listening.
async fn wait_for_stop_signal(after_install: impl FnOnce()) -> &'static str {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            after_install();
            tokio::select! {
                _ = term.recv() => "SIGTERM",
                _ = tokio::signal::ctrl_c() => "SIGINT",
            }
        }
        Err(e) => {
            // Registration can only fail on a broken runtime. Degrade to the
            // old behaviour rather than refusing to serve.
            tracing::error!(error = %e, "could not install a SIGTERM handler; SIGINT only");
            let _ = tokio::signal::ctrl_c().await;
            "SIGINT"
        }
    }
}

/// Wait for the daemon to reach the same quiet condition the self-update path
/// requires, or for a second stop signal, or for the deadline.
///
/// The predicate is deliberately [`update::Blockers::quiet`] and not "no turns
/// running". A turn SURVIVES this exit — it has its own process group, its own
/// stdout file, a durable `running_turns` row, and `recover()` adopts it on the
/// far side. Waiting for turns to reach zero is waiting for a fleet-wide idle
/// that measured 266 minutes on this daemon, so it would convert every clean
/// shutdown into a timeout and hand the decision to the supervisor's kill rung.
/// `spawning` is the state that genuinely cannot survive, and it is the state
/// this waits on.
///
/// A SECOND signal abandons the wait. That is what makes the supervisor's rung 3
/// distinct from its rung 2 without a second mechanism: the same signal, sent
/// twice, means "stop waiting for the work and close the database now".
async fn drain(gate: &update::UpdateGate, secs: u64) -> String {
    use tokio::signal::unix::{signal, SignalKind};
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut second = signal(SignalKind::terminate()).ok();

    loop {
        let blockers = gate.peek();
        if blockers.quiet() {
            return format!("quiet ({})", blockers.describe());
        }
        if tokio::time::Instant::now() >= deadline {
            return format!("TIMED OUT after {secs}s still busy ({})", blockers.describe());
        }
        let tick = tokio::time::sleep(std::time::Duration::from_millis(100));
        match second.as_mut() {
            Some(sig) => {
                tokio::select! {
                    _ = tick => {}
                    _ = sig.recv() => {
                        return format!(
                            "ABANDONED on a second signal, still busy ({})",
                            blockers.describe()
                        );
                    }
                }
            }
            None => tick.await,
        }
    }
}
