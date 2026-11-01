//! **THE CAULDRON FACE'S ONE CONNECTION TO THE THREADS DAEMON** (REL-6).
//!
//! the release plan (REL-6): *"Threads are pinned or recent per environment,
//! with no Projects grouping. Reuse the `tui --chat` client code."* So this
//! module is the chat face's client, reused whole: the seam
//! ([`crate::mirror::spawn`] — `seam::link` learns `shapes.list` first, holds
//! the book, resubscribes on every connect), the chat face's
//! [`crate::store::Store`] fed by the same `Store::apply`, the transcript path
//! ([`crate::mirror::spawn_transcript`]: `Seam::watch_log`, `open_transcript`,
//! `fill_bodies`) and the one mutation ([`crate::sends::spawn_send`]).
//!
//! **ONE per face process.** Every tab that reads the daemon (Threads now;
//! Tokens and Accounts next, V1.5 and V1.6) reads THIS — there is no second
//! socket, no second mirror and no second store.
//!
//! **Nothing here runs on the render thread.** The seam and its tasks run on
//! a runtime this module owns; what they learn reaches the face as [`Ev`]s on
//! one channel, which [`Live::drain`] empties with `try_recv` — it never waits.
//!
//! **The daemon down (consumer §9, TC-47).** The chat face's store drops every
//! current-state fact on a disconnect (`Store::invalidate`), which blanks the
//! rows the daemon decorated. The skill names that the defect to remove; this
//! face does not repeat it: a `Disconnected` sets the link state and the
//! outage clock and keeps every fact, so what the mirror last said stays
//! drawn, marked stale by the tab, and a send is refused behind
//! [`SEND_BLOCKED`] until the link is back. Nor does the link coming back
//! (`Invalidate`): the fresh export after the reconnect replaces each row it
//! carries (rule 2.5).

use std::collections::HashMap;

use chrono::Utc;
use tokio::sync::mpsc;
use tui_render::LinkState;

use crate::event::Ev;
use crate::mirror::{self, Handle};
use crate::store::Store;
use crate::transport::TransportEvent;

/// The legend on the message box while the link is down (consumer §9, the
/// operator's words: "can't send, threads is down").
pub const SEND_BLOCKED: &str = "can't send — threads is down";

/// Ports a pointed (test) connection refuses: his live daemons and the AUI.
/// One list for the whole workspace: `env::egress` owns it (REL-6a5).
pub const PROD_PORTS: [u16; 4] = env::egress::PROD_PORTS;

/// How many events one [`Live::drain`] applies before the face is owed a
/// frame — the chat face's `PAINT_BATCH`.
pub const DRAIN_BATCH: usize = 256;

/// Why an App with no [`Door`] has no connection.
pub const NO_DOOR: &str = "not connected: no threads endpoint was chosen for this face (only the face's own launch chooses one)";

/// **THE TEST-ONLY ISOLATION MARKER** (REL-6a2). Every test harness, drive
/// script and headless proof sets `CAULDRON_ISOLATED=1`; the operator never
/// does. It is its own variable so that nothing he sets for its own reason —
/// `CAULDRON_OPENER=none`, `CAULDRON_TYPE_ONE=off` — can ever disable his
/// Threads tab (REL-6ab-I F2): those are NOT isolation signals any more.
pub const ISOLATED_ENV: &str = env::egress::ISOLATED_ENV;

/// **WHY THIS FACE IS NOT HIS INTERACTIVE FACE**, if it is not:
/// - a `--dump` (headless);
/// - a unit test (`cfg!(test)`);
/// - a cargo TEST HARNESS process — an integration-test binary such as `it`,
///   which calls `cli::main` in-process (REL-6ab-I B1). Structural: decided
///   by what the process IS ([`exe_is_test_harness`]), so an in-process test
///   that forgets the marker is isolated anyway;
/// - the marker [`ISOLATED_ENV`]`=1`.
///
/// `None` means the interactive face.
pub fn isolation(headless: bool) -> Option<String> {
    if let Some(mode) = non_interactive() {
        return Some(format!("a non-interactive `{mode}`"));
    }
    let harness = std::env::current_exe().is_ok_and(|p| exe_is_test_harness(&p));
    isolation_from(headless, cfg!(test), harness, |k| std::env::var(k).ok())
}

/// The non-interactive mode this process was launched in, once its launch
/// said so ([`mark_non_interactive`]).
static NON_INTERACTIVE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// **RULE A + RULE B, THE PROCESS HALF** (REL-6a3). A launch that is not his
/// interactive face — `--headless`, `pool --dump`, `--cauldron --dump`, any
/// face started with no terminal — says so here, ONCE, before it builds a
/// runtime. From then on:
/// - [`isolation`] answers `Some` for the whole process, so EVERY connection
///   constructor (`mirror::spawn`, `Live::start`, `pool::client::spawn`,
///   `aui::spawn` and the AUI's per-attempt dial) refuses his live ports
///   ([`PROD_PORTS`]) — and an unpointed constructor IS on one of them (the
///   seam's default is 3653, the AUI's 9010);
/// - every child it spawns inherits `CAULDRON_ISOLATED=1` ([`export_marker`]).
pub fn mark_non_interactive(mode: &str) {
    let _ = NON_INTERACTIVE.set(mode.to_string());
    export_marker();
}

/// [`mark_non_interactive`]'s mode, if this process has one.
pub fn non_interactive() -> Option<&'static str> {
    NON_INTERACTIVE.get().map(String::as_str)
}

/// **RULE B (REL-6a3): AN ISOLATED PROCESS'S CHILDREN ARE ISOLATED.** Sets
/// `CAULDRON_ISOLATED=1` in this process's environment, which every child
/// spawned afterwards inherits (unless its spawner clears it — and the only
/// test spawner of the face, `tests/it/face.rs`, forces it back). Called by
/// [`mark_non_interactive`]. (A cargo TEST binary exports the same marker at
/// load, through `testkit/test-isolation`.) Never called for his interactive
/// face.
pub fn export_marker() {
    if std::env::var(ISOLATED_ENV).ok().as_deref() != Some("1") {
        std::env::set_var(ISOLATED_ENV, "1");
    }
}

