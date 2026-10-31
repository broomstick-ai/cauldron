//! **R6.S3 — environments you can actually USE side by side.**
//!
//! R6.S1 built the mechanism (a data root, a seam table, a hold and a launchd
//! label per environment). Doing the operator's sentence end to end on a
//! throwaway environment beside the live `prod` found what was still missing,
//! and this file is where those pieces live:
//!
//! * **`cauldron env create <name> --from <env>`** — SEED an environment from
//!   another: its pins (versions only), and with them a copy of every pinned
//!   service's data directory. The only sanctioned way to copy an
//!   environment's data. Refused while the source is held or anything in it
//!   is live, because a copy of a store a process is writing is a torn copy.
//! * **`cauldron env remove <name> [--stop] [--yes]`** — the teardown. Dry run
//!   by default; refuses `prod` by name, refuses an environment installed
//!   under launchd (KeepAlive would revive what it stops), and with `--stop`
//!   stops the holder and every service before it deletes the data root.
//! * **`cauldron pin <service> <version> --addr <host:port>`** — the per
//!   environment address of a HOSTED service, which R6.S1 could only take by
//!   hand-editing `env.toml`.
//! * **The hosted-port collision guard** ([`refuse_addr_collision`]): a hosted
//!   service's port is declared, not kernel-assigned, and the declaration is
//!   shared by every environment in a root. Before R6.S3 the only defence was
//!   `launch_hosted`'s pre-fork connect, which fires at launch time, in a
//!   daemon's log, and only while the other environment happens to be up. Now
//!   `pin` and `launch_hosted` both refuse an address any other environment in
//!   this root claims for any hosted service — prod included.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use env::decl::{self, Manifest};
use env::seams::{self, Table};
use env::Environment;

use env::supervisions;

use crate::{daemon, launchd};

// ═══════════════════════════════════════════════ HOSTED ADDRESS COLLISIONS ══

/// The port of `host:port`. Ports are compared and hosts are not: every
/// hosted service on this machine binds loopback, and `127.0.0.1:3653` vs
/// `0.0.0.0:3653` is a collision the kernel would also refuse.
fn port_of(addr: &str) -> Option<u16> {
    addr.rsplit(':').next()?.parse().ok()
}

/// **Every hosted address an environment CLAIMS**: `(service, addr)` for each
/// service it pins that does not register (so its port is declared), at the
/// environment's override if it has one and the declaration's `[launch] addr`
/// otherwise. Read from the shipped/override declaration — the address a
/// snapshot carries is the same `[launch] addr` key and an override wins over
/// both.
pub(crate) fn claimed_addrs(e: &Environment, m: &Manifest) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for svc in m.services.keys() {
        let Ok(d) = decl::load(&e.service_dir(), svc) else { continue };
        if d.registers {
            continue;
        }
        let addr = m.hosted_addr(svc).or_else(|| d.launch.as_ref().map(|l| l.addr.clone()));
        if let Some(a) = addr {
            out.push((svc.clone(), a));
        }
    }
    out
}

/// **REFUSE a hosted address that another environment in this root claims**,
/// or that another service in this one claims.
///
/// Two sources are read for every other environment: what its `env.toml`
/// claims ([`claimed_addrs`]) and what its seam table says is registered
/// there right now — so an address is refused both when the other
/// environment is down (its claim would collide the moment it came up) and
/// when something is up on it that no pin explains.
pub(crate) fn refuse_addr_collision(here: &Environment, service: &str, addr: &str) -> Result<()> {
    let want = port_of(addr).with_context(|| format!("`{addr}` is not a host:port address"))?;
    let mut names = here.environments()?;
    if !names.iter().any(|n| n == &here.name) {
        names.push(here.name.clone());
    }
    for name in names {
        // An environment that cannot be resolved (an aliased data root, say)
        // is refused by its own resolution; it claims nothing we can read.
        let Ok(e) = here.for_name(&name) else { continue };
        let same_env = e.name == here.name;
        let m = Manifest::load(&e.manifest()).unwrap_or_default();
        for (svc, a) in claimed_addrs(&e, &m) {
            if same_env && svc == service {
                continue;
            }
            if port_of(&a) == Some(want) {
                bail!(
                    "REFUSED: port {want} for hosted service `{service}` in environment `{}` \
                     COLLIDES with `{svc}` in environment `{}`, which claims {a}{}. A hosted \
                     service's port is declared, not kernel-assigned, and two processes on one \
                     port is one of them failing to bind — or, worse, the supervisor of one \
                     environment probing the other's process and calling it its own. Give this \
                     environment its own address: `cauldron --env {} pin {service} <version> \
                     --addr 127.0.0.1:<free port>`.",
                    here.name,
                    e.name,
                    if m.hosted_addr(&svc).is_some() {
                        " (its env.toml override)"
                    } else {
                        " (the declaration's [launch] addr, which it does not override)"
                    },
                    here.name
                );
            }
        }
        if same_env {
            continue;
        }
        if let Ok(t) = Table::load(&e.name, &e.data_root, &e.seams()) {
            for s in t.seams.values() {
                if port_of(&s.addr) == Some(want) {
                    bail!(
                        "REFUSED: port {want} for hosted service `{service}` in environment \
                         `{}` is REGISTERED in environment `{}` right now, by `{}` (pid {}, \
                         {}). Pick another port with `--addr`.",
                        here.name,
                        e.name,
                        s.service,
                        s.pid,
                        s.addr
                    );
                }
            }
        }
    }
    Ok(())
}

