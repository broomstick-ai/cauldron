//! The seam table, and how a reader knows it is not lying.
//!
//! # The file
//!
//! `<data_root>/seams.json`. A service binds `127.0.0.1:0`, gets whatever port
//! the kernel hands it, and writes its **real** bound address here. Nothing
//! allocates ports, nothing resolves names, nothing runs in between.
//!
//! # Why a consumer cannot be pointed at the wrong environment
//!
//! The table lives *inside* the data root, so the path a consumer opens
//! already decides the environment — there is no name to get wrong. Belt and
//! braces: every entry restates the `env` and `data_root` it was written
//! under, and [`Table::load`] **refuses** a table whose header or entries
//! disagree with the directory it was loaded from. A seam table copied from
//! one environment to another is rejected rather than followed.
//!
//! # Why a stale seam is detectable
//!
//! A file that says `echo: 127.0.0.1:51234` when nothing is listening is a
//! lie, and a table that can be confidently wrong is worse than no table. So
//! the verdict is **a handshake with the socket**, and the recorded pid is
//! only ever a cheap pre-filter and the thing `stop` signals.
//!
//! The three candidate mechanisms, and why the handshake wins:
//!
//! * **pid liveness** (`kill -0`) cannot see a process that is alive but no
//!   longer listening on that address, and pid reuse makes a hit meaningless.
//!   This machine makes the point for free: the live `threads` daemon has
//!   PPID 1 and was never cauldron's child, so a recorded parentage or pid is
//!   not evidence of anything about who owns a port.
//! * **a bind check** (can I connect?) proves something is listening but not
//!   that it is *the* service. A recycled ephemeral port answers happily.
//! * **a heartbeat** (a `last_seen` the service refreshes) is only as true as
//!   its period: for one whole period the file is allowed to lie, and the
//!   writer that must stay alive to refresh it is the very thing in doubt.
//!
//! So: connect, say `HELLO`, and compare the **boot token** the service
//! answers with against the token in the table. The token is minted per start
//! by whoever starts the service and handed to it in its environment, so an
//! answer carrying it proves the listener is the instance this row describes.
//! The same answer carries the service's own `env` and `data_root`, which are
//! compared too — that is how a service wired to the wrong environment is
//! caught even while it is perfectly alive.
//!
//! A service that cannot answer `HELLO` (threads today) is declared
//! `probe = "connect"` and its best verdict is [`SeamState::LiveUnverified`],
//! named so that the weaker evidence is never mistaken for the stronger.
//!
//! # HOSTING — a seam written FOR a process, by the supervisor that forked it
//!
//! Almost every seam is written by the service, from inside itself, after its
//! own bind ([`ServiceEnv::register`]). That call takes an
//! [`crate::warnings::Attestation`] that only an implementation can mint, so
//! such a row could not exist without a producer that serves warnings.
//!
//! [`record_hosted`] writes the other kind. The supervisor **hosts** every
//! service its environment pins: it builds or installs the image, forks the
//! process, watches it, restarts it, rolls it back and stops it. Some of those
//! processes are binaries from another repository that do not link this crate
//! at all — `threads` and `tts` on this machine — so there is nothing inside
//! them that could write a row, and the supervisor writes one on their behalf.
//!
//! **This is not the old `adopt`, and the difference is the whole of what
//! changed.** `adopt` wrote a row about a process cauldron did not start and
//! would never touch; `start`, `stop`, `build` and `pin` all refused it by
//! name, and the verb's entire content was not-owning. A hosted row describes
//! **cauldron's own child**. Every verb works on it. What is weaker is only
//! the EVIDENCE, and only in the two places where it honestly is:
//!
//! * **No boot token**, because the token is echoed back over a line protocol
//!   the binary does not speak. So a hosted row's ceiling is
//!   [`SeamState::LiveUnverified`] — see the ceiling in [`probe`], which is
//!   written against the absent token rather than against a flag, so no
//!   hand-edited file can reach the strong verdict.
//! * **No warnings**, because the supervisor cannot ask a process that does
//!   not parse `WARNINGS`. The aggregate NAMES that gap rather than closing
//!   it, and names it as derived from [`Seam::registered`] rather than from
//!   anything a declaration claimed.
//!
//! What is NOT weaker is the pid. A hosted row's pid is the one the kernel
//! handed the supervisor when it forked the child, recorded as
//! [`PidSource::CHILD`] — which is proof in exactly the way
//! [`PidSource::SELF`] is, and in exactly the way `socket-owner` was not. So
//! [`Seam::signalable`] is `true` for a hosted row, and that is the point: a
//! supervisor that may not signal what it started cannot restart anything.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{guard, lock};

pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
pub const TABLE_VERSION: u32 = 1;

