//! # `cauldron_env.rs` — **WHICH THREADS THIS FACE READS, ASKED OF CAULDRON.**
//!
//! R7.S1c. The face used to read threads on the seam's compiled-in default,
//! `127.0.0.1:3653`, which is production's chassis. Cauldron now has
//! ENVIRONMENTS — several cauldrons side by side, each with its own data root,
//! pins and seam table — so "the threads" is a question with one answer PER
//! ENVIRONMENT, and the face asks it of the environment it was launched in.
//!
//! ## The order, and why each step is where it is
//!
//! 1. **`TUI_THREADS_CHASSIS_PORT`** — the operator's (or a test's) explicit
//!    lever. It wins over everything, exactly as it did before; nothing below
//!    runs.
//! 2. **The environment** — `--env` on the `cauldron` router (which exports
//!    `CAULDRON_ENV`), else `CAULDRON_ENV`, else `ENV`, else `prod`; the root is
//!    `CAULDRON_HOME`, else `~/.cauldron`. Resolved by the `env` crate, the SAME
//!    code every cauldron verb uses, so the face and `cauldron start` cannot
//!    disagree about which environment they are in.
//! 3. **The threads declaration**, `<root>/services/threads.toml` then the
//!    shipped one — the same search `cauldron` does.
//!    - A service that REGISTERS binds `127.0.0.1:0` and publishes the address
//!      in the environment's SEAM TABLE, so the seam row is the only answer.
//!    - A HOSTED service (threads today, `registers = false`) is launched on a
//!      declared address: the environment's own `[services.threads] addr` in
//!      `data/<env>/env.toml` if it names one, else the declaration's
//!      `[launch] addr`. That is exactly the address `launch_hosted` hands the
//!      child, so the face looks where the supervisor will put it — including
//!      after a restart, when a stale seam row would still name the old pid.
//!
//! ## The one refusal
//!
//! **A non-`prod` environment that resolves to the address `prod` resolves to
//! in the same root is refused.** A declaration is shared by every environment
//! in a root, so an environment that never set its own `addr` inherits prod's
//! `127.0.0.1:3653` — and a face opened "in a test environment" would show the
//! operator's live ledger, and could send into it. The refusal names the line
//! that fixes it. The explicit lever (step 1) is not refused: that is a person
//! saying which port they mean.

use std::path::{Path, PathBuf};

use env::decl::{self, Manifest};
use env::seams::Table;
use env::Environment;

/// The service this face reads.
pub const SERVICE: &str = "threads";

/// Where a resolved address came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `TUI_THREADS_CHASSIS_PORT` was set.
    Override,
    /// `[services.threads] addr` in this environment's `env.toml`.
    EnvManifest(PathBuf),
    /// The seam table row of a REGISTERING threads.
    Seam(PathBuf),
    /// `[launch] addr` of the declaration at this path.
    Declaration(PathBuf),
}

impl Source {
    /// Short words for the down page and `--resolve`.
    pub fn describe(&self) -> String {
        match self {
            Source::Override => "TUI_THREADS_CHASSIS_PORT".into(),
            Source::EnvManifest(p) => format!("env.toml {}", p.display()),
            Source::Seam(p) => format!("seam table {}", p.display()),
            Source::Declaration(p) => format!("declaration {}", p.display()),
        }
    }
    /// One word, for a machine reading `--resolve`.
    pub fn tag(&self) -> &'static str {
        match self {
            Source::Override => "override",
            Source::EnvManifest(_) => "env.toml",
            Source::Seam(_) => "seam",
            Source::Declaration(_) => "declaration",
        }
    }
}

/// The answer: which environment, and where its threads is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The environment name (`prod` by default).
    pub env: String,
    /// The cauldron root.
    pub root: PathBuf,
    /// `host:port` of the threads chassis.
    pub addr: String,
    pub port: u16,
    pub source: Source,
    /// `[supervisor] auto_restart` of this environment's `env.toml`, as read
    /// at startup — whether a holder of this environment relaunches threads
    /// when it dies. `None` when it could not be read (or for the override).
    pub auto_restart: Option<bool>,
}

impl Resolved {
    /// `key=value` words, one line: what `tui --resolve` prints.
    pub fn line(&self) -> String {
        format!(
            "env={} root={} addr={} port={} source={}",
            self.env,
            self.root.display(),
            self.addr,
            self.port,
            self.source.tag()
        )
    }
}

fn port_of(addr: &str) -> Result<u16, String> {
    addr.rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
        .filter(|p| *p != 0)
        .ok_or_else(|| format!("`{addr}` is not a host:port with a non-zero port"))
}