/// A port the kernel says is free right now. Used to give a SEEDED
/// environment's hosted services their own addresses, so `env create --from`
/// produces an environment that can start rather than one that collides with
/// its source on its first launch.
fn free_loopback_addr() -> Result<String> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").context("asking the kernel for a free port")?;
    Ok(l.local_addr()?.to_string())
}

/// **The ports the operator's PROD faces dial.** 3653 is threads' chassis
/// (its declared default), 3651 its retired legacy door, 3753 the address the
/// operator's own prod pins threads at, and 9010 ui-server's `/health`. A
/// port chosen AUTOMATICALLY for any environment (REL-4.S8b) is never one of
/// these, whatever the kernel hands out — an environment that took one would,
/// the first time prod's process was down, answer prod's faces from its own
/// (empty) store.
pub(crate) const PROD_FACE_PORTS: [u16; 4] = [3651, 3653, 3753, 9010];

/// **`cauldron pin <service> <version> --addr auto`** (REL-4.S8b): the
/// address a hosted service gets when nobody typed one, by the rule `env
/// create --from` already uses for a seeded environment — a port the kernel
/// says is free, checked against every claim in the root.
///
/// * An environment named `prod` keeps the DECLARED address (`Ok(None)`: no
///   override is written), but only if that port is free right now and no
///   other environment claims it.
/// * Every other environment — and a prod whose declared port is taken —
///   gets its own port (`Ok(Some(addr))`), which is never the declared port,
///   never one of [`PROD_FACE_PORTS`] and never a port another environment
///   claims.
///
/// The three questions are parameters so the rule can be forced both ways
/// without binding a real prod port: `port_free(p)` (can this process bind
/// it), `claimed(p)` (does any environment in the root claim it) and
/// `kernel_port()` (a port the kernel hands out).
pub(crate) fn auto_hosted_addr(
    env_name: &str,
    declared: Option<&str>,
    port_free: &dyn Fn(u16) -> bool,
    claimed: &dyn Fn(u16) -> bool,
    kernel_port: &mut dyn FnMut() -> Result<u16>,
) -> Result<Option<String>> {
    let declared_port = declared.and_then(port_of);
    if env_name == "prod" {
        if let Some(p) = declared_port {
            if port_free(p) && !claimed(p) {
                return Ok(None);
            }
        }
    }
    for _ in 0..64 {
        let p = kernel_port()?;
        if Some(p) == declared_port || PROD_FACE_PORTS.contains(&p) || claimed(p) {
            continue;
        }
        return Ok(Some(format!("127.0.0.1:{p}")));
    }
    bail!(
        "REFUSED: 64 ports from the kernel were all either the declared port, one of prod's \
         face ports ({PROD_FACE_PORTS:?}) or claimed by another environment; pass an explicit \
         `--addr 127.0.0.1:<free port>`"
    )
}

/// [`auto_hosted_addr`] against the real machine: free means BOTH sockets a
/// hosted daemon binds on that number (`127.0.0.1` and, for threads' health
/// door, `[::1]`) can be bound now; claimed is [`refuse_addr_collision`].
pub(crate) fn auto_hosted_addr_here(
    env: &Environment,
    service: &str,
    declared: Option<&str>,
) -> Result<Option<String>> {
    let port_free = |p: u16| {
        std::net::TcpListener::bind(("127.0.0.1", p)).is_ok()
            && std::net::TcpListener::bind(("::1", p)).is_ok()
    };
    let claimed = |p: u16| refuse_addr_collision(env, service, &format!("127.0.0.1:{p}")).is_err();
    let mut kernel = || -> Result<u16> {
        let a = free_loopback_addr()?;
        port_of(&a).with_context(|| format!("the kernel's free address `{a}` has no port"))
    };
    auto_hosted_addr(&env.name, declared, &port_free, &claimed, &mut kernel)
}