/// One seam: a service's real bound address, plus everything a reader needs to
/// decide whether to believe it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Seam {
    pub service: String,
    /// The real bound address, as reported by the kernel to the service.
    pub addr: String,
    pub pid: u32,
    /// Minted per start by the starter, handed to the service in its
    /// environment, and echoed back by the service on `HELLO`. This is the
    /// identity proof.
    pub boot_token: String,
    /// The resolved build label this instance is running. Commit-derived for
    /// anything cauldron built — see [`Seam::commit`].
    pub version: String,
    /// **THE VERSION IDENTITY: the commit this instance was built from.**
    ///
    /// The operator's ruling is *"we should have a version that does not rely
    /// on attestation - likely the commit. the registry should be clear about
    /// that version."* This field is the second half of that sentence, and it
    /// is on the SEAM rather than only in the build record on purpose: the
    /// registry is what a reader opens to find out what is running, and a
    /// reader who has to go and resolve a build label against a directory of
    /// `build.json` files in order to learn the commit is reading a registry
    /// that is not clear about the version.
    ///
    /// Why a commit and not the content fingerprint that used to be the only
    /// label: a fingerprint is a name for *these bytes*, so it can say whether
    /// two files are the same and nothing else. It cannot be compared for
    /// order, it cannot be looked up, it does not survive a rebuild with a
    /// different compiler, and there is no question you can ask a repository
    /// about it. A commit is nameable, comparable, diffable and ordered, and it
    /// still identifies the build exactly, because [`Seam::dirty`] is what
    /// carries the one case where it would not.
    ///
    /// Empty only where it is honestly unknown: a row written before this field
    /// existed, or a hosted image that cannot state its own commit (`tts` has
    /// no `build.rs` and no `--build-stamp`, so there is nothing compiled into
    /// it to read).
    /// It is NEVER guessed — an invented commit is worse than an absent one,
    /// because a reader would believe it.
    #[serde(default)]
    pub commit: String,
    /// **Was the working tree dirty when this was built?**
    ///
    /// A commit does not name a build made from uncommitted edits, so a dirty
    /// build's commit is a half-truth and this flag is the other half. It is
    /// recorded rather than refused here because the refusal belongs at BUILD
    /// time (`cauldron build` refuses a dirty tree unless told otherwise); by
    /// the time a seam exists, the honest thing to do with a dirty build is to
    /// say so in every view of it.
    #[serde(default)]
    pub dirty: bool,
    pub binary: String,
    /// Restated on every entry so a misplaced table is detectable.
    pub env: String,
    pub data_root: String,
    pub data_dir: String,
    pub started_at: u64,
    /// `seam` (HELLO handshake) | `connect` (bind check only) | `none`.
    pub probe: String,

    // ──────────────────────────────────────────── who wrote this row
    /// **DID THE SERVICE WRITE THIS ROW ITSELF?**
    ///
    /// `true` for every row [`ServiceEnv::register`] wrote: the service wrote
    /// it from inside itself, after its own bind, holding an
    /// [`crate::warnings::Attestation`] that only an implementation of
    /// [`crate::warnings::ServesWarnings`] can mint. Such a row could not
    /// exist for a producer with no warnings shape, because such a producer
    /// does not link.
    ///
    /// `false` for a row [`record_hosted`] wrote: the supervisor forked this
    /// process and wrote the row for it, because the binary does not link this
    /// crate and has nothing inside it that could. It is still cauldron's
    /// child and still cauldron's to restart and to stop.
    ///
    /// **It is the one field the warnings gap is derived from**, and that is
    /// why it replaced a declaration knob. The field it replaced,
    /// `warnings_askable`, was a boolean an operator wrote by hand about a
    /// binary the file does not contain; it could be wrong in both directions
    /// with nothing finding out. This one is written by whichever function
    /// wrote the row, so it cannot be wrong about which function wrote the
    /// row.
    ///
    /// **A ROW THAT DOES NOT SAY DEFAULTS TO `false`, AND THAT IS THE SAFE
    /// DIRECTION RATHER THAN THE FLATTERING ONE.**
    ///
    /// This defaulted to `true` for about an hour, on the reasoning that
    /// before this field existed `register` was the only writer. That
    /// reasoning was WRONG, and it was wrong in the direction that does
    /// damage: `adopt` also wrote rows, the two rows in production's
    /// `seams.json` right now were both written by it, and serde silently
    /// ignores the `"adopted": true` they still carry.
    ///
    /// So reading those rows as registered made `warnings::check` treat them
    /// as producers in breach of a contract they never signed — `status`
    /// exited 4 — and, worse, made it ASK them: it opened 127.0.0.1:3652 and
    /// wrote `WARNINGS` at the socket the operator's voice comes out of, which
    /// is the one side effect `tts`'s declaration says must never happen. It
    /// was caught by running `cauldron status` against production once and
    /// reading the exit code.
    ///
    /// `false` is both the truth for every row on this machine and the
    /// direction that cannot cause a side effect: an unregistered row is never
    /// contacted and is named as a gap. A legacy row that really was written
    /// by `register` is therefore understated for one cycle, until the next
    /// start rewrites it — and understating coverage is a cost the aggregate
    /// announces in words, where the other direction was a write onto a
    /// production socket.
    #[serde(default)]
    pub registered: bool,
    /// Where the recorded [`Seam::pid`] came from, so a pid that was *guessed*
    /// is never read as a pid that was *proved*. One of [`PidSource`]'s words.
    #[serde(default = "pid_source_self")]
    pub pid_source: String,
    /// **THE HTTP HEALTH URL** for a row with `probe = "http"` (R5.S3): the
    /// declaration's `[launch] health`, expanded. Empty for every other probe.
    /// See [`crate::hosting::http_health`].
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub health: String,
}


/// **HOW A RECORDED PID WAS COME BY.**
///
/// It is on the row because the recorded pid is what a signal would go to, and
/// a pid that was *guessed* must never be read as a pid that was *proved*. The
/// list is short now and every entry is proof: the inferring sources
/// (`socket-owner`, `pgrep-exact`, `declared`, `unknown`) went with `adopt`,
/// because cauldron no longer writes a row about a process it did not fork and
/// so never has to infer a pid again.
pub struct PidSource;

impl PidSource {
    /// The service's own `std::process::id()`, from inside the process, on the
    /// row the service wrote itself.
    pub const SELF: &'static str = "self";
    /// **The pid the kernel handed the supervisor when it forked this child.**
    ///
    /// Proof in exactly the way [`PidSource::SELF`] is. It is not an
    /// observation about who happens to hold a port and it is not a name
    /// lookup: it is the return value of the fork, for a process whose parent
    /// is the supervisor recording it. The thing `socket-owner` could never
    /// establish — that *this* process is the one we are responsible for — is
    /// what this source is.
    pub const CHILD: &'static str = "child";

    pub const ALL: &'static [&'static str] = &[Self::SELF, Self::CHILD];
}

fn pid_source_self() -> String {
    PidSource::SELF.to_string()
}

impl Seam {
    /// **Is this a seam `cauldron stop` is allowed to signal?**
    ///
    /// The one place the question is answered, so `stop`, the daemon's restart
    /// path and `test`'s teardown cannot answer it three different ways.
    ///
    /// The condition is about the PID's PROVENANCE and nothing else: the pid
    /// must have come from a source that is proof. There are two —
    /// [`PidSource::SELF`], the service's own `getpid` on a row it wrote
    /// itself, and [`PidSource::CHILD`], the pid the kernel handed the
    /// supervisor at the fork. Both answer "is this process the one we are
    /// responsible for" with yes.
    ///
    /// # What changed, and why the answer is now YES for the services that
    /// matter
    ///
    /// This used to also require `!self.adopted`, and the live `threads`
    /// daemon was the case it existed for: alive, listening, right address,
    /// and refused — because stopping it was another supervisor's job. There
    /// is no other supervisor now. Cauldron forks `threads`, so its pid is
    /// `child`, so `stop` signals it, and a supervisor that may not signal
    /// what it started could not restart anything, which is the whole job.
    ///
    /// What did NOT change is the rule underneath: a pid cauldron **inferred**
    /// is a pid cauldron may not kill. `socket-owner` and `pgrep-exact` are
    /// gone from [`PidSource`] rather than demoted, so there is no value this
    /// field can hold that would let a signal reach a process cauldron did not
    /// fork.
    pub fn signalable(&self) -> bool {
        PidSource::ALL.contains(&self.pid_source.as_str())
    }