/// **RULE A (REL-6a3): A NON-INTERACTIVE MODE IS GIVEN ITS ENDPOINT.** `explicit`
/// is `TUI_THREADS_CHASSIS_PORT` as the launch set it, read BEFORE anything
/// resolves the environment into it. Unset, junk, `0`, or one of his live
/// ports is refused, naming `mode`, and the caller exits non-zero without
/// dialling. Pointed at a test daemon, the port is returned.
pub fn require_explicit_endpoint(mode: &str, explicit: Option<&str>) -> Result<u16, String> {
    let v = explicit.map(str::trim).filter(|v| !v.is_empty()).ok_or_else(|| {
        format!(
            "refused: `{mode}` is non-interactive, and a non-interactive face is never left to find a daemon on its \
             own. Point it at a test daemon with TUI_THREADS_CHASSIS_PORT=<port> (never 3651/3653/3753/9010)"
        )
    })?;
    let port = match v.parse::<u16>() {
        Ok(p) if p > 0 => p,
        _ => return Err(format!("refused: `{mode}`: TUI_THREADS_CHASSIS_PORT=`{v}` is not a port")),
    };
    if PROD_PORTS.contains(&port) {
        return Err(format!(
            "refused: `{mode}` is non-interactive and port {port} is a live daemon of his (3651/3653/3753/9010)"
        ));
    }
    Ok(port)
}

/// The port a dial of `url` reaches, as the dialler reads it: parsed by
/// `env::egress::target_of` (a real URL parser), so `?query`, `#fragment`,
/// userinfo and `[::1]` are read correctly and a missing port is the
/// scheme's default (REL-6a5; the hand-split this replaces read
/// `ws://127.0.0.1:9010?x` as no port, REL-6a3-I B2). `None` only when the
/// url cannot be read at all.
pub fn url_port(url: &str) -> Option<u16> {
    env::egress::target_of(url).ok().map(|t| t.port)
}

/// **WHY `url` MAY NOT BE DIALLED FROM THIS PROCESS**, if it may not. The
/// decision is `env::egress::admit_url` — the one choke point (REL-6a5):
/// isolated ([`isolation`]), only a loopback host on a port outside
/// [`PROD_PORTS`] is admitted, and anything unreadable is refused.
/// `what` names the connection in the refusal.
pub fn refuse_isolated_url(what: &str, url: &str) -> Option<String> {
    env::egress::admit_url(what, url, isolation(false).as_deref()).err()
}

/// **THE FACE'S ONE WEBSOCKET DIAL** (REL-6a5). Every `connect_async` in
/// this crate is this one, except the vendored seam's (`seam/link.rs`, whose
/// one constructor `mirror::spawn` refuses first; its per-attempt re-check is
/// REL-6a4's). Refused — never dialled — per [`refuse_isolated_url`].
pub async fn dial_ws(
    what: &str,
    url: &str,
) -> Result<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, DialError> {
    if let Some(refusal) = refuse_isolated_url(what, url) {
        return Err(DialError::Refused(refusal));
    }
    tokio_tungstenite::connect_async(url).await.map(|(ws, _)| ws).map_err(|e| DialError::Failed(e.to_string()))
}

/// Why [`dial_ws`] has no socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialError {
    /// The choke point refused it; nothing was dialled.
    Refused(String),
    /// Dialled, and the connection failed.
    Failed(String),
}

impl std::fmt::Display for DialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DialError::Refused(s) | DialError::Failed(s) => f.write_str(s),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  REL-6a4: where an isolated process WRITES
// ──────────────────────────────────────────────────────────────────────────

/// **THE HOME EVERY FILE THE FACE WRITES HANGS OFF** (REL-6a4). The draft
/// journal (`~/.tui/draft`), mic captures (`~/.tui/captures`), the logs and
/// the first-run state dir all resolve through this, never through
/// `dirs::home_dir()` directly (`rel6a4::every_home_dir_call_in_src_is_routed_or_read_only`
/// says so).
///
/// His interactive face: `$HOME`, as always. An ISOLATED process
/// ([`isolation`] — a test binary, a unit test, a dump, the marker): `$HOME`
/// only if it is an isolated root ([`is_isolated_root`]); otherwise a
/// per-process scratch home ([`scratch_home`]). So an in-process test that
/// runs with his real `HOME` (every test in the `it` binary does) writes its
/// drafts into scratch, never into `~/.tui/draft` (REL-1.S1c-I N3).
pub fn face_home() -> Option<std::path::PathBuf> {
    let isolated = isolation(false).is_some();
    let protected = if isolated { protected_homes() } else { Vec::new() };
    face_home_from(isolated, dirs::home_dir(), &protected, || scratch_home(&protected))
}

/// [`face_home`] as the `String` the chat face's `App` (and its `Store`)
/// take: what `main` hands to `app::run` / `app::run_headless` (REL-6a4b B1).
/// That `App` home is where `index::apply_registry` writes
/// (`<home>/.cauldron/projects.json` on register / reorder) and what
/// `new_thread` derives the bucket cwd from — so under isolation it is the
/// isolated home, never `dirs::home_dir()`. Empty when there is none (an
/// isolated process whose scratch could not be made): every registry write
/// is then refused ([`isolated_write_refusal`]: a relative path).
pub fn face_home_string() -> String {
    face_home().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default()
}

/// [`face_home`] over explicit inputs. `protected` is [`protected_homes`]
/// (his home from the password database first, which does not follow a
/// repointed `$HOME`); `scratch` makes the per-process scratch home.
pub fn face_home_from(
    isolated: bool,
    home: Option<std::path::PathBuf>,
    protected: &[std::path::PathBuf],
    scratch: impl FnOnce() -> Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    if !isolated {
        return home;
    }
    match home {
        Some(h) if is_isolated_root_of(&h, protected) => Some(h),
        _ => scratch(),
    }
}

/// The env var that ADDS a home to [`protected_homes`] (REL-6a4b). It can
/// only ever protect more: nothing it names is ever made writable, and with
/// his passwd home unknown nothing is isolated whatever it says. A test sets
/// it to a stand-in for his home in a CHILD process, so a forced-red run of
/// a guard writes into the stand-in (watched by stat) and never into his.
pub const PROTECTED_HOME_ENV: &str = "CAULDRON_PROTECTED_HOME";

/// **THE HOMES AN ISOLATED PROCESS MAY NOT WRITE UNDER**: his passwd home
/// (`env::guard::passwd_home`), then [`PROTECTED_HOME_ENV`] when it is set and
/// absolute. EMPTY when his passwd home cannot be found, and empty means
/// nothing is an isolated root ([`is_isolated_root_of`]).
pub fn protected_homes() -> Vec<std::path::PathBuf> {
    let Some(passwd) = env::guard::passwd_home() else { return Vec::new() };
    let mut out = vec![passwd];
    if let Some(extra) = std::env::var_os(PROTECTED_HOME_ENV).map(std::path::PathBuf::from) {
        if extra.is_absolute() && !out.contains(&extra) {
            out.push(extra);
        }
    }
    out
}