#[cfg(test)]
mod auto_addr_tests {
    use super::{auto_hosted_addr, PROD_FACE_PORTS};
    use anyhow::Result;

    /// A kernel that hands out `seq` in order.
    fn kernel(seq: Vec<u16>) -> impl FnMut() -> Result<u16> {
        let mut it = seq.into_iter();
        move || Ok(it.next().expect("the test's kernel ran out of ports"))
    }

    const DECL: Option<&str> = Some("127.0.0.1:3653");

    /// A NON-prod environment never gets the declared port — not even when it
    /// is free and nobody claims it, which is exactly the state a stranger's
    /// machine is in before prod is up.
    #[test]
    fn non_prod_never_gets_the_declared_port_even_when_free() {
        let mut k = kernel(vec![50001]);
        let got = auto_hosted_addr("dev", DECL, &|_| true, &|_| false, &mut k).unwrap();
        assert_eq!(got.as_deref(), Some("127.0.0.1:50001"));
    }

    /// Opposing arm: prod with its declared port free and unclaimed keeps the
    /// default (no override written).
    #[test]
    fn prod_keeps_the_default_only_when_it_is_free() {
        let mut k = kernel(vec![50002]);
        assert_eq!(auto_hosted_addr("prod", DECL, &|_| true, &|_| false, &mut k).unwrap(), None);
        // busy -> its own port
        let mut k = kernel(vec![50003]);
        let got = auto_hosted_addr("prod", DECL, &|p| p != 3653, &|_| false, &mut k).unwrap();
        assert_eq!(got.as_deref(), Some("127.0.0.1:50003"));
        // free but claimed by another environment -> its own port
        let mut k = kernel(vec![50004]);
        let got = auto_hosted_addr("prod", DECL, &|_| true, &|p| p == 3653, &mut k).unwrap();
        assert_eq!(got.as_deref(), Some("127.0.0.1:50004"));
    }

    /// The kernel is not trusted: a port it hands out that is the declared
    /// one, one of prod's face ports, or claimed elsewhere is skipped.
    #[test]
    fn a_kernel_port_that_is_prods_or_claimed_is_skipped() {
        let mut seq: Vec<u16> = vec![3653];
        seq.extend(PROD_FACE_PORTS);
        seq.extend([50010, 50011]);
        let mut k = kernel(seq);
        let got = auto_hosted_addr("dev", DECL, &|_| true, &|p| p == 50010, &mut k).unwrap();
        assert_eq!(got.as_deref(), Some("127.0.0.1:50011"));
    }
}

// ═════════════════════════════════════════════════════════════ env create ══