    /// Why not, in a sentence `stop` can print. `None` when it is signalable.
    pub fn refusal_to_signal(&self) -> Option<String> {
        if self.signalable() {
            return None;
        }
        Some(format!(
            "`{}` records pid {} from `{}`, which is not a source that proves anything. A pid \
             cauldron INFERRED is not a pid cauldron may kill: the two honest sources are `{}` \
             (the service reported its own pid from inside itself) and `{}` (the kernel handed \
             cauldron this pid when cauldron forked the process). Any other value in this field \
             is a row from before hosting, or a hand edit. Nothing was signalled.",
            self.service,
            self.pid,
            self.pid_source,
            PidSource::SELF,
            PidSource::CHILD
        ))
    }

    /// **Could this seam's listener ever prove it is the instance this row
    /// describes?** It cannot without a boot token, and the token is minted by
    /// whoever STARTS the service and echoed back over cauldron's own line
    /// protocol. A hosted binary that does not link this crate speaks neither,
    /// so no such proof is available for it — which is precisely why its
    /// ceiling is [`SeamState::LiveUnverified`].
    pub fn has_boot_token(&self) -> bool {
        !self.boot_token.trim().is_empty()
    }
}

/// The file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    pub table_version: u32,
    pub env: String,
    pub data_root: String,
    pub seams: BTreeMap<String, Seam>,
    #[serde(skip)]
    path: PathBuf,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Table {
    pub fn empty(env: &str, data_root: &Path, path: &Path) -> Self {
        Self {
            table_version: TABLE_VERSION,
            env: env.to_string(),
            data_root: data_root.display().to_string(),
            seams: BTreeMap::new(),
            path: path.to_path_buf(),
        }
    }

    /// Load the table for `(env, data_root)`, refusing one that does not
    /// belong to that environment. An absent file is an empty table — "no
    /// service has started here yet" is a true statement, not an error.
    pub fn load(env: &str, data_root: &Path, path: &Path) -> Result<Self> {
        let path = guard::require_inside(data_root, path)?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty(env, data_root, &path))
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let mut t: Table = serde_json::from_str(&text)
            .with_context(|| format!("parsing the seam table {}", path.display()))?;
        t.path = path.clone();
        if t.table_version != TABLE_VERSION {
            bail!(
                "REFUSED: seam table {} is version {}, this build speaks version {}",
                path.display(),
                t.table_version,
                TABLE_VERSION
            );
        }
        let want_root = guard::normalize(data_root)?.display().to_string();
        if t.env != env || t.data_root != want_root {
            bail!(
                "REFUSED: seam table {} says it belongs to environment `{}` at data root `{}`, \
                 but it was loaded from environment `{}` at data root `{}`. \
                 A seam table IS its environment; a table that does not match the directory it \
                 sits in is a lie and is not followed.",
                path.display(),
                t.env,
                t.data_root,
                env,
                want_root
            );
        }
        for (key, s) in &t.seams {
            if key != &s.service {
                bail!(
                    "REFUSED: seam table {} keys `{key}` with a seam naming service `{}`",
                    path.display(),
                    s.service
                );
            }
            if s.env != env || s.data_root != want_root {
                bail!(
                    "REFUSED: seam `{}` in {} was written under environment `{}` / data root \
                     `{}`, but this table is environment `{}` / `{}`. A consumer is not pointed \
                     at the wrong environment by a mixed table; it is refused one.",
                    s.service,
                    path.display(),
                    s.env,
                    s.data_root,
                    env,
                    want_root
                );
            }
        }
        Ok(t)
    }

    fn write_atomic(&self) -> Result<()> {
        let tmp = self.path.with_extension(format!("json.tmp.{}", std::process::id()));
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming into {}", self.path.display()))?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Read-modify-write the table under the lock. Every mutation goes through
/// here, so two services registering at once cannot lose one another.
pub fn update<T>(
    env: &str,
    data_root: &Path,
    path: &Path,
    f: impl FnOnce(&mut Table) -> Result<T>,
) -> Result<T> {
    let path = guard::require_inside(data_root, path)?;
    lock::with_lock(&path, || {
        let mut t = Table::load(env, data_root, &path)?;
        let out = f(&mut t)?;
        t.write_atomic()?;
        Ok(out)
    })
}

// ---------------------------------------------------------------- the verdict

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SeamState {
    /// Handshake answered and the boot token, env and data root all match.
    Live,
    /// Something is listening, but the declaration asked for no identity
    /// check, so this is a bind check and is named as the weaker thing it is.
    LiveUnverified,
    /// Nothing is listening on the recorded address.
    StaleNoListener,
    /// Something is listening and it is not this instance.
    StaleWrongOwner,
    /// This instance is listening but reports a different environment.
    StaleWrongEnv,
    /// **This instance is listening, is the right instance, in the right
    /// environment — and is running a DIFFERENT COMMIT than the registry says
    /// it is.**
    ///
    /// Its own category rather than folded into
    /// [`SeamState::StaleWrongOwner`], because the two are different faults
    /// with different fixes. Wrong owner means *the address is held by
    /// something this row does not describe*; this means *the row describes
    /// this process correctly and is wrong about what it is*. The second one
    /// cannot be fixed by restarting anything: the registry is the thing that
    /// is wrong, and `status` saying "live" while being wrong about the commit
    /// is exactly the silence the operator's ruling on version identity was
    /// aimed at.
    StaleWrongVersion,
    /// The address accepted a connection and then said nothing in time.
    UnknownTimeout,
    /// **Something is listening, and its HEALTH CHECK did not answer 200**
    /// (R5.S3, `probe = "http"`). Not live: a listener is not a healthy
    /// service, and this is the verdict that keeps the two apart.
    Unhealthy,
    /// The table has no seam for this service.
    Down,
}

impl SeamState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SeamState::Live => "live",
            SeamState::LiveUnverified => "live:unverified",
            SeamState::StaleNoListener => "stale:no-listener",
            SeamState::StaleWrongOwner => "stale:wrong-owner",
            SeamState::StaleWrongEnv => "stale:wrong-env",
            SeamState::StaleWrongVersion => "stale:wrong-version",
            SeamState::UnknownTimeout => "unknown:timeout",
            SeamState::Unhealthy => "unhealthy",
            SeamState::Down => "down",
        }
    }
    /// Is this verdict the table telling a lie?
    pub fn is_stale(&self) -> bool {
        matches!(
            self,
            SeamState::StaleNoListener
                | SeamState::StaleWrongOwner
                | SeamState::StaleWrongEnv
                | SeamState::StaleWrongVersion
        )
    }
}