/// [`is_isolated_root`] against EVERY protected home; `false` for none.
pub fn is_isolated_root_of(p: &std::path::Path, protected: &[std::path::PathBuf]) -> bool {
    !protected.is_empty() && protected.iter().all(|h| is_isolated_root(p, Some(h)))
}

/// **WHY THIS PROCESS MAY NOT WRITE `what` AT `p`**, if it may not
/// (REL-6a4b): it is isolated and `p` is not an isolated root (relative, his
/// home, inside it or above it, or his home cannot be found). `None` for his
/// interactive face. The second layer under [`face_home`]: a write that
/// takes its home from anywhere else is still refused here.
pub fn isolated_write_refusal(what: &str, p: &std::path::Path) -> Option<String> {
    let why = isolation(false)?;
    isolated_write_refusal_from(Some(&why), what, p, &protected_homes())
}

/// [`isolated_write_refusal`] over explicit inputs.
pub fn isolated_write_refusal_from(isolated: Option<&str>, what: &str, p: &std::path::Path, protected: &[std::path::PathBuf]) -> Option<String> {
    let why = isolated?;
    (!is_isolated_root_of(p, protected)).then(|| {
        format!(
            "refused: {what} at {} — not an isolated root (relative, his home, inside it, an ancestor of it, or his \
             home could not be found), and this process is isolated ({why})",
            p.display()
        )
    })
}

/// **MAY AN ISOLATED PROCESS WRITE UNDER `p`?** Absolute, and neither his
/// home, inside it (his store is `~/.cauldron`), nor an ancestor of it —
/// the same rule `tests/it/face.rs`'s `scratch_ok` applies to a launch.
/// `passwd` unknown is a no: nothing can be shown to be outside a home that
/// cannot be found.
pub fn is_isolated_root(p: &std::path::Path, passwd: Option<&std::path::Path>) -> bool {
    let Some(real) = passwd else { return false };
    if !p.is_absolute() {
        return false;
    }
    let (p, real) = (canon(p), canon(real));
    !(p.starts_with(&real) || real.starts_with(&p))
}

/// **WHY AN ISOLATED PROCESS MAY NOT USE `root` AS ITS CAULDRON STORE**, if
/// it may not (REL-6a4): `root` is his (`~/.cauldron`, which is where an
/// unset `CAULDRON_HOME` lands) or anything else [`is_isolated_root`] refuses.
/// `None` for his interactive face.
pub fn isolated_root_refusal(root: &std::path::Path) -> Option<String> {
    let why = isolation(false)?;
    isolated_root_refusal_from(Some(&why), root, &protected_homes())
}

/// [`isolated_root_refusal`] over explicit inputs.
pub fn isolated_root_refusal_from(isolated: Option<&str>, root: &std::path::Path, protected: &[std::path::PathBuf]) -> Option<String> {
    let why = isolated?;
    (!is_isolated_root_of(root, protected)).then(|| {
        format!(
            "refused: the cauldron store root {} is not an isolated root (his home, inside it, an ancestor of it, \
             or his home could not be found), and this process is isolated ({why}). Point CAULDRON_HOME at a scratch dir",
            root.display()
        )
    })
}

/// Where [`scratch_home`] goes when `$TMPDIR` is not an isolated root.
pub const SCRATCH_FALLBACK: &str = "/private/tmp";

/// The per-process scratch home [`face_home`] falls back to: one `0700` dir
/// under the system temp dir, made once — or under [`SCRATCH_FALLBACK`] when
/// the temp dir is under (or above) a protected home, or `None` when that is
/// too (REL-6a4b; [`scratch_home_from`]).
fn scratch_home(protected: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    static HOME: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        use std::os::unix::fs::DirBuilderExt;
        let p = scratch_home_from(&std::env::temp_dir(), std::path::Path::new(SCRATCH_FALLBACK), std::process::id(), protected)?;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&p).ok()?;
        Some(p)
    })
    .clone()
}

/// **WHERE THE SCRATCH HOME GOES** (REL-6a4b, REL-6a4-I N3): under `tmp` if
/// that is an isolated root, else under `fallback` if that is, else nowhere.
/// A `$TMPDIR` inside his home would otherwise make the "isolated" home a dir
/// inside his home.
pub fn scratch_home_from(tmp: &std::path::Path, fallback: &std::path::Path, pid: u32, protected: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    let name = format!("tui-isolated-home-{pid}");
    [tmp, fallback].into_iter().map(|d| d.join(&name)).find(|p| is_isolated_root_of(p, protected))
}