pub(crate) fn verb_env_create(args: &[String]) -> Result<i32> {
    let mut name: Option<String> = None;
    let mut from: Option<String> = None;
    let mut auto_restart = false;
    let mut pins_only = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--auto-restart" => auto_restart = true,
            "--pins-only" => pins_only = true,
            "--from" => {
                i += 1;
                from = Some(args.get(i).cloned().context("`--from` needs an environment name")?);
            }
            s if s.starts_with("--from=") => from = Some(s["--from=".len()..].to_string()),
            s if s.starts_with('-') => bail!("unknown option `{s}` for `cauldron env create`"),
            s if name.is_none() => name = Some(s.to_string()),
            s => bail!("`cauldron env create` takes ONE name (got a second: `{s}`)"),
        }
        i += 1;
    }
    let name = name.context("`cauldron env create <name> [--from <env>]` needs a name")?;
    if pins_only && from.is_none() {
        bail!("`--pins-only` only means something with `--from <env>`");
    }
    let here = Environment::resolve()?;
    let e = here.for_name(&name)?;
    // R9.C7f7: a directory a failed create left behind (only the writers'
    // lock file and an empty `logs/`, see `undo_create`) is not an
    // environment, so it does not block creating one.
    if std::fs::symlink_metadata(&e.data_root).is_ok() && !vacant(&e) {
        bail!(
            "REFUSED: environment `{name}` already exists at {}. Creating it again would \
             rewrite its env.toml — its pins and its supervision policy. `cauldron env show \
             {name}` says what is there.",
            e.data_root.display()
        );
    }
    create_test_window();

    // ── THE SOURCE, CHECKED BEFORE ANYTHING IS WRITTEN ─────────────────────
    let source = match &from {
        None => None,
        Some(f) => {
            let src = here.for_name(f)?;
            if src.name == e.name {
                bail!("REFUSED: an environment cannot be seeded from itself");
            }
            if !src.data_root.is_dir() {
                bail!("REFUSED: `--from {f}`: there is no environment `{f}` ({} does not exist)", src.data_root.display());
            }
            let m = Manifest::load(&src.manifest())?;
            if !pins_only {
                refuse_live_source(&src, &m)?;
            }
            Some((src, m))
        }
    };

    e.ensure_dirs()?;
    // R9.C7f6 (C-7, inspect R9.C7f5 F2, ledger R9.C7f5-2): the create is one
    // critical section with every other env.toml writer. The `data_root`
    // check above is unlocked, so a `cauldron pin` (whose `ensure_manifest`
    // creates the environment) or a second `env create` of the same name can
    // land between it and here. Under the writers' lock the existence of
    // env.toml is re-checked, and if another writer created it this create
    // REFUSES and leaves the directory alone: it is not ours any more, and
    // removing it would destroy that writer's env.toml. Held until the save,
    // so the data copy is inside it too: a `pin` into this name waits for the
    // create (it is a CLI verb and blocks), and the holder's bounded
    // `lock_within` writes fail "busy" and retry on its next pass, unchanged.
    // The lock is blocking: `env create` is an operator verb he can interrupt.
    let _held = Manifest::lock(&e.manifest())?;
    if std::fs::symlink_metadata(e.manifest()).is_ok() {
        bail!(
            "REFUSED: environment `{name}` was created by another writer while this `env create` \
             was starting ({} now exists). Nothing was written and its directory was left as \
             that writer made it. `cauldron env show {name}` says what is there.",
            e.manifest().display()
        );
    }
    // What THIS create wrote, so a failure removes exactly that and nothing
    // another writer put in the directory (R9.C7f7).
    let mut wrote: Vec<PathBuf> = Vec::new();
    let result = (|| -> Result<Manifest> {
        create_locked_test_window();
        let mut m = Manifest::default();
        m.supervisor.auto_restart = auto_restart;
        if let Some((src, sm)) = &source {
            // VERSIONS ONLY. `good` is per environment by definition (it is
            // what ran HERE), and an address override is the one thing that
            // must NOT be copied — it would collide with the source.
            for (svc, p) in &sm.services {
                m.set_version(svc, &p.version);
            }
            for (svc, _) in claimed_addrs(src, sm) {
                let a = free_loopback_addr()?;
                refuse_addr_collision(&e, &svc, &a)?;
                if let Some(p) = m.services.get_mut(&svc) {
                    p.addr = Some(a);
                }
            }
            if !pins_only {
                for svc in sm.services.keys() {
                    let from_dir = src.service_data(svc)?;
                    if !from_dir.exists() {
                        continue;
                    }
                    let to_dir = e.service_data(svc)?;
                    if std::fs::symlink_metadata(&to_dir).is_err() {
                        wrote.push(to_dir.clone());
                    }
                    let n = copy_tree(&from_dir, &to_dir)
                        .with_context(|| format!("copying `{svc}`'s data from `{}`", src.name))?;
                    println!(
                        "cauldron: copied `{svc}`'s data from `{}` ({n} files): {} -> {}",
                        src.name,
                        from_dir.display(),
                        to_dir.display()
                    );
                }
            }
        }
        // REL-3.S1: the CURATED environment. `cauldron-build` is seeded with
        // the soup crate's dataset (cauldron's own development, scrubbed) into
        // the face's own-seed dir, the path the face reads any cauldron's data
        // through. Every other name starts empty: no other environment ever
        // shows seed data (`soup::demo::seed_env` refuses them). Inside the
        // critical section and recorded in `wrote`, so a failed create
        // removes a half-written seed with everything else it wrote.
        if e.name == ::soup::demo::ENV {
            let face_dir = e.service_data(::soup::demo::OWNER)?;
            if std::fs::symlink_metadata(&face_dir).is_err() {
                wrote.push(face_dir.clone());
            }
            let seeded = ::soup::demo::seed_env(&e).context("seeding the curated environment")?;
            println!(
                "cauldron: seeded `{}` with the curated dataset ({} files): {}",
                e.name,
                seeded.wrote.len(),
                seeded.seed_dir.display()
            );
        }
        m.save(&e.manifest())?;
        Ok(m)
    })();
    let m = match result {
        Ok(m) => m,
        Err(err) => {
            // A half-seeded environment is removed rather than left to be
            // mistaken for a seeded one — STILL UNDER THE LOCK, and only what
            // this create wrote. See `undo_create` for why not the directory.
            undo_create(&e, &wrote);
            drop(_held);
            return Err(err).context(format!("environment `{name}` was NOT created"));
        }
    };
    println!(
        "created environment `{name}`{}",
        match &source {
            Some((s, _)) if pins_only => format!(" (pins from `{}`, no data)", s.name),
            Some((s, _)) => format!(" (pins and data from `{}`)", s.name),
            None => String::new(),
        }
    );
    crate::print_env(&e)?;
    let hosted: Vec<String> = m
        .services
        .iter()
        .filter_map(|(k, p)| p.addr.as_ref().map(|a| format!("{k} @{a}")))
        .collect();
    if !hosted.is_empty() {
        println!(
            "  hosted ports   {} (fresh, kernel-free when created; change with `pin --addr`)",
            hosted.join("  ")
        );
    }
    println!();
    println!(
        "next: pin services into it (`cauldron --env {name} pin <service> <version>`), then \
         hold it (`cauldron --env {name} daemon`). It shares this root's builds and \
         declarations, and nothing else."
    );
    Ok(0)
}