/// What a service answers `HELLO` with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub service: String,
    pub env: String,
    pub data_root: String,
    pub boot_token: String,
    pub version: String,
    /// The commit the LISTENER says it was built from, which is what makes
    /// [`SeamState::StaleWrongVersion`] detectable. `serde(default)` so a
    /// producer built before this field existed answers an empty string rather
    /// than failing to parse — and an empty answer is never compared, because
    /// "it did not say" is not "it disagrees".
    #[serde(default)]
    pub commit: String,
    pub pid: u32,
    pub addr: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub state: SeamState,
    /// Human-readable reason, always set for anything but `live`.
    pub detail: String,
    pub identity: Option<Identity>,
    /// Is the PROCESS this row describes still running? See [`row_pid_alive`].
    ///
    /// Never the LIVENESS verdict: a live pid with nothing on its port is not
    /// a reachable service, which is what `state` answers. But since C-1 it IS
    /// the STOP verdict for a row cauldron owns ([`Verdict::process_gone`]):
    /// `threads` closes its port and then drains for up to 45 s, so "the port
    /// went quiet" is the START of its stop, not the end of it.
    pub pid_alive: Option<bool>,
}

impl Verdict {
    /// **HAS THE PROCESS THIS ROW DESCRIBES ACTUALLY GONE?** (C-1)
    ///
    /// For a row cauldron owns — its pid is one cauldron forked or one the
    /// service reported from inside itself ([`Seam::signalable`]) — the answer
    /// is the PID, never the port. A quiet port with a live pid is a service
    /// mid-drain, and declaring it gone is how a second `threads` was started
    /// on the same `THREADS_HOME` while the first was still checkpointing.
    ///
    /// For any other row (an inferred pid cauldron may not act on) there is no
    /// pid worth trusting, and the old test stands: the address went quiet.
    /// The same when `ps` could not be run at all (`pid_alive: None`).
    pub fn process_gone(&self, seam: &Seam) -> bool {
        match (seam.signalable(), self.pid_alive) {
            (true, Some(alive)) => !alive,
            _ => self.state.is_stale(),
        }
    }

    /// The row is ours, its listener is gone, and its process is not: the
    /// service is ALREADY STOPPING (or has lost its listener). A stop path
    /// must WAIT for it rather than signal it again — a second SIGTERM makes
    /// `threads` abandon its drain (`services/threads/src/lib.rs`, `drain`).
    pub fn owned_and_exiting(&self, seam: &Seam) -> bool {
        seam.signalable()
            && self.pid_alive == Some(true)
            && self.state == SeamState::StaleNoListener
    }

    /// **THE ROW IS OURS AND ITS PROCESS IS STILL RUNNING — whatever the
    /// probe made of its port.** (C-1b, R9.C1 second inspection N1.)
    ///
    /// The probe state answers "is the SERVICE reachable and healthy?", and
    /// `unhealthy`, `unknown:timeout` and `stale:wrong-*` are all "no". None of
    /// them is "the process is gone": for a row cauldron owns
    /// ([`Seam::signalable`]) that is the pid's answer alone
    /// ([`Verdict::process_gone`]). So for every verb that clears a row,
    /// launches onto it or restarts it, this is "NOT GONE": `stop` signals it
    /// through the one stop path and clears the row only once the pid has
    /// exited, a launch refuses, and `restart` stops it first. It includes
    /// [`Verdict::owned_and_exiting`], which only changes HOW the stop path
    /// stops it (wait, without a second signal).
    ///
    /// A row cauldron does not own, or whose pid is dead or reused, or where
    /// `ps` could not be asked (`pid_alive: None`), is never this — it keeps
    /// the old reading: a stale row is a lie to clear, with nothing signalled.
    pub fn owned_and_running(&self, seam: &Seam) -> bool {
        seam.signalable() && self.pid_alive == Some(true)
    }
}

/// **IS THE PROCESS THIS ROW DESCRIBES STILL RUNNING?** `None` only when the
/// question could not be asked (no `/bin/ps`).
///
/// Three ways a pid answers `kill -0` and is still NOT this row's process, and
/// each one is answered `false` here because each one has a consequence:
///
/// * **No such process** — the plain case.
/// * **A zombie** (`stat` starts with `Z`). The daemon is the PARENT of every
///   service it forks, so a service that has exited stays a zombie — and
///   answers `kill -0` — until the daemon reaps it. Waiting on `kill -0`
///   alone, the daemon would wait out the whole stop grace on its own
///   unreaped child and then SIGKILL a corpse.
/// * **A REUSED pid** — a process that started AFTER this row was written
///   (`started_at`, written once the instance was already up). Since C-1 a
///   live pid is what holds a stop open and what a stop SIGNALS, so a reused
///   pid read as "ours" would mean signalling a stranger. The process's age
///   comes from `ps -o etime`; two seconds of slack cover its whole-second
///   resolution. A row with no `started_at` (0) skips this check.
pub fn row_pid_alive(seam: &Seam) -> Option<bool> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=,etime=", "-p", &seam.pid.to_string()])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.trim();
    if line.is_empty() {
        // `ps -p` exits 1 and prints nothing for a pid that does not exist.
        // An EMPTY answer with a SUCCESS status is not an answer at all.
        return if out.status.success() { None } else { Some(false) };
    }
    let mut parts = line.split_whitespace();
    let stat = parts.next().unwrap_or("");
    if stat.starts_with('Z') {
        return Some(false);
    }
    if seam.started_at > 0 {
        if let Some(age) = parts.next().and_then(parse_etime) {
            let born = now().saturating_sub(age);
            if born > seam.started_at + 2 {
                return Some(false);
            }
        }
    }
    Some(true)
}