/// `p` with symlinks resolved through its nearest existing ancestor (`/var` is
/// `/private/var` on macOS), so a path not yet made compares like one that is.
fn canon(p: &std::path::Path) -> std::path::PathBuf {
    let mut tail = Vec::new();
    let mut at = p;
    loop {
        if let Ok(c) = at.canonicalize() {
            return tail.iter().rev().fold(c, |acc: std::path::PathBuf, t| acc.join(t));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                at = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// [`isolation`] over explicit inputs, so every signal can be tested without
/// touching the process environment.
pub fn isolation_from(headless: bool, unit_test: bool, test_harness: bool, var: impl Fn(&str) -> Option<String>) -> Option<String> {
    if headless {
        return Some("a headless --dump".into());
    }
    if unit_test {
        return Some("a unit test".into());
    }
    if test_harness {
        return Some("a cargo test harness".into());
    }
    if var(ISOLATED_ENV).is_some_and(|x| x.trim() == "1") {
        return Some(format!("{ISOLATED_ENV}=1"));
    }
    None
}

/// **IS THIS EXECUTABLE A CARGO TEST HARNESS?** `env::egress` owns the
/// rule (REL-6a5), so the face and the CLI decide it alike.
pub fn exe_is_test_harness(exe: &std::path::Path) -> bool {
    env::egress::exe_is_test_harness(exe)
}

/// **WHERE THE THREADS CONNECTION GOES**, decided before anything dials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    port: u16,
}

impl Endpoint {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// **RESOLVE, OR REFUSE.** `explicit` is `TUI_THREADS_CHASSIS_PORT` as
    /// set (or not); `isolated` is [`isolation`]'s answer.
    ///
    /// - An explicit value must be a port (1..=65535). The seam reads junk,
    ///   `0` and unset all as 3653 — his live daemon — so junk is refused,
    ///   never passed on.
    /// - With no explicit port, the interactive face resolves the
    ///   environment's own (`cauldron_env::resolve_in`, as the chat face
    ///   does); an isolated face is REFUSED: it never falls back to the
    ///   environment's daemon, which for prod is 3653 (REL-6-I F1, F2).
    /// - An isolated face never gets one of his live ports
    ///   ([`PROD_PORTS`]), explicit or resolved. His interactive face does.
    pub fn resolve(env: &env::Environment, isolated: Option<&str>, explicit: Option<&str>) -> Result<Endpoint, String> {
        let explicit = explicit.map(str::trim).filter(|v| !v.is_empty());
        let port = match explicit {
            Some(v) => match v.parse::<u16>() {
                Ok(p) if p > 0 => p,
                _ => {
                    return Err(format!(
                        "refused: TUI_THREADS_CHASSIS_PORT=`{v}` is not a port (the seam would read it as 3653, his live daemon)"
                    ))
                }
            },
            None => match isolated {
                Some(why) => {
                    return Err(format!(
                        "refused: this face is isolated ({why}) and TUI_THREADS_CHASSIS_PORT is not set. An isolated face \
                         never falls back to the environment's own threads daemon; point it at a test daemon with \
                         TUI_THREADS_CHASSIS_PORT=<port>"
                    ))
                }
                None => crate::cauldron_env::resolve_in(env)?.port,
            },
        };
        if let Some(why) = isolated {
            if PROD_PORTS.contains(&port) {
                return Err(format!(
                    "refused: port {port} is a live daemon of his (3651/3653/3753/9010), and this face is isolated ({why})"
                ));
            }
        }
        Ok(Endpoint { port })
    }
}

/// **THE LAUNCH'S DECISION** that this face may connect, and how it was
/// launched. Only `cli` makes one (`pub(crate)`), so an App built by a test
/// has none and [`crate::cauldron::app::App::start_live`] dials nothing.
///
/// **REL-6d: THE DOOR OUTLIVES A CAULDRON SWITCH.** It is carried by
/// `App::switch_env`, and it remembers what the LAUNCH set — the environment
/// it opened and `TUI_THREADS_CHASSIS_PORT` as the launch held it — because
/// `Live::start` re-points that variable (the seam's lever) at every
/// connection it makes, so reading it again after a connection would read
/// the face's own last port back as if the launch had chosen it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Door {
    headless: bool,
    /// The environment the launch opened (root and name).
    launch_root: std::path::PathBuf,
    launch_env: String,
    /// `TUI_THREADS_CHASSIS_PORT` as the launch set it, before anything here
    /// pointed it.
    explicit: Option<String>,
}

impl Door {
    pub(crate) fn of_launch(headless: bool, env: &env::Environment, explicit: Option<String>) -> Door {
        Door { headless, launch_root: env.root.clone(), launch_env: env.name.clone(), explicit }
    }

    /// [`isolation`] for this launch.
    pub fn isolation(&self) -> Option<String> {
        isolation(self.headless)
    }

    /// **WHERE THIS FACE'S THREADS GO WHILE IT SHOWS `env`** (REL-6d).
    /// [`Door::endpoint_with`] over this process's isolation and the ports
    /// it was given (`env::egress::admit_url`).
    pub fn endpoint(&self, env: &env::Environment) -> Result<Endpoint, String> {
        let iso = self.isolation();
        self.endpoint_with(env, iso.as_deref(), |url| env::egress::admit_url("threads", url, iso.as_deref()))
    }

    /// **THE RULE, over explicit inputs.**
    /// - The launch's own environment: [`Endpoint::resolve`] with the
    ///   launch's explicit port, exactly as at launch.
    /// - Any other environment (he switched cauldron): the explicit port
    ///   named the LAUNCH's daemon, so it never follows him into another
    ///   environment — that environment's OWN daemon is resolved.
    ///   - His interactive face: [`Endpoint::resolve`] with no explicit port.
    ///   - An isolated face the launch never pointed: refused, unchanged
    ///     (REL-6a F1 — no fallback to an environment's daemon).
    ///   - An isolated face the launch DID point: the environment's own
    ///     declared port, admitted only if `admit` (the egress gate: a port
    ///     this process was given) admits it and it is not one of his live
    ///     ports. Anything else is refused, naming why; nothing is dialled.
    pub fn endpoint_with(&self, env: &env::Environment, isolated: Option<&str>, admit: impl Fn(&str) -> Result<(), String>) -> Result<Endpoint, String> {
        let launch = env.name == self.launch_env && env.root == self.launch_root;
        if launch {
            return Endpoint::resolve(env, isolated, self.explicit.as_deref());
        }
        match (isolated, self.explicit.as_deref().map(str::trim).filter(|v| !v.is_empty())) {
            (None, _) | (Some(_), None) => Endpoint::resolve(env, isolated, None),
            (Some(why), Some(_)) => {
                let port = crate::cauldron_env::resolve_in(env)?.port;
                // REL-6e F2: the egress gate's own never-dial list (his live
                // daemons AND tts 3652), so every port it refuses even when
                // given is refused HERE, in the Door's words, before `admit`.
                if env::egress::NEVER_PORTS.contains(&port) {
                    let his = env::egress::NEVER_PORTS.map(|p| p.to_string()).join("/");
                    return Err(format!(
                        "refused: environment `{}` declares threads on port {port}, a live daemon of his ({his}), and this face is isolated ({why})",
                        env.name
                    ));
                }
                admit(&format!("ws://127.0.0.1:{port}/")).map_err(|e| format!("environment `{}`: {e}", env.name))?;
                Ok(Endpoint { port })
            }
        }
    }
}

/// The connection, its runtime and the store it feeds.
pub struct Live {
    /// The chat face's store: the thread list's daemon facts, the open
    /// thread's transcript, the link state. The ONLY copy.
    pub store: Store,
    rx: Option<mpsc::Receiver<Ev>>,
    tx: Option<mpsc::Sender<Ev>>,
    handle: Option<Handle>,
    rt: Option<tokio::runtime::Runtime>,
    /// Held for the life of a POINTED connection: the lock every test seam in
    /// this binary takes before it sets `TUI_THREADS_CHASSIS_PORT`.
    _ports: Option<mirror::TestPorts>,
    /// Why there is no connection, when there is none.
    pub why_not: Option<String>,
    /// Whether drafts persist in the chat face's journal (`draft.rs`). Only a
    /// real face does: a test must never write under the real HOME.
    pub drafts: bool,
    /// Sends the daemon has answered, for the tab to settle its composer by.
    sent: Vec<(String, Result<Option<u64>, String>)>,
    /// Sends issued and not yet answered, by thread.
    pub sending: HashMap<String, String>,
    /// V1.5: shape frames a test folded into a DETACHED Live by hand
    /// ([`Live::fold_by_hand`]) — the mirror [`Live::pool`] reads when there
    /// is no connection. Never set on a connected Live.
    by_hand: Option<crate::seam::held::Held>,
    /// V1.6: what the daemon answered the account verbs this face said
    /// ([`Live::account`]), for Config › Accounts to fold in.
    acct_tx: std::sync::mpsc::Sender<AccountReply>,
    acct_rx: std::sync::mpsc::Receiver<AccountReply>,
    /// **THE POOL AS OF THE LAST TICK** (V1-ACCT2, V1-ACCT-I B1): read off
    /// the seam's mirror once per [`Live::drain`], and what [`Live::pool`]
    /// answers until the next. The mirror is folded on the connection's
    /// thread, so two reads of it inside one frame could see two rosters:
    /// the Config page drew 22 rows and then `App::dash_rows` counted 24
    /// (`view.rs`'s "drawn rows and App::dash_rows disagree"), and in a
    /// release build the selection and hit-map index a different list from
    /// the one drawn. Every read between two ticks now sees one pool.
    ticked: Option<Pool>,
}

/// **WHAT THE DAEMON ANSWERED AN ACCOUNT VERB** (V1.6): the ack's payload (a
/// login's `session_id` and `url`, its `ok`), or the refusal. `tag` is the
/// caller's, so a reply to a login he has left is known for what it is.
/// Never the code he pasted.
#[derive(Debug, Clone)]
pub struct AccountReply {
    pub tag: u64,
    /// `begin`, `complete`, `cancel` or `in_pool`.
    pub verb: &'static str,
    pub result: Result<serde_json::Value, String>,
}

impl Live {
    /// A store with no connection: what `App::open` holds until the face
    /// starts one, and what a test feeds facts into by hand.
    pub fn detached(home: String) -> Self {
        let (acct_tx, acct_rx) = std::sync::mpsc::channel();
        Live {
            store: Store::new(String::new(), home),
            rx: None,
            tx: None,
            handle: None,
            rt: None,
            _ports: None,
            why_not: Some("not connected to a threads daemon".into()),
            drafts: false,
            sent: Vec::new(),
            sending: HashMap::new(),
            by_hand: None,
            acct_tx,
            acct_rx,
            ticked: None,
        }
    }

    /// **THE FACE'S CONNECTION**, to the endpoint `ep` — already resolved,
    /// and already refused if this face is isolated and it names one of his
    /// live ports ([`Endpoint::resolve`]). Nothing here reads the environment
    /// to decide where to go: the seam is pointed at exactly `ep`'s port.
    ///
    /// `pub(crate)`, deliberately: only the face's own launch (`cli`) holds a
    /// [`Door`] to resolve one through, so an integration test has no way to
    /// reach this door — it uses [`Live::start_pointed`], which refuses his
    /// ports outright (REL-6a, inspect F1).
    pub(crate) fn start(ep: &Endpoint, home: String, drafts: bool) -> Self {
        if let Err(why) = mirror::check_ports() {
            return Self::refused(home, why);
        }
        // THE CONSTRUCTOR'S OWN REFUSAL (REL-6a2), whatever resolved `ep`:
        // an isolated process never connects to one of his live ports.
        if let Some(why) = isolation(false) {
            if PROD_PORTS.contains(&ep.port) {
                return Self::refused(home, format!("refused: port {} is a live daemon of his, and this process is isolated ({why})", ep.port));
            }
        }
        std::env::set_var("TUI_THREADS_CHASSIS_PORT", ep.port.to_string());
        let mut live = Self::detached(home.clone());
        live.drafts = drafts;
        match live.connect(mirror::spawn) {
            Ok(()) => live,
            Err(why) => Self::refused(home, why),
        }
    }

    /// **A CONNECTION POINTED AT `port`** — the door tests and headless proofs
    /// use. It REFUSES his live ports ([`PROD_PORTS`]) and never connects to
    /// them, and it holds the binary's test-port lock for its whole life.
    pub fn start_pointed(port: u16, home: String) -> Result<Self, String> {
        if PROD_PORTS.contains(&port) {
            return Err(format!("refused: port {port} is a live daemon of his (3651/3653/3753/9010 are never dialled from a pointed face)"));
        }
        let mut live = Self::detached(home);
        let mut guard = None;
        live.connect(|| {
            let (h, rx, g) = mirror::spawn_at(port);
            guard = Some(g);
            (h, rx)
        })?;
        live._ports = guard;
        Ok(live)
    }

    pub(crate) fn refused(home: String, why: String) -> Self {
        let mut l = Self::detached(home);
        l.why_not = Some(why);
        l
    }

    fn connect(&mut self, spawn: impl FnOnce() -> (Handle, mpsc::Receiver<TransportEvent>)) -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("cauldron-live")
            .enable_all()
            .build()
            .map_err(|e| format!("could not start the threads runtime: {e}"))?;
        let (tx, rx) = mpsc::channel::<Ev>(256);
        let (handle, mut trx) = {
            let _in = rt.enter();
            spawn()
        };
        let fwd = tx.clone();
        rt.spawn(async move {
            while let Some(ev) = trx.recv().await {
                if fwd.send(Ev::Transport(ev)).await.is_err() {
                    return;
                }
            }
        });
        self.store.endpoint = crate::seam::link::ws_url();
        self.rt = Some(rt);
        self.tx = Some(tx);
        self.rx = Some(rx);
        self.handle = Some(handle);
        self.why_not = None;
        self.ticked = self.mirror_pool();
        Ok(())
    }

    /// Whether a connection was started (it may still be down).
    pub fn is_started(&self) -> bool {
        self.handle.is_some()
    }

    /// Whether the link is up right now, by the store's link state — the
    /// state ordered with the frames.
    pub fn is_up(&self) -> bool {
        matches!(self.store.link, LinkState::Connected)
    }

    /// Whether this connection's daemon EVER answered (its welcome named a
    /// boot): the store keeps `boot_id` across a disconnect (REL-6e F3).
    pub fn was_reached(&self) -> bool {
        self.store.boot_id.is_some()
    }

    /// Whether the daemon was reached and then lost (the stale state).
    pub fn is_down(&self) -> bool {
        matches!(self.store.link, LinkState::Disconnected { .. })
    }

    /// **APPLY ONE TRANSPORT FACT**, the chat face's `Store::apply` with one
    /// difference: a disconnect keeps every fact (see the module header).
    pub fn feed(&mut self, ev: TransportEvent) {
        let now = Utc::now();
        match ev {
            TransportEvent::Disconnected { reason, next_retry_in_ms } => {
                self.store.down_since.get_or_insert(now);
                self.store.link = LinkState::Disconnected {
                    reason,
                    retry_at: chrono::TimeDelta::try_milliseconds(next_retry_in_ms as i64)
                        .filter(|d| !d.is_zero())
                        .map(|d| now + d),
                };
            }
            // The link came up: the chat face drops every fact here and waits
            // for the exports to refill them, so a daemon-only thread read
            // `untitled` on this tab after a reconnect until they did
            // (measured: REL-6 G5's first run). Kept instead: the fresh
            // export replaces each row it carries (rule 2.5).
            TransportEvent::Invalidate { reason } => {
                tracing::info!(%reason, "kept every fact across the link change");
            }
            ev => {
                self.store.apply(ev, now);
            }
        }
    }

    /// **EMPTY THE CHANNEL, NEVER WAIT ON IT.** Applies at most
    /// [`DRAIN_BATCH`] events and returns how many; the caller paints once
    /// for the batch.
    ///
    /// It is also the ONE point the account pool moves for this face: the
    /// mirror is read once here ([`Live::pool`] answers from that read until
    /// the next tick), and a pool that changed counts as one more thing to
    /// paint.
    pub fn drain(&mut self) -> usize {
        let mut n = 0;
        while n < DRAIN_BATCH {
            let Some(rx) = self.rx.as_mut() else { break };
            let ev = match rx.try_recv() {
                Ok(ev) => ev,
                Err(_) => break,
            };
            n += 1;
            self.apply(ev);
        }
        if self.handle.is_some() {
            let now = self.mirror_pool();
            if now != self.ticked {
                self.ticked = now;
                n += 1;
            }
        }
        n
    }

    /// One event off the channel.
    pub fn apply(&mut self, ev: Ev) {
        match ev {
            Ev::Transport(t) => self.feed(t),
            Ev::Transcript { thread_id, result } => match result {
                Ok((rows, from)) => self.store.load_transcript(&thread_id, rows, from),
                Err(why) => self.store.transcript_failed(&thread_id, why),
            },
            Ev::TranscriptBodies { thread_id, rows } => {
                let from = rows.first().map(|r| r.seq).unwrap_or(0);
                let from = self.store.open_from_seq().unwrap_or(from);
                self.store.load_transcript(&thread_id, rows, from);
            }
            Ev::Sent { thread_id, result } => {
                self.sending.remove(&thread_id);
                self.sent.push((thread_id, result));
            }
            _ => {}
        }
    }

    /// **OPEN A THREAD'S TRANSCRIPT** — the chat face's open: the store's open
    /// thread, then `threads/log` held for it and drawn first, bodies after.
    pub fn open(&mut self, id: &str) {
        if self.store.open_id() == Some(id) {
            return;
        }
        self.store.open(id);
        if let (Some(rt), Some(h), Some(tx)) = (&self.rt, &self.handle, &self.tx) {
            let _in = rt.enter();
            mirror::spawn_transcript(h.clone(), tx.clone(), id.to_string());
        }
    }

    pub fn close(&mut self) {
        self.store.close();
    }

    /// **SEND `text` INTO `thread`** through the seam's `send` — never a local
    /// echo: the message appears when the daemon logs it. Refused, with the
    /// reason, while the link is not up.
    pub fn send(&mut self, thread: &str, text: &str) -> Result<(), String> {
        if !self.is_started() {
            return Err(format!("not sent: {}", self.why_not.as_deref().unwrap_or("not connected to a threads daemon")));
        }
        if !self.is_up() {
            return Err(SEND_BLOCKED.to_string());
        }
        let (Some(rt), Some(h), Some(tx)) = (&self.rt, &self.handle, &self.tx) else {
            return Err(SEND_BLOCKED.to_string());
        };
        let _in = rt.enter();
        crate::sends::spawn_send(h.clone(), tx.clone(), thread.to_string(), text.to_string(), 1);
        self.sending.insert(thread.to_string(), text.to_string());
        Ok(())
    }

    /// **SAY ONE ACCOUNT VERB** (V1.6) through this face's one connection;
    /// its answer comes back on [`Live::take_account_replies`] with `tag`.
    /// Refused, with the reason, while the link is not up — never queued.
    pub fn account(&mut self, v: crate::acts::AccountVerb, tag: u64) -> Result<(), String> {
        if !self.is_started() {
            return Err(self.why_not.clone().unwrap_or_else(|| "not connected to a threads daemon".into()));
        }
        let (Some(rt), Some(h)) = (&self.rt, &self.handle) else {
            return Err("not connected to a threads daemon".into());
        };
        // The seam's own link state, not only the store's: the pool this
        // page draws is read straight off the seam's mirror, so a roster on
        // screen means the seam is up even before the store's link event is
        // drained (measured: a flip right after the roster arrived was
        // refused as "not connected").
        if !(self.is_up() || h.is_live()) {
            return Err("the threads daemon is not connected right now".into());
        }
        let verb = match &v {
            crate::acts::AccountVerb::LoginBegin { .. } => "begin",
            crate::acts::AccountVerb::LoginComplete { .. } => "complete",
            crate::acts::AccountVerb::LoginCancel { .. } => "cancel",
            crate::acts::AccountVerb::InPool { .. } => "in_pool",
        };
        let (h, tx) = (h.clone(), self.acct_tx.clone());
        rt.spawn(async move {
            let result = h.account(v).await.map(|ack| ack.payload);
            let _ = tx.send(AccountReply { tag, verb, result });
        });
        Ok(())
    }

    /// The account verbs the daemon answered since the last call.
    pub fn take_account_replies(&mut self) -> Vec<AccountReply> {
        self.acct_rx.try_iter().collect()
    }

    /// The sends the daemon answered since the last call.
    pub fn take_sent(&mut self) -> Vec<(String, Result<Option<u64>, String>)> {
        std::mem::take(&mut self.sent)
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  V1.5: the account pool, for Status › Tokens
// ──────────────────────────────────────────────────────────────────────────

/// **A CELL THE PRODUCER MARKED** while a command changes it (consumer §7,
/// `docs/PENDING-PROMISES.md`): read, never written, timed out or inferred.
/// The cell's own `CellState` stays `Known`; the VALUE is the mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    Pending { op: String, since: String, until: Option<String> },
    RecheckFailed { op: String, reason: String },
}

impl Mark {
    /// The mark a cell's value carries, if it carries one: `state` is
    /// `pending` or `recheck_failed` AND `op` is present.
    pub fn of(v: &serde_json::Value) -> Option<Mark> {
        let s = |k: &str| v.get(k).and_then(serde_json::Value::as_str).map(str::to_string);
        let op = s("op")?;
        match v.get("state")?.as_str()? {
            "pending" => Some(Mark::Pending { op, since: s("started_at").unwrap_or_else(|| "—".into()), until: s("deadline_at") }),
            "recheck_failed" => Some(Mark::RecheckFailed { op, reason: s("reason").unwrap_or_else(|| "no reason given".into()) }),
            _ => None,
        }
    }

    /// What the face says for it — never the last real value (consumer §7).
    /// The producer's words (`op`, `reason`) may quote an account key: they
    /// pass the one masker here, so no surface draws them raw (V1-ACCT-I N5).
    pub fn words(&self) -> String {
        crate::pool::model::masked_text(&match self {
            Mark::Pending { op, since, until: Some(u) } => format!("pending {op} since {since} until {u}"),
            Mark::Pending { op, since, until: None } => format!("pending {op} since {since}"),
            Mark::RecheckFailed { reason, .. } => format!("recheck failed: {reason}"),
        })
    }
}

/// **WHAT THIS FACE'S ONE CONNECTION HOLDS OF THE ACCOUNT POOL** — the
/// standing `accounts` and `threads/non-idle` mirrors the seam already holds
/// (`seam::held::STANDING`), folded into the pool board's cards by
/// [`crate::pool::model::project`]: the meters, the daemon's `pressure` and
/// the placement are the pool board's reading, not re-derived here. No
/// subscription is added for it (CON-37) and `accounts/gauge-history` is not
/// asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct Pool {
    /// An `accounts` export, or a complete membership, has arrived. Until
    /// then the pool is LOADING — never empty, never `0`.
    pub known: bool,
    pub board: crate::pool::model::Board,
    /// Accounts the producer named that have no row here yet: drawn loading.
    pub loading: Vec<String>,
    /// Each account's marked cells (§7), by cell name.
    pub marks: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Mark>>,
    /// Each account's `credential.subscription_type` (`max`, `pro`), when the
    /// daemon knows it.
    pub subscriptions: std::collections::BTreeMap<String, String>,
}