/// **R9.C7f7 (inspect R9.C7f6 finding 1): a failed create removes what IT
/// wrote, under the lock, and never the writers' lock file.**
///
/// It used to `remove_dir_all(data_root)` while holding the lock on
/// `data_root/.env.toml.lock`. That unlinked the very inode it held locked. A
/// writer already blocked on that lock (a `pin` whose `ensure_manifest` made
/// the directory, a second `env create`) holds an fd on that inode, so when
/// this process exited it was handed a lock on an ORPHAN. Any later writer
/// made the directory again and locked a FRESH inode, and two processes held
/// "the" lock at once. The second hazard was the same call: the directory is
/// not only ours once the lock is taken (a `pin` waiting on us made it; a
/// holder may have put `supervisor.lock` and `logs/` there), so removing all
/// of it removed other writers' files.
///
/// THE CHOICE: keep the lock file's inode, rather than release first and
/// unlink after. A release-then-unlink is the same race one step later (the
/// waiter acquires the still-linked inode, then the unlink orphans it under
/// it) and is only safe if every locker re-validates the path's inode after
/// acquiring and retries, which would change every env.toml writer in the
/// root to make this one rare failure path tidy. Keeping the inode needs no
/// other writer to change: the lock file is never removed, which is already
/// `ManifestLock`'s stated invariant, and this create now keeps it too. The
/// cost is a directory that outlives a failed create, holding only the lock
/// file and an empty `logs/`; [`vacant`] makes that not count as an
/// environment, so creating it again works as it did.
///
/// Removed: env.toml (only if a failing `save` somehow left it — the atomic
/// save normally leaves none), each service data directory this create made
/// by copying, and `logs/` if it is still empty. Nothing else.
fn undo_create(e: &Environment, wrote: &[PathBuf]) {
    for d in wrote {
        let _ = std::fs::remove_dir_all(d);
    }
    let _ = std::fs::remove_file(e.manifest());
    let _ = std::fs::remove_dir(e.data_root.join("logs"));
}

/// A data root holding nothing but the env.toml writers' lock file and an
/// empty `logs/` — what a failed `env create` leaves — is not an environment.
/// One predicate, shared with `cauldron envs` ([`env::paths::vacant_data_root`]).
fn vacant(e: &Environment) -> bool {
    ::env::paths::vacant_data_root(&e.data_root)
}