/// `ps -o etime` — `[[dd-]hh:]mm:ss` — as whole seconds.
fn parse_etime(s: &str) -> Option<u64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let mut secs = 0u64;
    for f in rest.split(':') {
        secs = secs * 60 + f.parse::<u64>().ok()?;
    }
    Some(days * 86_400 + secs)
}

#[cfg(test)]
mod etime_tests {
    #[test]
    fn etime_forms_parse() {
        assert_eq!(super::parse_etime("00:07"), Some(7));
        assert_eq!(super::parse_etime("01:02:03"), Some(3723));
        assert_eq!(super::parse_etime("2-00:00:01"), Some(172_801));
        assert_eq!(super::parse_etime("x"), None);
    }
}

fn resolve(addr: &str) -> Result<SocketAddr> {
    addr.to_socket_addrs()
        .with_context(|| format!("resolving the seam address `{addr}`"))?
        .next()
        .with_context(|| format!("the seam address `{addr}` resolved to nothing"))
}

/// Ask the socket. This is the decision.
pub fn probe(seam: &Seam) -> Verdict {
    let alive = row_pid_alive(seam);
    let sa = match resolve(&seam.addr) {
        Ok(sa) => sa,
        Err(e) => {
            return Verdict {
                state: SeamState::StaleNoListener,
                detail: format!("{e}"),
                identity: None,
                pid_alive: alive,
            }
        }
    };
    let stream = match TcpStream::connect_timeout(&sa, PROBE_TIMEOUT) {
        Ok(s) => s,
        Err(e) => {
            return Verdict {
                state: SeamState::StaleNoListener,
                detail: format!(
                    "nothing is listening on {} ({e}); the seam table says `{}` is there, and it \
                     is not{}",
                    seam.addr,
                    seam.service,
                    match alive {
                        Some(true) => format!(
                            " — pid {} is still alive, which is exactly why pid liveness is not \
                             the test of LIVE (and, for a row cauldron owns, exactly why a quiet \
                             port is not the test of GONE: the process may be draining)",
                            seam.pid
                        ),
                        Some(false) => format!(" (pid {} is gone)", seam.pid),
                        None => String::new(),
                    }
                ),
                identity: None,
                pid_alive: alive,
            }
        }
    };
    // ── R5.S3: `probe = "http"` — the listener is there; is it HEALTHY? ──
    // A 200 from the declared health URL is the strongest verdict a hosted
    // row can reach (`live:unverified`, the same ceiling as `connect`: no
    // boot token). Anything else is `unhealthy`, never up.
    if seam.probe == "http" {
        drop(stream);
        return match crate::hosting::health_answer(&seam.health, PROBE_TIMEOUT) {
            // R9.C7h: ALIVE, NOT READY. `503 status: "migrating"` is a service
            // migrating its store behind a served port. It is not relaunched:
            // that would restart the migration from the top, every pass,
            // forever. Its own stuck bound turns a migration that never ends
            // into `recovery_stuck`, which lands in `Unhealthy` below.
            Ok(a) if a.alive_not_ready() => Verdict {
                state: SeamState::LiveUnverified,
                detail: format!(
                    "listening on {} and its health check {} answered 503 `{}`: ALIVE, NOT \
                     READY (a store migration is running; its own stuck bound ends it). Not \
                     relaunched",
                    seam.addr,
                    seam.health,
                    crate::hosting::ALIVE_NOT_READY
                ),
                identity: None,
                pid_alive: alive,
            },
            Ok(crate::hosting::HealthAnswer { code: 200, .. }) => Verdict {
                state: SeamState::LiveUnverified,
                detail: format!(
                    "listening on {} and its health check {} answered 200. No boot token, so \
                     this is health, not identity",
                    seam.addr, seam.health
                ),
                identity: None,
                pid_alive: alive,
            },
            Ok(crate::hosting::HealthAnswer { code, status }) => Verdict {
                state: SeamState::Unhealthy,
                detail: format!(
                    "something is listening on {}, but its health check {} answered {code}{}, \
                     not 200",
                    seam.addr,
                    seam.health,
                    status.map(|s| format!(" (`{s}`)")).unwrap_or_default()
                ),
                identity: None,
                pid_alive: alive,
            },
            Err(e) => Verdict {
                state: SeamState::Unhealthy,
                detail: format!(
                    "something is listening on {}, but its health check {} failed: {e:#}",
                    seam.addr, seam.health
                ),
                identity: None,
                pid_alive: alive,
            },
        };
    }
    if seam.probe == "connect" || seam.probe == "none" {
        return Verdict {
            state: SeamState::LiveUnverified,
            detail: format!(
                "something is listening on {}, and `probe = \"{}\"` asked for no identity check, \
                 so this is a bind check only: it does not prove the listener is `{}`{}",
                seam.addr,
                seam.probe,
                seam.service,
                if !seam.registered {
                    ". This row was written FOR the process by the supervisor that forked it: \
                     the binary does not link cauldron's `env` crate, so it speaks neither the \
                     line protocol nor the boot token and `live` is not reachable"
                } else {
                    ""
                }
            ),
            identity: None,
            pid_alive: alive,
        };
    }
    // ───────────────────────── THE CEILING ON `live`, AND WHY IT IS HERE ─────
    //
    // **A seam with no boot token can never read `live`.** The token is the
    // whole identity proof: it is minted per start by whoever STARTS the
    // service and handed to it in its environment, so an answer carrying it
    // proves the listener is the instance this row describes. A row with no
    // token has nothing for an answer to match, and `live` means *matched*.
    //
    // Without this clause the handshake below has a hole that is not
    // theoretical: the comparison is `id.boot_token != seam.boot_token`, so a
    // seam recording the EMPTY token and a listener answering the EMPTY token
    // compare EQUAL, and a row that proved nothing reads `live`. A HOSTED row
    // has exactly that empty token by construction, so the hole would open on
    // the one case hosting exists for — and it would open silently, in the
    // direction of claiming more than was established.
    //
    // So the refusal is written against the TOKEN rather than against
    // [`Seam::registered`]. A flag is a statement about provenance that a
    // hand-edited file can flip; the token is the evidence itself, and its
    // absence is not editable into presence. `live` is not weakened by
    // hosting, because hosting cannot reach it.
    if !seam.has_boot_token() {
        return Verdict {
            state: SeamState::LiveUnverified,
            detail: format!(
                "something is listening on {}, but this seam carries NO BOOT TOKEN, so the \
                 handshake has nothing to match and the strong verdict is not available: \
                 `live` means the listener proved it is the instance this row describes, and \
                 nothing here can prove that.{} The best honest verdict is `{}`.",
                seam.addr,
                if !seam.registered {
                    format!(
                        " This row was written FOR the process (pid {} from `{}`), because the \
                         binary does not link cauldron's `env` crate and has nothing inside it \
                         that could write a row or echo a token. The pid is proof — the kernel \
                         handed it to the supervisor at the fork — and `cauldron stop` WILL \
                         signal it. What is unavailable is the identity handshake, and nothing \
                         is pretending otherwise.",
                        seam.pid, seam.pid_source
                    )
                } else {
                    format!(
                        " The declaration asks for `probe = \"{}\"`, which is the strong check, \
                         but the row it would check was written without a token.",
                        seam.probe
                    )
                },
                SeamState::LiveUnverified.as_str()
            ),
            identity: None,
            pid_alive: alive,
        };
    }
    let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));
    let mut w = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            return Verdict {
                state: SeamState::UnknownTimeout,
                detail: format!("could not write to {}: {e}", seam.addr),
                identity: None,
                pid_alive: alive,
            }
        }
    };
    // `HELLO`, written through the SINGLE closed vocabulary the supervisor is
    // allowed to speak. See `crate::warnings::SupervisorAsk`: this and
    // `warnings::ask` are the only two socket writes in the supervisor path,
    // and a third verb does not compile.
    let hello = format!("{}\n", crate::warnings::SupervisorAsk::Hello.word());
    if let Err(e) = w.write_all(hello.as_bytes()).and_then(|_| w.flush()) {
        return Verdict {
            state: SeamState::UnknownTimeout,
            detail: format!("writing HELLO to {} failed: {e}", seam.addr),
            identity: None,
            pid_alive: alive,
        };
    }
    let mut line = String::new();
    if let Err(e) = BufReader::new(stream).read_line(&mut line) {
        return Verdict {
            state: SeamState::UnknownTimeout,
            detail: format!("{} accepted a connection then said nothing: {e}", seam.addr),
            identity: None,
            pid_alive: alive,
        };
    }
    let id: Identity = match serde_json::from_str(line.trim()) {
        Ok(id) => id,
        Err(e) => {
            return Verdict {
                state: SeamState::StaleWrongOwner,
                detail: format!(
                    "{} answered something that is not a cauldron seam identity ({e}); \
                     the listener is not `{}`",
                    seam.addr, seam.service
                ),
                identity: None,
                pid_alive: alive,
            }
        }
    };
    // Belt and braces on the SAME invariant, now about the ANSWER. The clause
    // above refuses a seam with no token; this one refuses a LISTENER with no
    // token, so a service that answers an empty string cannot match a row that
    // somehow carries one either. Both directions of the empty-equals-empty
    // hole are closed, and `live` requires a non-empty token on both sides.
    if id.boot_token.trim().is_empty() {
        return Verdict {
            state: SeamState::StaleWrongOwner,
            detail: format!(
                "the listener on {} answered an EMPTY boot token. An empty token proves \
                 nothing, so it is never a match: `live` requires the listener to carry the \
                 token this row records, and this one carries none.",
                seam.addr
            ),
            identity: Some(id),
            pid_alive: alive,
        };
    }
    if id.boot_token != seam.boot_token {
        return Verdict {
            state: SeamState::StaleWrongOwner,
            detail: format!(
                "the listener on {} is a DIFFERENT instance: it answers boot token `{}`, the \
                 seam table records `{}`. The address is held by something this row does not \
                 describe.",
                seam.addr, id.boot_token, seam.boot_token
            ),
            identity: Some(id),
            pid_alive: alive,
        };
    }
    if id.env != seam.env || id.data_root != seam.data_root {
        return Verdict {
            state: SeamState::StaleWrongEnv,
            detail: format!(
                "the listener on {} is alive and is this instance, but it reports environment \
                 `{}` at data root `{}` while the seam table says `{}` at `{}`. \
                 A service running against the wrong data root is caught here rather than later.",
                seam.addr, id.env, id.data_root, seam.env, seam.data_root
            ),
            identity: Some(id),
            pid_alive: alive,
        };
    }
    // ── IS THE REGISTRY RIGHT ABOUT THE VERSION? ────────────────────────
    //
    // Only asked when BOTH sides named a commit. An empty answer means the
    // producer predates this field and "it did not say" is not "it disagrees";
    // an empty row means the commit was honestly unknown when the row was
    // written (a hosted image that cannot state one). Comparing against an empty string
    // in either direction would manufacture a disagreement out of silence,
    // which is the same empty-equals-empty hole the boot token closes twice.
    if !seam.commit.is_empty() && !id.commit.is_empty() && id.commit != seam.commit {
        return Verdict {
            state: SeamState::StaleWrongVersion,
            detail: format!(
                "the listener on {} IS this instance, in this environment, and it reports commit \
                 `{}` while the registry records `{}`. Nothing needs restarting: the process is \
                 fine and the REGISTRY is wrong about what it is running. A `live` verdict here \
                 would be the supervisor confirming a version it had not checked.",
                seam.addr, id.commit, seam.commit
            ),
            identity: Some(id),
            pid_alive: alive,
        };
    }
    if id.service != seam.service {
        return Verdict {
            state: SeamState::StaleWrongOwner,
            detail: format!(
                "the listener on {} answers to service `{}`, not `{}`",
                seam.addr, id.service, seam.service
            ),
            identity: Some(id),
            pid_alive: alive,
        };
    }
    Verdict { state: SeamState::Live, detail: String::new(), identity: Some(id), pid_alive: alive }
}