/// [`Live::pool`] over one held mirror set, at `now` (which only drops a limit
/// that has passed, as the pool board does).
pub fn pool_of(h: &crate::seam::held::Held, now: chrono::DateTime<Utc>) -> Pool {
    use crate::pool::model::{project, Fold, ACCOUNTS, FLEET, NON_IDLE, SUMMARY, THREADS};
    let mut accounts = Fold::new(ACCOUNTS);
    let mut non_idle = Fold::new(NON_IDLE);
    let mut marks: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Mark>> = Default::default();
    let mut loading = Vec::new();
    let mut subscriptions = std::collections::BTreeMap::new();
    let mut known = false;
    if let Some(m) = h.mirror(ACCOUNTS) {
        known = m.exported() || m.members().complete();
        accounts.exported = m.exported();
        accounts.dead = m.dead_reason().map(str::to_string);
        if m.shared().is_object() {
            accounts.shared = m.shared().clone();
        }
        for k in m.keys() {
            let row = m.row(&k).clone();
            if let Some(o) = row.as_object() {
                for (cell, v) in o {
                    if let Some(mk) = Mark::of(v) {
                        marks.entry(k.clone()).or_default().insert(cell.clone(), mk);
                    }
                }
            }
            if let Some(t) = row.pointer("/credential/subscription_type").and_then(serde_json::Value::as_str) {
                subscriptions.insert(k.clone(), t.to_string());
            }
            accounts.rows.insert(k, row);
        }
        for k in m.members().keys() {
            if k != "default" && !accounts.rows.contains_key(&k) {
                loading.push(k);
            }
        }
    }
    if let Some(m) = h.mirror(NON_IDLE) {
        if matches!(m.key_state(FLEET), shapewire::mirror::KeyState::Held) {
            non_idle.rows.insert(FLEET.to_string(), m.row(FLEET).clone());
            non_idle.exported = true;
        }
    }
    let board = project(&accounts, &non_idle, &Fold::new(THREADS), &Fold::new(SUMMARY), now);
    Pool { known, board, loading, marks, subscriptions }
}