/// The explicit lever, if set. A value that is set but is not a usable port is
/// REFUSED rather than skipped: skipping it would quietly hand the face the
/// environment's port while its author believes they pointed it elsewhere.
fn override_port(raw: Option<String>) -> Result<Option<u16>, String> {
    match raw.filter(|v| !v.trim().is_empty()) {
        None => Ok(None),
        Some(v) => v
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .map(Some)
            .ok_or_else(|| format!("TUI_THREADS_CHASSIS_PORT={v:?} is not a port (1-65535)")),
    }
}

/// **Resolve from the process environment.** See the module doc for the order.
pub fn resolve() -> Result<Resolved, String> {
    resolve_with(std::env::var("TUI_THREADS_CHASSIS_PORT").ok())
}

/// [`resolve`] with the explicit lever's value HANDED IN rather than read from
/// the process environment (TUI-P1a, inspect F-1). `TUI_THREADS_CHASSIS_PORT`
/// is ALSO the variable every test seam in this binary is pointed by
/// (`mirror::spawn_dead`/`spawn_at`, under `mirror::TEST_PORTS`), and the
/// seam re-reads it on every reconnect. The tests of this module used to
/// `remove_var` and `set_var` it under a DIFFERENT lock, so a concurrent
/// `spawn_dead` could write `1` between their write and their read (measured:
/// `port 1, source Override`), and a reconnecting test seam could read it
/// UNSET and dial the default, 3653, his live daemon. So these tests no
/// longer touch it at all: the value under test is an argument.
pub fn resolve_with(override_raw: Option<String>) -> Result<Resolved, String> {
    let env = Environment::resolve().map_err(|e| format!("{e:#}"))?;
    // REL-6a4: an isolated process never takes his store as its root.
    if let Some(why) = crate::cauldron::live::isolated_root_refusal(&env.root) {
        return Err(why);
    }
    if let Some(port) = override_port(override_raw)? {
        return Ok(Resolved {
            env: env.name.clone(),
            root: env.root.clone(),
            addr: format!("127.0.0.1:{port}"),
            port,
            source: Source::Override,
            auto_restart: Manifest::load(&env.manifest()).ok().map(|m| m.supervisor.auto_restart),
        });
    }
    let r = resolve_in(&env)?;
    if env.name != "prod" {
        // The collision check, asked of the same root's `prod`. Only when it
        // resolves at all: a root with no prod declaration has nothing to
        // collide with.
        if let Ok(prod) = env.for_name("prod").map_err(|e| format!("{e:#}")).and_then(|p| resolve_in(&p)) {
            if prod.addr == r.addr {
                return Err(format!(
                    "REFUSED: environment `{}` resolves threads to {} ({}), which is the SAME \
                     address `prod` resolves to in {}. A face opened in `{}` would read — and \
                     could send into — prod's threads.\n  \
                     Give `{}` its own address: `cauldron --env {} pin threads <version> \
                     --addr 127.0.0.1:<port>` (it writes [services.threads] addr into {}), or \
                     point this one face explicitly with TUI_THREADS_CHASSIS_PORT=<port>.",
                    env.name,
                    r.addr,
                    r.source.describe(),
                    env.root.display(),
                    env.name,
                    env.name,
                    env.name,
                    env.manifest().display()
                ));
            }
        }
    }
    Ok(r)
}

/// Resolve inside one environment, without the override and without the
/// collision check.
pub fn resolve_in(env: &Environment) -> Result<Resolved, String> {
    let done = |addr: String, source: Source| -> Result<Resolved, String> {
        let port = port_of(&addr)?;
        Ok(Resolved {
            env: env.name.clone(),
            root: env.root.clone(),
            addr,
            port,
            source,
            auto_restart: Manifest::load(&env.manifest()).ok().map(|m| m.supervisor.auto_restart),
        })
    };
    let d = decl::load(&env.service_dir(), SERVICE).map_err(|e| {
        format!(
            "cannot resolve `{SERVICE}` in environment `{}`: {e:#}\n  \
             Point this face explicitly with TUI_THREADS_CHASSIS_PORT=<port>.",
            env.name
        )
    })?;
    if d.registers {
        let seams = env.seams();
        let t = Table::load(&env.name, &env.data_root, &seams).map_err(|e| format!("{e:#}"))?;
        return match t.seams.get(SERVICE) {
            Some(s) => done(s.addr.clone(), Source::Seam(seams)),
            None => Err(format!(
                "`{SERVICE}` registers its own address in environment `{}`, and its seam table \
                 {} has no row for it yet — it has not been started there. Start it with \
                 `cauldron --env {} start {SERVICE}`.",
                env.name,
                seams.display(),
                env.name
            )),
        };
    }
    let manifest = env.manifest();
    if let Some(a) = Manifest::load(&manifest).map_err(|e| format!("{e:#}"))?.hosted_addr(SERVICE) {
        return done(a, Source::EnvManifest(manifest));
    }
    match &d.launch {
        Some(l) => done(l.addr.clone(), Source::Declaration(d.loaded_from.clone())),
        None => Err(format!(
            "the `{SERVICE}` declaration {} has neither `registers = true` nor a `[launch] addr`",
            d.loaded_from.display()
        )),
    }
}