// ------------------------------------------------- the service side of a seam

/// Everything a service needs, read from its own environment. **Nothing is
/// defaulted.** A service with an incomplete environment refuses to run rather
/// than pick a data root, because a service that can quietly choose its own
/// data root can quietly choose production's.
#[derive(Debug, Clone)]
pub struct ServiceEnv {
    pub service: String,
    pub env: String,
    pub data_root: PathBuf,
    pub data_dir: PathBuf,
    pub seams: PathBuf,
    pub boot_token: String,
    pub version: String,
    /// The commit this build came from. **Required, like every other field
    /// here.** See [`ServiceEnv::from_env`] on why nothing in this struct is
    /// defaulted, and [`Seam::commit`] on why the commit is the version.
    pub commit: String,
    /// Whether the tree was dirty at build time. **Also required**, and
    /// deliberately not defaulted to `false`: a missing flag defaulted to
    /// `clean` is a supervisor choosing the flattering answer about a question
    /// it was not told the answer to.
    pub dirty: bool,
    pub bind: String,
}

pub const SERVICE_VARS: &[&str] = &[
    "CAULDRON_SERVICE",
    "CAULDRON_ENV",
    "CAULDRON_DATA_ROOT",
    "CAULDRON_SERVICE_DATA_DIR",
    "CAULDRON_SEAMS",
    "CAULDRON_BOOT_TOKEN",
    "CAULDRON_VERSION",
    "CAULDRON_COMMIT",
    "CAULDRON_DIRTY",
    "CAULDRON_BIND",
];