impl Live {
    /// **THE ACCOUNT POOL, OFF THIS FACE'S ONE CONNECTION** (V1.5): read
    /// under the seam's lock, which no task holds across I/O. `None` when
    /// there is no connection (and nothing was folded by hand) —
    /// [`Live::why_not`] says why.
    ///
    /// Connected, it is the pool as of the last tick ([`Live::drain`]), never
    /// a fresh read: a frame reads it many times (the rows it draws, then
    /// `App::dash_rows`, the selection, the hit-map), and every one of those
    /// must see the same roster (V1-ACCT-I B1).
    pub fn pool(&self) -> Option<Pool> {
        match (&self.handle, &self.by_hand) {
            (Some(_), _) => self.ticked.clone(),
            (None, Some(held)) => Some(pool_of(held, Utc::now())),
            (None, None) => None,
        }
    }

    /// The seam's mirror as it is THIS instant, folded on the connection's
    /// thread. Never drawn from: [`Live::drain`] reads it once per tick, and
    /// a test reads it to see a frame that has arrived and not yet been
    /// ticked in.
    pub fn mirror_pool(&self) -> Option<Pool> {
        match (&self.handle, &self.by_hand) {
            (Some(h), _) => Some(h.read(|held| pool_of(held, Utc::now()))),
            (None, Some(held)) => Some(pool_of(held, Utc::now())),
            (None, None) => None,
        }
    }