/// R9.C7f7 test hook: `CAULDRON_TEST_CREATE_LOCKED_MS` holds `env create`
/// INSIDE the writers' lock, before it writes anything, and says so on
/// stderr, so a test can block a second writer on that lock. Unset (always,
/// outside tests) it does nothing.
fn create_locked_test_window() {
    if let Some(ms) = std::env::var("CAULDRON_TEST_CREATE_LOCKED_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
        eprintln!("cauldron(test): env create holds the env.toml lock for {ms}ms");
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// R9.C7f6 test hook: `CAULDRON_TEST_CREATE_WINDOW_MS` holds `env create`
/// between its unlocked existence check and taking the env.toml writers'
/// lock, and says on stderr that it is in the window, so a test can land a
/// concurrent `pin` or `env create` exactly there. Unset (always, outside
/// tests) it does nothing.
fn create_test_window() {
    if let Some(ms) = std::env::var("CAULDRON_TEST_CREATE_WINDOW_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
        eprintln!("cauldron(test): env create window open for {ms}ms");
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// A seed copy from an environment something is writing is a torn copy. So:
/// not while it is HELD (a holder may launch a service mid-copy) and not while
/// any of its rows is live.
fn refuse_live_source(src: &Environment, _m: &Manifest) -> Result<()> {
    let t = Table::load(&src.name, &src.data_root, &src.seams())?;
    let live: Vec<String> = t
        .seams
        .values()
        .filter(|s| !seams::probe(s).process_gone(s))
        .map(|s| format!("`{}` (pid {}, {})", s.service, s.pid, s.addr))
        .collect();
    if !live.is_empty() {
        bail!(
            "REFUSED to copy `{}`'s data: it is RUNNING — {}. A copy of a store a live process \
             is writing (sqlite with a WAL, a turns directory mid-append) is a torn copy that \
             opens fine and is wrong. Stop it first (`cauldron env remove` does not apply; \
             stop its holder and `cauldron --env {} stop`), or seed pins only with \
             `--pins-only`.",
            src.name,
            live.join(", "),
            src.name
        );
    }
    Ok(())
}

/// Copy a directory tree, refusing symlinks (a link copied as a link keeps
/// pointing into the SOURCE environment, which is the alias this whole step
/// refuses). Files go through `std::fs::copy`, which clones on APFS.
fn copy_tree(from: &Path, to: &Path) -> Result<u64> {
    let mut n = 0;
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("listing {}", from.display()))? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if ft.is_symlink() {
            bail!(
                "REFUSED: {} is a symlink. Copied as a link it would still point into the \
                 source environment; copied through, it would copy whatever it points at. \
                 Neither is a seed.",
                src.display()
            );
        } else if ft.is_dir() {
            n += copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)
                .with_context(|| format!("copying {} -> {}", src.display(), dst.display()))?;
            n += 1;
        }
    }
    Ok(n)
}

// ═════════════════════════════════════════════════════════════ env remove ══

/// How long `env remove --stop` waits for a signalled holder to let go.
const HOLDER_GONE_WAIT: Duration = Duration::from_secs(10);

pub(crate) fn verb_env_remove(args: &[String]) -> Result<i32> {
    let mut name: Option<&str> = None;
    let mut stop = false;
    let mut yes = false;
    for a in args {
        match a.as_str() {
            "--stop" => stop = true,
            "--yes" => yes = true,
            s if s.starts_with('-') => bail!("unknown option `{s}` for `cauldron env remove`"),
            s if name.is_none() => name = Some(s),
            s => bail!("`cauldron env remove` takes ONE name (got a second: `{s}`)"),
        }
    }
    let name = name.context("`cauldron env remove <name> [--stop] [--yes]` needs a name")?;
    // BY NAME, FIRST, BEFORE ANY RESOLUTION. Prod is never removed by a
    // command; its data is the operator's, and deleting it is an `rm -rf`
    // he types himself if he ever means it.
    if name == env::alias::PROD {
        bail!(
            "REFUSED: `prod` is never removed by cauldron. Its data root is the operator's live \
             store."
        );
    }
    let here = Environment::resolve()?;
    let e = here.for_name(name)?;
    let meta = std::fs::symlink_metadata(&e.data_root);
    match meta {
        Err(_) => bail!("REFUSED: there is no environment `{name}` ({} does not exist)", e.data_root.display()),
        Ok(m) if !m.is_dir() => bail!("REFUSED: {} is not a directory", e.data_root.display()),
        Ok(_) => {}
    }
    // Resolution already refused a data root that resolves anywhere but
    // `<root>/data/<name>`; this restates it at the point of deletion.
    let expected = here.root.join("data").join(name);
    if e.data_root != expected {
        bail!("REFUSED: {} is not {}", e.data_root.display(), expected.display());
    }
    if let Ok(p) = launchd::plist_path(&e) {
        if p.exists() {
            bail!(
                "REFUSED: environment `{name}` is installed under launchd ({}). Its job is \
                 KeepAlive, so a stopped holder would be revived mid-removal. Run `cauldron \
                 --env {name} uninstall` first.",
                p.display()
            );
        }
    }

    println!("{}", e.banner());
    let table = Table::load(&e.name, &e.data_root, &e.seams())?;
    let live: Vec<&seams::Seam> =
        table.seams.values().filter(|s| !seams::probe(s).process_gone(s)).collect();
    if !yes {
        println!("cauldron: DRY RUN — would remove environment `{name}`:");
        println!("  data root      {} (deleted, with everything in it)", e.data_root.display());
        for s in &live {
            println!(
                "  live           `{}` pid {} at {}{}",
                s.service,
                s.pid,
                s.addr,
                if stop { " (would be stopped)" } else { " (REFUSES removal without --stop)" }
            );
        }
        println!("  untouched      builds, declarations and every other environment");
        println!("cauldron: re-run with `--yes` to do it.");
        return Ok(2);
    }
    if !live.is_empty() && !stop {
        let names: Vec<String> =
            live.iter().map(|s| format!("`{}` (pid {}, {})", s.service, s.pid, s.addr)).collect();
        bail!(
            "REFUSED: environment `{name}` is RUNNING — {}. Removing a data root under live \
             processes leaves them writing to deleted files. Add `--stop` to stop them first.",
            names.join(", ")
        );
    }

    let hold = stop_everything(&e, &table, "cauldron env remove")
        .with_context(|| format!("REFUSED to remove `{name}`"))?;
    let gone: PathBuf = e.data_root.clone();
    std::fs::remove_dir_all(&gone).with_context(|| format!("deleting {}", gone.display()))?;
    drop(hold);
    println!("cauldron: removed environment `{name}` ({} deleted)", gone.display());
    Ok(0)
}

// ══════════════════════════════════════════════════ the one orderly stop ══

/// **Stop an environment's holder and then every service in it, and come
/// back HOLDING it** — the sequence `env remove --stop` and `down` share.
///
/// 1. The holder first, so it cannot relaunch what step 3 stops: TERM, then
///    KILL after [`HOLDER_GONE_WAIT`]. A holder does NOT stop its hosted
///    services on TERM (they survive a holder restart by design: a
///    self-updating holder exits and its successor re-attaches them), which is
///    why this function exists at all — R6.S4 F2 measured a TERMed holder's
///    threads orphaned and still serving.
/// 2. Take the hold, so no new holder can start in it while it is stopped.
/// 3. Every live service, through the one stop path (`stop_by_seam`, the
///    RUNNING version's own signal and escalation).
/// 4. Re-read, re-probe: nothing live may remain, or it is an error.
///
/// The returned hold is the caller's to drop (`down`) or to delete under
/// (`env remove`).
///
/// C-1 (R9.C1 inspect F2): "running" here, and in `down`, `env remove` and the
/// seed-copy refusal, is [`seams::Verdict::process_gone`] — for a row cauldron
/// owns, its PID, not its port. `threads` closes its port and then drains for
/// up to 45 s on its data root; reading the quiet port as "not running" made
/// `down` skip it and `env remove --yes` delete the data root under it.
pub(crate) fn stop_everything(e: &Environment, table: &Table, who: &str) -> Result<env::hold::Hold> {
    let name = &e.name;
    // ── 1. THE HOLDER FIRST, so it cannot relaunch what step 3 stops. ─────
    if let Some(s) = table.seams.get(daemon::SELF_SERVICE) {
        if !seams::probe(s).process_gone(s) {
            if let Some(why) = s.refusal_to_signal() {
                bail!("will not signal the holder of `{name}`: {why}");
            }
            println!("cauldron: stopping the holder of `{name}` (pid {}, {})", s.pid, s.addr);
            crate::signal(s.pid, "TERM");
            if !crate::wait_gone(s, HOLDER_GONE_WAIT) {
                crate::signal(s.pid, "KILL");
                crate::wait_gone(s, Duration::from_secs(3));
            }
        }
    }
    // ── 2. TAKE THE HOLD, so nothing can start in it meanwhile. ──────────
    let note = env::hold::note_for(&e.name, who, crate::SELF_COMMIT);
    let deadline = Instant::now() + HOLDER_GONE_WAIT;
    let hold = loop {
        match env::hold::try_hold(&e.data_root, note.clone())? {
            env::hold::Attempt::Held(h) => break h,
            env::hold::Attempt::Taken { path, note } => {
                if Instant::now() > deadline {
                    bail!("{}", env::hold::refusal(&e.name, &path, note.as_ref()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };
    // ── 3. EVERY SERVICE, through the one stop path. ─────────────────────
    let table = Table::load(&e.name, &e.data_root, &e.seams())?;
    for s in table.seams.values() {
        if s.service == daemon::SELF_SERVICE {
            continue;
        }
        if seams::probe(s).process_gone(s) {
            continue;
        }
        let (sig, esc) = crate::stop_terms(e, s);
        crate::stop_by_seam(e, s, &sig, esc, supervisions::Why::REQUESTED)
            .with_context(|| format!("`{}` would not stop", s.service))?;
    }
    // ── 4. NOTHING LIVE MAY REMAIN. Re-read, re-probe. ──────────────────
    let table = Table::load(&e.name, &e.data_root, &e.seams())?;
    let still: Vec<String> = table
        .seams
        .values()
        .filter(|s| s.service != daemon::SELF_SERVICE && !seams::probe(s).process_gone(s))
        .map(|s| format!("`{}` (pid {})", s.service, s.pid))
        .collect();
    if !still.is_empty() {
        bail!("still running after stop: {}", still.join(", "));
    }
    Ok(hold)
}

// ═══════════════════════════════════════════════════════════════════ down ══

/// **`cauldron [--env <e>] down [--supervised]`** (R6.S5, R6.S4 finding F2):
/// stop this environment's holder and every service it hosts, and leave
/// everything else — the data root, `env.toml`, the declarations, the builds
/// — exactly where it was. The environment is still listed by `envs`, and
/// `cauldron --env <e> daemon` brings it back.
///
/// **Refused for a SUPERVISED environment** — `prod` by name, or any
/// environment whose launchd plist is installed — unless `--supervised` is
/// given. Under launchd with KeepAlive the holder is revived at once, so
/// `down` there is a restart of every service rather than a stop, and on prod
/// it is an outage of the operator's live store. For a lasting stop of an
/// installed environment, `cauldron --env <e> uninstall` first.
pub(crate) fn verb_down(args: &[String]) -> Result<i32> {
    let mut supervised_ok = false;
    for a in args {
        match a.as_str() {
            "--supervised" => supervised_ok = true,
            s if s.starts_with('-') => bail!("unknown option `{s}` for `cauldron down` (only `--supervised`)"),
            s => bail!(
                "`cauldron down` takes no service (got `{s}`): it stops the whole environment. \
                 To stop one service, `cauldron stop {s}`."
            ),
        }
    }
    let e = Environment::resolve()?;
    println!("{}", e.banner());
    match std::fs::symlink_metadata(&e.data_root) {
        Ok(m) if m.is_dir() => {}
        _ => bail!("REFUSED: there is no environment `{}` ({} does not exist)", e.name, e.data_root.display()),
    }
    let plist = launchd::plist_path(&e).ok().filter(|p| p.exists());
    let supervised = e.name == env::alias::PROD || plist.is_some();
    if supervised && !supervised_ok {
        let why = match &plist {
            Some(p) => format!("it is installed under launchd ({})", p.display()),
            None => "it is `prod`, the operator's live environment".to_string(),
        };
        bail!(
            "REFUSED: `cauldron down` will not stop environment `{}`: {why}. Under launchd \
             KeepAlive revives the holder at once, so this would restart every service rather \
             than stop it. For a lasting stop run `cauldron --env {} uninstall` first; to do it \
             anyway, add `--supervised`.",
            e.name,
            e.name
        );
    }
    if let Some(p) = &plist {
        println!(
            "cauldron: WARNING: `{}` is installed under launchd ({}); launchd may revive its \
             holder, which relaunches what it pins.",
            e.name,
            p.display()
        );
    }
    let table = Table::load(&e.name, &e.data_root, &e.seams())?;
    let live: Vec<&seams::Seam> =
        table.seams.values().filter(|s| !seams::probe(s).process_gone(s)).collect();
    if live.is_empty() {
        println!("cauldron: nothing is running in `{}`; nothing to stop", e.name);
        return Ok(0);
    }
    let hold = stop_everything(&e, &table, "cauldron down")
        .with_context(|| format!("REFUSED: `cauldron down` could not stop `{}`", e.name))?;
    // The dead holder's own row would otherwise stay in the table (and in
    // `envs`) naming a pid that is gone; the next holder writes a fresh one.
    crate::clear(&e, daemon::SELF_SERVICE)?;
    drop(hold);
    println!(
        "cauldron: `{}` is down (data, pins and declarations kept in {}); `cauldron --env {} \
         daemon` brings it back",
        e.name,
        e.data_root.display(),
        e.name
    );
    Ok(0)
}