impl ServiceEnv {
    pub fn from_env() -> Result<Self> {
        let mut missing = Vec::new();
        let mut get = |k: &str| match std::env::var(k) {
            Ok(v) if !v.trim().is_empty() => v,
            _ => {
                missing.push(k.to_string());
                String::new()
            }
        };
        let service = get("CAULDRON_SERVICE");
        let env = get("CAULDRON_ENV");
        let data_root = get("CAULDRON_DATA_ROOT");
        let data_dir = get("CAULDRON_SERVICE_DATA_DIR");
        let seams = get("CAULDRON_SEAMS");
        let boot_token = get("CAULDRON_BOOT_TOKEN");
        let version = get("CAULDRON_VERSION");
        let commit = get("CAULDRON_COMMIT");
        // `CAULDRON_DIRTY` is read as a WORD, not as truthiness: `1`/`true`
        // mean dirty, `0`/`false` mean clean, and anything else is refused
        // rather than quietly read as clean. A flag whose unparseable value
        // means "fine" is a flag that reports clean when it is broken.
        let dirty_raw = get("CAULDRON_DIRTY");
        let bind = get("CAULDRON_BIND");
        if !missing.is_empty() {
            bail!(
                "REFUSED TO START: this is a cauldron service and its environment is incomplete. \
                 Missing: {}. A cauldron service never defaults its data root — it is started by \
                 `cauldron start <service>`, which sets all of {:?}. Running it by hand against \
                 an unknown data root is the mistake this refusal exists to prevent.",
                missing.join(", "),
                SERVICE_VARS
            );
        }
        let dirty = match dirty_raw.trim() {
            "1" | "true" => true,
            "0" | "false" => false,
            other => bail!(
                "REFUSED TO START: `CAULDRON_DIRTY={other}` is not a yes or a no. It says whether \
                 the build this process is came from a dirty working tree, and an unparseable \
                 answer is not `clean`. Use `1`/`true` or `0`/`false`."
            ),
        };
        let data_root = guard::refuse_protected(Path::new(&data_root))?;
        let data_dir = guard::require_inside(&data_root, Path::new(&data_dir))?;
        let seams = guard::require_inside(&data_root, Path::new(&seams))?;
        Ok(Self {
            service,
            env,
            data_root,
            data_dir,
            seams,
            boot_token,
            version,
            commit,
            dirty,
            bind,
        })
    }

    /// What this service answers `HELLO` with.
    pub fn identity(&self, addr: &SocketAddr) -> Identity {
        Identity {
            service: self.service.clone(),
            env: self.env.clone(),
            data_root: self.data_root.display().to_string(),
            boot_token: self.boot_token.clone(),
            version: self.version.clone(),
            commit: self.commit.clone(),
            pid: std::process::id(),
            addr: addr.to_string(),
        }
    }

    /// Write the real bound address into the environment's seam table. Called
    /// by the service, after the bind, with the address the kernel gave it —
    /// which is why the file can only claim a listener when there was one.
    ///
    /// # Why this takes an [`Attestation`](crate::warnings::Attestation)
    ///
    /// **This is where the warnings contract is enforced at COMPILE TIME.**
    /// Registering a seam is the act that makes a process a cauldron service:
    /// before it there is no address anything can read, and after it `status`
    /// and `start` can find the process. So the obligation attaches here rather
    /// than to a crate boundary or a trait in the chassis.
    ///
    /// The attestation's field is private to [`crate::warnings`] and the only
    /// way to mint one is
    /// [`attest_warnings!`](crate::attest_warnings), which does not type-check
    /// unless the producer implements
    /// [`ServesWarnings`](crate::warnings::ServesWarnings). **A producer with no
    /// warnings shape therefore does not link**, and the compile error names
    /// the two items it is missing.
    ///
    /// It is deliberately NOT a runtime check on the shape name: a service is
    /// allowed to make the shape it answers configurable, and whether its
    /// answer is honest is the supervisor's question, asked over the socket
    /// ([`crate::warnings::check`]). That check is still there. It is the
    /// backstop now — it is what catches a process started by something other
    /// than `cauldron start` — and this is the mechanism.
    pub fn register(
        &self,
        addr: &SocketAddr,
        binary: &str,
        probe: &str,
        warnings: &crate::warnings::Attestation,
    ) -> Result<()> {
        // Named so it is visible in a stack trace and cannot be mistaken for an
        // unused parameter: holding one is the whole claim.
        let _attested_shape: &str = warnings.shape();
        update(&self.env, &self.data_root, &self.seams, |t| {
            t.seams.insert(
                self.service.clone(),
                Seam {
                    service: self.service.clone(),
                    addr: addr.to_string(),
                    pid: std::process::id(),
                    boot_token: self.boot_token.clone(),
                    version: self.version.clone(),
                    commit: self.commit.clone(),
                    dirty: self.dirty,
                    binary: binary.to_string(),
                    env: self.env.clone(),
                    data_root: self.data_root.display().to_string(),
                    data_dir: self.data_dir.display().to_string(),
                    started_at: now(),
                    probe: probe.to_string(),
                    // THIS function is the one that makes a row `registered`,
                    // and it is the only one. It is reached only by a process
                    // holding an `Attestation`, which is mintable only by
                    // `attest_warnings!`, which does not type-check without a
                    // `ServesWarnings` impl. So `registered = true` on a row
                    // means: a producer that serves warnings wrote this, from
                    // inside itself, after its own bind.
                    registered: true,
                    pid_source: PidSource::SELF.to_string(),
                    health: String::new(),
                },
            );
            Ok(())
        })
    }
}