    /// Fold one shape frame into a DETACHED Live by hand — what a test that
    /// needs no socket feeds [`Live::pool`] with. A connected Live ignores it:
    /// its mirror is the seam's.
    pub fn fold_by_hand(&mut self, frame: &serde_json::Value) {
        if self.handle.is_none() {
            self.by_hand.get_or_insert_with(crate::seam::held::Held::new).fold(frame);
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        // Never block a caller's runtime on ours: a face leaving does not wait
        // for a socket to close.
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

impl crate::cauldron::app::App {
    /// **START THE FACE'S CONNECTION** — once per process; a second call does
    /// nothing. The face calls it the first time the Threads tab is shown
    /// (`cli`: at launch on that tab, or when he goes to it); a face that
    /// never shows it dials nothing. Never called from `set_ctab`, so an
    /// in-process test that visits the tab cannot dial the environment's
    /// daemon. Drafts persist only when the face persists (`--dump` never
    /// writes).
    pub fn start_live(&mut self) {
        if self.live.is_started() {
            return;
        }
        // The Store's home: shortening a cwd and reading the project indexes.
        // `face_home` like every other home lookup (REL-6a4b).
        let home = face_home_string();
        // **NO DOOR, NO DIAL** (REL-6a). Only the face's launch (`cli`) opens
        // a door; an App built any other way — every in-process test — has
        // none, and is refused here before anything is resolved.
        // REL-6d: the endpoint is the SHOWN environment's, through the
        // launch's door (never the lever as a previous connection left it).
        let live = match &self.live_door {
            None => Live::refused(home, NO_DOOR.to_string()),
            Some(door) => match door.endpoint(&self.env) {
                Ok(ep) => Live::start(&ep, home, self.persist),
                Err(why) => Live::refused(home, why),
            },
        };
        self.attach_live(live);
    }

    /// **WHETHER THE SHOWN TAB READS THE DAEMON** — Threads, and Status ›
    /// Tokens (V1.5: the account pool is the same connection's). The launch
    /// and the live loop start the one connection the first time this holds.
    pub fn shows_live(&self) -> bool {
        use crate::cauldron::app::CTab;
        // V1.6: Config › Accounts lists the daemon's accounts, and an
        // environment's Settings tab switches them on or off for itself.
        self.ctab == CTab::Threads || (self.ctab == CTab::Status && self.sttab == crate::cauldron::status::StatusTab::Tokens) || matches!(self.ctab, CTab::Accounts | CTab::Settings)
    }

    /// Put `live` behind the face: its project indexes read (the list's
    /// source, before the first frame, as the chat face does), then projected.
    pub fn attach_live(&mut self, mut live: Live) {
        live.store.load_projects();
        self.live = live;
        crate::cauldron::threads::settle(self);
    }
}

#[cfg(test)]
mod constructor_tests {
    use super::*;

    /// A scratch root with environments `a` (threads on `pa`) and `b` (on `pb`).
    fn two_envs(pa: u16, pb: u16) -> (env::Environment, env::Environment) {
        let root = std::env::temp_dir().join(format!("rel6d-door-{}-{pa}-{pb}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(root.join("services")).unwrap();
        std::fs::write(
            root.join("services").join("threads.toml"),
            "name = \"threads\"\nkind = \"daemon\"\nregisters = false\nprobe = \"connect\"\n[launch]\naddr = \"127.0.0.1:1\"\n[build]\nartifact = \"/usr/bin/true\"\nbin = \"true\"\n",
        )
        .unwrap();
        let mk = |name: &str, port: u16| {
            let d = root.join("data").join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("env.toml"), format!("[services.threads]\nversion = \"latest\"\naddr = \"127.0.0.1:{port}\"\n")).unwrap();
            env::Environment { root: root.clone(), root_source: env::paths::Source::CauldronHome, name: name.into(), name_source: env::paths::Source::Default, data_root: d }
        };
        (mk("a", pa), mk("b", pb))
    }

    /// **REL-6d: THE DOOR'S ENDPOINT PER SHOWN ENVIRONMENT.** Every arm has
    /// its opposite: the launch's environment keeps the launch's explicit
    /// port; a switched one gets its OWN port (never the launch's) — for an
    /// isolated face only when the gate admits it and it is not his; an
    /// isolated face the launch never pointed is refused everywhere (REL-6a F1).
    #[test]
    fn rel6d_the_door_resolves_the_shown_environments_own_endpoint_under_the_gate() {
        let (a, b) = two_envs(47_101, 47_102);
        let admit_all = |_: &str| Ok(());
        let admit_none = |u: &str| Err(format!("refused: {u} was not given"));
        let pointed = Door::of_launch(true, &a, Some("47100".into()));
        let port = |r: Result<Endpoint, String>| r.map(|e| e.port());
        // The launch's environment: the launch's explicit port.
        assert_eq!(port(pointed.endpoint_with(&a, Some("t"), admit_all)), Ok(47_100));
        // Switched, isolated, pointed: b's own port, gated.
        assert_eq!(port(pointed.endpoint_with(&b, Some("t"), admit_all)), Ok(47_102));
        let why = pointed.endpoint_with(&b, Some("t"), admit_none).unwrap_err();
        assert!(why.contains("was not given") && why.contains("`b`"), "{why}");
        // Switched, isolated, b on one of his ports: refused even if admitted.
        let (_, b_prod) = two_envs(47_101, 3653);
        let why = pointed.endpoint_with(&b_prod, Some("t"), admit_all).unwrap_err();
        assert!(why.contains("port 3653") && why.contains("refused"), "{why}");
        // REL-6e F2: every port the egress gate never dials (tts 3652 too) is
        // refused by the Door itself, in its words, even when admitted.
        for p in env::egress::NEVER_PORTS {
            let (_, b_never) = two_envs(47_101, p);
            let why = pointed.endpoint_with(&b_never, Some("t"), admit_all).unwrap_err();
            assert!(why.contains(&format!("declares threads on port {p}, a live daemon of his")), "{p}: {why}");
        }
        // Isolated, never pointed: refused on both, whatever the gate says.
        let unpointed = Door::of_launch(true, &a, None);
        for e in [&a, &b] {
            let why = unpointed.endpoint_with(e, Some("t"), admit_all).unwrap_err();
            assert!(why.contains("refused") && why.contains("TUI_THREADS_CHASSIS_PORT"), "{why}");
        }
        // His interactive face: the shown environment's own daemon.
        let his = Door::of_launch(false, &a, None);
        assert_eq!(port(his.endpoint_with(&a, None, admit_none)), Ok(47_101));
        assert_eq!(port(his.endpoint_with(&b, None, admit_none)), Ok(47_102));
        let his_pointed = Door::of_launch(false, &a, Some("47100".into()));
        assert_eq!(port(his_pointed.endpoint_with(&b, None, admit_none)), Ok(47_102), "the launch's pointer never follows him into another cauldron");
    }

    /// **REL-6a2: `Live::start` itself refuses his ports in an isolated
    /// process** (a unit test is one), whatever resolved the endpoint, and
    /// before it points the lever or connects. (The opposition, port 1
    /// passing, is the `it` test of the same name's.)
    #[test]
    fn rel6a2_live_start_refuses_his_ports_when_isolated() {
        assert!(isolation(false).is_some(), "sentinel: a unit test is isolated");
        let _unset = crate::mirror::ports_unset();
        for p in PROD_PORTS {
            let l = Live::start(&Endpoint { port: p }, String::new(), false);
            let why = l.why_not.clone().unwrap_or_default();
            assert!(!l.is_started() && why.contains("refused") && why.contains(&format!("port {p}")), "port {p}: {why}");
            assert!(std::env::var("TUI_THREADS_CHASSIS_PORT").is_err(), "the lever was pointed at {p} before the refusal");
        }
    }
}