/// **Which `cauldron` the restart button runs.** `CAULDRON_BIN` (the router
/// exports its own path when it execs this face), else a `cauldron` beside
/// this binary (the install layout: `~/.cauldron/bin/{cauldron,tui}`, and
/// `target/<profile>/` in a build), else `cauldron` on PATH.
pub fn cauldron_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("CAULDRON_BIN").filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    if let Some(dir) = std::env::current_exe().ok().as_deref().and_then(Path::parent) {
        let beside = dir.join("cauldron");
        if beside.exists() {
            return beside;
        }
    }
    PathBuf::from("cauldron")
}

/// The restart command's arguments: `--env <name> start threads`. The
/// environment is passed by flag, not left to inheritance, so the button
/// cannot start threads in a different environment from the one on the page.
pub fn restart_args(env: &str) -> Vec<String> {
    vec!["--env".into(), env.into(), "start".into(), SERVICE.into()]
}

/// The resolution this process made at startup, for the down page.
static RESOLVED: std::sync::OnceLock<Resolved> = std::sync::OnceLock::new();

/// Record the startup resolution. First call wins.
pub fn set_resolved(r: Resolved) {
    let _ = RESOLVED.set(r);
}

/// The startup resolution, if `main` made one (tests and examples do not).
pub fn resolved() -> Option<&'static Resolved> {
    RESOLVED.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Environment-variable tests share one process; serialise them.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Root(PathBuf);
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn root(tag: &str) -> Root {
        let p = std::env::temp_dir().join(format!(
            "tui-cauldron-env-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(p.join("services")).unwrap();
        // A hosted threads declaration in the ROOT, so the test does not
        // depend on whatever the shipped file says today.
        std::fs::write(
            p.join("services").join("threads.toml"),
            "name = \"threads\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n\
             [launch]\naddr = \"127.0.0.1:3653\"\n\
             [build]\nartifact = \"/usr/bin/true\"\nbin = \"true\"\n",
        )
        .unwrap();
        Root(p)
    }

    fn env_toml(root: &Path, env: &str, body: &str) {
        let d = root.join("data").join(env);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("env.toml"), body).unwrap();
    }

    fn with_env<T>(root: &Path, env: &str, f: impl FnOnce() -> T) -> T {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // NOT `TUI_THREADS_CHASSIS_PORT`: see `resolve_with`. The lever is
        // passed to `resolve_with` by the tests that exercise it.
        std::env::set_var("CAULDRON_HOME", root);
        std::env::set_var("CAULDRON_ENV", env);
        let out = f();
        std::env::remove_var("CAULDRON_HOME");
        std::env::remove_var("CAULDRON_ENV");
        out
    }

    /// **AN ENVIRONMENT'S OWN ADDRESS IS WHAT THE FACE READS**, and the
    /// opposing case in the same root: with no `addr` of its own, prod reads
    /// the declaration's. A constant answer fails one of the two.
    #[test]
    fn an_environment_with_its_own_addr_resolves_to_it_and_prod_to_the_declaration() {
        let r = root("own");
        env_toml(&r.0, "t1", "[services.threads]\nversion = \"latest\"\naddr = \"127.0.0.1:47653\"\n");
        let t1 = with_env(&r.0, "t1", || resolve_with(None)).unwrap();
        assert_eq!((t1.env.as_str(), t1.port), ("t1", 47653), "{t1:?}");
        assert_eq!(t1.source.tag(), "env.toml");
        let prod = with_env(&r.0, "prod", || resolve_with(None)).unwrap();
        assert_eq!((prod.env.as_str(), prod.port), ("prod", 3653), "{prod:?}");
        assert_eq!(prod.source.tag(), "declaration");
    }

    /// **TWO ENVIRONMENTS, TWO ADDRESSES** — neither leaks into the other.
    #[test]
    fn two_environments_resolve_to_their_own_ports() {
        let r = root("two");
        env_toml(&r.0, "a", "[services.threads]\nversion = \"latest\"\naddr = \"127.0.0.1:47001\"\n");
        env_toml(&r.0, "b", "[services.threads]\nversion = \"latest\"\naddr = \"127.0.0.1:47002\"\n");
        let a = with_env(&r.0, "a", || resolve_with(None)).unwrap();
        let b = with_env(&r.0, "b", || resolve_with(None)).unwrap();
        assert_eq!((a.port, b.port), (47001, 47002));
    }

    /// **NEVER PROD'S PORT FROM A TEST ENVIRONMENT.** An environment that set
    /// no address of its own inherits prod's from the shared declaration, and
    /// is refused, by a message that names the line that fixes it.
    #[test]
    fn a_non_prod_environment_on_prods_address_is_refused() {
        let r = root("collide");
        env_toml(&r.0, "t2", "[services.threads]\nversion = \"latest\"\n");
        let err = with_env(&r.0, "t2", || resolve_with(None)).unwrap_err();
        assert!(err.contains("REFUSED") && err.contains("SAME"), "{err}");
        assert!(err.contains("[services.threads]"), "the refusal must name the fix: {err}");
    }

    /// **THE SEAM'S PORT LEVER DOES NOT REACH THESE TESTS** (TUI-P1a, inspect
    /// F-1). A test seam (`mirror::spawn_dead`) points `TUI_THREADS_CHASSIS_PORT`
    /// at `1` for the whole process; these tests ran under a DIFFERENT lock and
    /// read that variable, so whichever ran second resolved `port 1, Override`
    /// — 2 of 2 inspector runs red. Here the lever IS set to `1` (asserted, the
    /// witness: set by a test seam) and N = 2 environments still resolve to their own addresses.
    ///
    /// FORCED (report): put `resolve` back in place of `resolve_with(None)` in
    /// this test → both resolve `1`/`override`, red, every run.
    #[tokio::test]
    async fn the_seams_port_lever_does_not_reach_the_resolution_under_test() {
        let (_h, _rx) = crate::mirror::spawn_dead();
        let r = root("lever-free");
        env_toml(&r.0, "t1", "[services.threads]\nversion = \"latest\"\naddr = \"127.0.0.1:47653\"\n");
        for (env, want, tag) in [("t1", 47653u16, "env.toml"), ("prod", 3653, "declaration")] {
            let got = with_env(&r.0, env, || {
                // Held so no other test seam re-points the lever mid-read.
                let _ports = crate::mirror::TEST_PORTS.lock().unwrap_or_else(|e| e.into_inner());
                assert!(
                    std::env::var("TUI_THREADS_CHASSIS_PORT").is_ok(),
                    "witness: the seam's lever is set while this resolves"
                );
                resolve_with(None)
            })
            .unwrap();
            assert_eq!((got.port, got.source.tag()), (want, tag), "{env}: {got:?}");
        }
    }

    /// **THE EXPLICIT LEVER STILL WINS**, over an environment that names its
    /// own address — so a test pointing one face at a scratch port is obeyed.
    #[test]
    fn the_explicit_port_wins_over_the_environment() {
        let r = root("lever");
        env_toml(&r.0, "t3", "[services.threads]\nversion = \"latest\"\naddr = \"127.0.0.1:47003\"\n");
        let got = with_env(&r.0, "t3", || resolve_with(Some("47999".into()))).unwrap();
        assert_eq!((got.port, got.source.tag()), (47999, "override"));
    }

    /// **A REGISTERING THREADS IS READ FROM THE SEAM TABLE ONLY**: with no row,
    /// it is not guessed from anything else.
    #[test]
    fn a_registering_threads_with_no_seam_row_is_not_guessed() {
        let r = root("reg");
        std::fs::write(
            r.0.join("services").join("threads.toml"),
            "name = \"threads\"\nkind = \"daemon\"\nregisters = true\n\
             [build]\nartifact = \"/usr/bin/true\"\nbin = \"true\"\n",
        )
        .unwrap();
        env_toml(&r.0, "prod", "");
        let err = with_env(&r.0, "prod", || resolve_with(None)).unwrap_err();
        assert!(err.contains("has no row"), "{err}");
    }

    #[test]
    fn the_restart_command_names_the_environment() {
        assert_eq!(restart_args("t1"), ["--env", "t1", "start", "threads"]);
    }
}