// ═══════════════════════════════════════════════════════════════ HOSTING ════

/// **What the supervisor must know before it will write a row for a child it
/// forked but that cannot write its own.**
///
/// There is no `Default`: every field is something the supervisor established
/// at the fork, and a hosted row assembled from defaults would be a row about
/// a process nobody identified.
#[derive(Debug, Clone)]
pub struct Hosted {
    pub service: String,
    /// The address the child was TOLD to bind and has been seen listening on.
    ///
    /// A registering service binds `127.0.0.1:0` and publishes whatever the
    /// kernel gave it, which is why two cauldron deployments cannot collide on
    /// a port. A hosted binary cannot do that: it has no way to tell anybody
    /// which port it got, so the port has to be one the supervisor already
    /// knows. That is a real cost of hosting a binary that does not link this
    /// crate, it is the reason `[launch] addr` exists in the declaration, and
    /// it is why hosting two instances of one hosted service in two
    /// environments needs two declared addresses rather than being free.
    pub addr: String,
    /// `connect` or `none`. **Never `seam`** — refused below, for the reason
    /// in [`probe`]'s ceiling.
    pub probe: String,
    /// **The pid the kernel handed us at the fork.** Recorded as
    /// [`PidSource::CHILD`].
    pub pid: u32,
    pub version: String,
    /// The commit, read off the image itself where the image can state one and
    /// EMPTY where it cannot. Never guessed from a repository's log.
    pub commit: String,
    pub dirty: bool,
    pub binary: String,
    pub data_dir: PathBuf,
    /// The expanded `[launch] health` URL for `probe = "http"`; empty otherwise.
    pub health: String,
}

/// **Write the row for a hosted child.**
///
/// # Why this takes no [`crate::warnings::Attestation`], and what that costs
///
/// [`ServiceEnv::register`] takes one, and that is the warnings contract's
/// entire mechanism: a producer with no warnings shape does not link, cannot
/// write a seam, and has no address for anything to read. This function writes
/// a seam and takes no attestation, because **there is no producer here to
/// attest.** `register` is called BY the service, so the type system has the
/// producer in hand and can demand a proof about it. This is called by the
/// SUPERVISOR, about a binary compiled in another repository that does not
/// link this crate; there is no type to bound and no build to fail.
///
/// Minting an attestation on the child's behalf was available and is refused
/// outright — the supervisor swearing to a property it cannot know would make
/// every attestation anywhere worth nothing. [`crate::warnings::Attestation`]
/// therefore has no public constructor and this function has no parameter in
/// which that mistake could be made.
///
/// So the obligation MOVES, from compile time to the aggregate's own
/// accounting, in the direction that admits more rather than less: the row
/// carries `registered = false`, [`crate::warnings::check`] files it as a
/// named gap, and `status` says the total excludes it, by name, every time.
///
/// **And it is self-closing, which the thing it replaced was not.** The old
/// `warnings_askable = false` was a declaration knob: it stayed false until
/// somebody edited it, and it was a claim a TOML file made about a binary it
/// does not contain. This is derived from which function wrote the row. The
/// day `threads` links this crate and calls `register`, the row it writes is
/// `registered = true` and the gap is gone with no edit anywhere.
pub fn record_hosted(
    env: &str,
    data_root: &Path,
    seams_path: &Path,
    h: &Hosted,
    boot_token: &str,
) -> Result<Seam> {
    if h.probe == "seam" {
        bail!(
            "REFUSED to write a hosted row for `{}` with `probe = \"seam\"`. The `seam` probe \
             is a handshake in cauldron's line protocol against a boot token, and a binary that \
             does not link this crate speaks neither. The handshake could only ever fail, so \
             the row would be permanently stale while the service was healthy. `connect` or \
             `none`.",
            h.service
        );
    }
    if h.probe != "connect" && h.probe != "none" && h.probe != "http" {
        bail!("REFUSED: `{}` has `probe = \"{}\"`, which is not a probe mode.", h.service, h.probe);
    }
    if h.pid == 0 {
        bail!(
            "REFUSED to write a hosted row for `{}` with pid 0. This row's whole claim is that \
             the supervisor forked this process and knows its pid; a zero means it does not, \
             and `stop` would then have nothing to signal.",
            h.service
        );
    }
    // PROVE THE LISTENER IS THERE BEFORE RECORDING IT. The row is the one
    // place a lie is expensive, and a row written the instant after a fork,
    // before the child has bound, would be a lie for as long as the boot took.
    let sa = resolve(&h.addr)?;
    if let Err(e) = TcpStream::connect_timeout(&sa, PROBE_TIMEOUT) {
        bail!(
            "REFUSED to write a hosted row for `{}`: nothing is listening on {} ({e}). The \
             process was forked, but a row is a statement that a consumer can reach the service \
             at this address, and that statement is not true yet.",
            h.service,
            h.addr
        );
    }
    let data_dir = guard::require_inside(data_root, &h.data_dir)?;
    let seam = Seam {
        service: h.service.clone(),
        addr: sa.to_string(),
        pid: h.pid,
        // **THE TOKEN IS RECORDED AND IS USUALLY EMPTY, AND THAT IS THE
        // POINT.** It is not a field waiting to be filled in: it is the
        // recorded fact that this binary cannot echo one, and it is what
        // `probe` reads in order to refuse `live`. It is carried as a
        // parameter rather than hardcoded empty so that the day a hosted
        // binary learns to answer HELLO, the supervisor can record the token
        // it minted and the row gets the strong verdict with no change here.
        boot_token: boot_token.to_string(),
        version: h.version.clone(),
        commit: h.commit.trim().to_string(),
        dirty: h.dirty,
        binary: h.binary.clone(),
        env: env.to_string(),
        data_root: guard::normalize(data_root)?.display().to_string(),
        data_dir: data_dir.display().to_string(),
        started_at: now(),
        probe: h.probe.clone(),
        registered: false,
        pid_source: PidSource::CHILD.to_string(),
        health: h.health.clone(),
    };
    let written = seam.clone();
    update(env, data_root, seams_path, |t| {
        t.seams.insert(h.service.clone(), seam);
        Ok(())
    })?;
    Ok(written)
}
