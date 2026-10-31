//! Service declarations and the per-environment manifest.
//!
//! Two files, deliberately split, because they answer different questions.
//!
//! * A **declaration** (`services/<name>.toml`) says what a service *is*: how
//!   to build it, what binary it produces, how it is probed. It is per
//!   cauldron root, shared by every environment, because the identity of a
//!   service does not change when you change rooms.
//! * The **manifest** (`<data_root>/env.toml`) says which *version* of each
//!   service this environment runs. That is the per-seam version knob: build
//!   once, pin per environment. `dev` can be pinned to a label that `prod`
//!   has never run, and nothing had to be copied for that to be true.
//!
//! The declaration format is a deliberate subset of the drafted supervisor's
//! (design v2 §7.1 / D7) with the same key names where they overlap, so the
//! files the operator edits do not have to be rewritten later.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// `services/<name>.toml`
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Declaration {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `daemon` today. Kept so the field exists where v2 expects it.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// `seam` (HELLO identity handshake) | `connect` (bind check only) |
    /// `none`. Anything but `seam` is weaker and says so in `status`.
    #[serde(default = "default_probe")]
    pub probe: String,
    /// Extra environment handed to the service, after the cauldron variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// How long `start` waits for the service to write its seam.
    #[serde(default = "default_start_timeout")]
    pub start_timeout_ms: u64,
    /// **WHICH WARNINGS SHAPE THIS SERVICE SERVES.** Absent means the default,
    /// `warnings` — so a service cannot become exempt by forgetting. See
    /// [`WarningsDecl`] for why this is a shape NAME and not a boolean.
    #[serde(default)]
    pub warnings: WarningsDecl,
    /// **Where this service's IMAGE comes from, and how cauldron installs it
    /// as a version.** Required: a declaration with no `[build]` says nothing
    /// about what would be launched. See [`Build`], which carries two ways of
    /// producing the bytes and one way of naming them.
    #[serde(default)]
    pub build: Option<Build>,
    /// **DOES THIS SERVICE WRITE ITS OWN SEAM?**
    ///
    /// `true` (the default) means the binary links cauldron's `env` crate and
    /// calls [`crate::seams::ServiceEnv::register`] after its own bind, so the
    /// row in `seams.json` is the service's own, carries a boot token, and
    /// could only have been written by a producer holding a
    /// [`crate::warnings::Attestation`]. That is the normal case and the one
    /// the whole contract is built around.
    ///
    /// `false` means the binary does NOT link that crate — it is somebody
    /// else's daemon, built in another repository, which cauldron nevertheless
    /// **hosts**: launches, watches, restarts, rolls back and stops. Cauldron
    /// writes the row on its behalf ([`crate::seams::record_hosted`]), and the
    /// row says so ([`crate::seams::Seam::registered`]).
    ///
    /// # What this is NOT
    ///
    /// It is **not** the old `[adopt]` section under another name, and the
    /// difference is the whole of what changed. `[adopt]` was a statement
    /// about OWNERSHIP: the process belonged to another supervisor, `start`,
    /// `stop`, `build` and `pin` all refused it by name, and cauldron's entire
    /// business with it was to look at it. `registers = false` is a statement
    /// about one BINARY's link line. Every verb works: cauldron spawned the
    /// process, the pid is its own child's, `stop` signals it, `pin` chooses
    /// its version and the daemon restarts it. The only thing it cannot do is
    /// be handed a seam by the service, so the supervisor writes one.
    ///
    /// # The one honest cost, which is not papered over
    ///
    /// A process that does not link the crate does not speak its line
    /// protocol, so the supervisor cannot ask it `WARNINGS` and the warnings
    /// aggregate cannot cover it. That gap is **derived from this field and
    /// never declared**: there is no knob that says "ask it anyway", because
    /// the old `warnings_askable = false` was a hand-written claim about a
    /// binary the file does not contain, and it could be wrong in both
    /// directions with nothing finding out. See
    /// [`crate::warnings::Compliance::Unregistered`].
    ///
    /// It closes by the service being rewritten to register, at which point
    /// this line is deleted and nothing else changes.
    #[serde(default = "default_registers")]
    pub registers: bool,
    /// **THE SIGNAL THAT STOPS THIS SERVICE CLEANLY.** `TERM` by default.
    ///
    /// A key rather than a constant because the two real services disagree and
    /// the disagreement is measured, not guessed. `threads` installs a
    /// `SignalKind::terminate()` handler and runs a 45-second drain plus a WAL
    /// checkpoint on SIGTERM (`lib/threads/src/lib.rs`), so SIGTERM is its
    /// clean stop. `tts` installs **only** `tokio::signal::ctrl_c()`, which on
    /// Unix is SIGINT alone (`lib/tts/src/lib.rs`), so a SIGTERM to `tts`
    /// falls through to the default disposition: immediate death, no drain, no
    /// checkpoint. Sending the wrong signal to the voice daemon is not a
    /// stylistic matter.
    ///
    /// `INT` or `TERM`. The escalation to `KILL` is not configurable here: it
    /// is the supervisor's last rung and belongs to [`Declaration`]'s
    /// `escalate` instead.
    #[serde(default = "default_stop_signal")]
    pub stop_signal: String,
    /// **MAY THE SUPERVISOR HARD-KILL THIS SERVICE WHEN IT WILL NOT GO DOWN?**
    ///
    /// `true` by default, per the 2026-09-02 ruling: *"if it tries to softly
    /// kill it and it won't go down, then it has to hard kill it. I'm okay
    /// with that."*
    ///
    /// `false` for the voice path, and the reason is on the record: that ruling
    /// named the threads module and did not extend to the service that speaks
    /// to him mid-sentence. `~/.threads-supervisor/services/tts.conf` carries
    /// the same decision as `ESCALATION=0` and this key is where it moves to.
    /// A service with `escalate = false` that ignores its stop signal is
    /// reported and left alone — never SIGKILLed.
    #[serde(default = "default_escalate")]
    pub escalate: bool,
    /// **HOW LONG A CLEAN STOP IS GIVEN before `escalate` is considered**, in
    /// seconds, counted until the PROCESS is gone — not until its port goes
    /// quiet (C-1). 15 by default.
    ///
    /// A key because the clean stop's length is the service's, and the
    /// longest one is measured: `threads` closes its port, drains for up to
    /// 45 s, waits up to 5 s on its serve task and then checkpoints its WAL
    /// with a 5 s budget — 55 s, sized against the old supervisor's 60 s
    /// rung. A 15 s grace SIGKILLed it mid-drain. `threads.toml` says 75.
    #[serde(default = "default_stop_grace_secs")]
    pub stop_grace_secs: u64,
    /// **HOW A NON-REGISTERING CHILD IS REACHED.** Required when
    /// `registers = false` and refused otherwise. See [`Launch`].
    #[serde(default)]
    pub launch: Option<Launch>,
    /// **WHICH IDENTITY A PERMISSION PROMPT FROM THIS SERVICE NAMES.** Absent
    /// means the default: inherit cauldron's identity, so the prompt reads
    /// `Cauldron`. See [`PermissionsDecl`].
    #[serde(default)]
    pub permissions: PermissionsDecl,
    /// **THIS SERVICE IS DECLARED HERE BUT STILL LAUNCHED BY SOMETHING ELSE.**
    /// See [`Migration`]. Absent — the normal case — means cauldron hosts it.
    #[serde(default)]
    pub migration: Option<Migration>,
    /// **WHAT THE SUPERVISOR DOES WHEN THIS SERVICE DIES** (R5.S3).
    /// `always` (the default) or `never`. See [`crate::hosting::Restart`].
    /// The environment's `auto_restart` is the outer switch and this is the
    /// inner one: a restart happens only when BOTH allow it.
    #[serde(default = "crate::hosting::default_restart")]
    pub restart: String,
    /// **WHEN A `job` RUNS** (R5.S3). Required for a hosted `kind = "job"`.
    /// See [`crate::hosting::Schedule`].
    #[serde(default)]
    pub schedule: Option<crate::hosting::Schedule>,
    /// **THE FILE THIS DECLARATION WAS READ FROM.** Not a TOML key — set by
    /// [`load`] and skipped by serde, because it is a fact about where the
    /// file is and not something the file gets to claim.
    ///
    /// It exists so that a `[build]` naming a package in *this* repository
    /// does not have to name the repository. See
    /// [`Declaration::workspace_for`], and the defect in its doc.
    #[serde(skip)]
    pub loaded_from: PathBuf,
    /// **THE EXACT BYTES THIS DECLARATION WAS PARSED FROM.** Not a TOML key —
    /// set by [`load`] beside `loaded_from`, and skipped by serde.
    ///
    /// It exists so that `cauldron build` can put a copy of the declaration
    /// it resolved INTO the build directory ([`DECLARATION_SNAPSHOT`]), byte
    /// for byte, and record its hash. Re-serialising the parsed struct would
    /// lose the comments, the key order and every default-that-was-not-written,
    /// and a snapshot that is not the file is a snapshot a reader cannot diff
    /// against the file.
    #[serde(skip)]
    pub text: String,
}

/// **`[migration]` — a service that is REGISTERED with cauldron and not yet
/// HOSTED by it, because something outside cauldron still launches it.**
///
/// "Cauldron hosts ALL services" is the target, and on the day this section
/// was written three things on this machine were still started by launchd
/// rather than by cauldron (`ui-server` under `com.harness.aui`, `kokoro`
/// under `com.harness.kokoro`, the account-expiry alarm under
/// `ai.broomstick.claude-account-expiry-alarm`). Each gets its declaration
/// NOW, so `cauldron status` lists every service there is, and this section
/// records — in the file, not in a report that rots — who launches it today
/// and the exact steps that move it under cauldron.
///
/// # What this is NOT
///
/// It is **not** the deleted `[adopt]`. `[adopt]` was a permanent statement
/// that another supervisor owned a process and cauldron would write rows about
/// it. This section is a statement that a HANDOVER IS PENDING, and while it is
/// pending cauldron does exactly two things with the service:
///
/// 1. **It refuses to launch it** — `start`, `pin` and the daemon's launch path
///    all refuse a declaration carrying `[migration]`, because the outside
///    launcher is still live and two supervisors on one port is how one
///    service becomes two.
/// 2. **`status` connects to `[launch] addr`, read-only,** and reports
///    `outside:up` / `outside:down`. It writes no row, sends no signal and
///    claims no pid.
///
/// The section is deleted as the last step of the migration, and nothing else
/// in the declaration changes.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Migration {
    /// Who launches it today, as `<launcher>:<label>` — `launchd:com.harness.aui`.
    pub launched_by: String,
    /// The file that tells that launcher to do so (a LaunchAgent plist).
    #[serde(default)]
    pub plist: String,
    /// What has to be true, or be built, before the handover can happen. Empty
    /// means nothing blocks it but the operator's word.
    #[serde(default)]
    pub blocked_on: Vec<String>,
    /// The handover, in order. The last step is always deleting this section.
    #[serde(default)]
    pub steps: Vec<String>,
}

impl Migration {
    /// The refusal every launching path prints, so they cannot drift.
    pub fn refusal(&self, service: &str) -> String {
        let mut s = format!(
            "REFUSED: `{service}` is registered with cauldron but still launched by `{}`{}. \
             Cauldron will not launch it while that is true: two supervisors on one port is how \
             one service becomes two. The handover is written in its declaration's \
             `[migration]` section:",
            self.launched_by,
            if self.plist.is_empty() { String::new() } else { format!(" ({})", self.plist) }
        );
        for b in &self.blocked_on {
            s.push_str(&format!("\n  blocked on: {b}"));
        }
        for (i, st) in self.steps.iter().enumerate() {
            s.push_str(&format!("\n  {}. {st}", i + 1));
        }
        s
    }
}

/// **`[permissions]` — the identity a macOS prompt names, declared here rather
/// than per service, outside the repo.**
///
/// Today the equivalent knob is `DEPLOY_IDENTIFIER` in
/// `~/.threads-supervisor/services/<name>.conf`: one file per service, outside
/// this repo, written by hand, and *missing entirely for `tts`* — which is why
/// `tts` is under no signature gate and why its prompts can never be durable.
/// A knob that a service can be exempt from by being forgotten is the defect;
/// this section exists so there is one place that answers the question for
/// every service, with a default that is safe rather than absent.
///
/// It carries no grant and no path to one. The only thing it decides is *which
/// name the operator is shown*, and (through `identity` below) whether the claim
/// is checkable against the artifact.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PermissionsDecl {
    /// `inherit` (cauldron is the TCC subject; a prompt reads `Cauldron`) or
    /// `own` (the service disclaims and is its own subject). Default
    /// `inherit`.
    #[serde(default = "default_responsibility")]
    pub responsibility: String,
    /// The signing identifier this service's artifact is expected to bear, if
    /// any. Empty is the honest answer when the service carries no stable
    /// identity — exactly as an empty `commit` is — and is NOT the same thing
    /// as claiming one and being wrong. Checked against the artifact rather
    /// than trusted.
    #[serde(default)]
    pub identity: String,
}

impl Default for PermissionsDecl {
    fn default() -> Self {
        Self {
            responsibility: default_responsibility(),
            identity: String::new(),
        }
    }
}

fn default_responsibility() -> String {
    crate::identity::Responsibility::default().as_str().to_string()
}

impl PermissionsDecl {
    /// The parsed disposition, refusing an unknown spelling by name.
    pub fn responsibility(&self) -> Result<crate::identity::Responsibility> {
        crate::identity::Responsibility::parse(&self.responsibility)
    }

    /// The claimed identity, or `None` when nothing is claimed.
    pub fn claimed_identity(&self) -> Result<Option<crate::identity::Identity>> {
        if self.identity.trim().is_empty() {
            return Ok(None);
        }
        crate::identity::Identity::parse(&self.identity).map(Some)
    }
}

/// **WHAT A DECLARATION IS ALLOWED TO SAY ABOUT WARNINGS.**
///
/// `services/<name>.toml` carries one of three things, and the shape of this
/// type is the argument:
///
/// ```toml
/// # nothing at all        -> Shape("warnings")   the default; compliant by
/// #                                              expectation, checked at start
/// warnings = "warnings"   # -> Shape("warnings") the same, said out loud
/// warnings = "diagnostics"# -> Shape("diagnostics")  a service with its own name
/// warnings = false        # -> Refused           a confession, and `start` refuses it
/// ```
///
/// **IT IS NOT A BOOLEAN, DELIBERATELY.** A boolean is a claim a TOML file
/// makes about a binary it does not contain, and it can be wrong in both
/// directions with nothing finding out. What this carries instead is *the
/// question to ask* — a shape name — so the declaration's only power is to
/// point the supervisor at the right request, and the ANSWER is what decides
/// compliance ([`crate::warnings::check`]). The supervisor checks; it does not
/// trust.
///
/// **AND ABSENT IS NOT EXEMPT.** The default is the default shape, not "no
/// warnings". The only way to declare non-compliance is to write
/// `warnings = false`, which is a sentence an operator has to type on purpose
/// — and `cauldron start` refuses it, by name, before spawning anything.
/// There is no spelling of "I forgot" that silences the contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarningsDecl {
    /// Serve this shape. Checked by handshake at start.
    Shape(String),
    /// `warnings = false`. Nothing to ask, and `start` refuses it.
    Refused,
}

impl Default for WarningsDecl {
    fn default() -> Self {
        WarningsDecl::Shape(crate::warnings::SHAPE.to_string())
    }
}

impl<'de> Deserialize<'de> for WarningsDecl {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Flag(bool),
            Name(String),
        }
        match Raw::deserialize(d)? {
            Raw::Flag(true) => Ok(WarningsDecl::default()),
            Raw::Flag(false) => Ok(WarningsDecl::Refused),
            Raw::Name(s) => {
                let s = s.trim().to_string();
                if s.is_empty() {
                    // An empty string is not a way of saying `false`. Saying
                    // `false` is the way of saying `false`, and it is refused
                    // by name rather than by silence.
                    return Err(D::Error::custom(
                        "`warnings = \"\"` is not a shape name. Name the shape this service \
                         serves, or write `warnings = false` to declare that it serves none \
                         (which `cauldron start` refuses).",
                    ));
                }
                if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '/')
                {
                    return Err(D::Error::custom(format!(
                        "`warnings = \"{s}\"` is not a usable shape name ([A-Za-z0-9_-/])"
                    )));
                }
                Ok(WarningsDecl::Shape(s))
            }
        }
    }
}

impl Serialize for WarningsDecl {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            WarningsDecl::Shape(n) => s.serialize_str(n),
            WarningsDecl::Refused => s.serialize_bool(false),
        }
    }
}

impl Declaration {
    /// The build, or a refusal naming what is missing. Never panics: a
    /// declaration with no `[build]` is a malformed file, not a legal state,
    /// but it is read from disk and so must be reported rather than asserted.
    pub fn build(&self) -> Result<&Build> {
        self.build.as_ref().with_context(|| {
            format!(
                "the declaration for `{}` has no `[build]` section, so nothing says where this \
                 service's image comes from. Cauldron HOSTS every service it declares — it \
                 launches it, watches it, restarts it and rolls it back — and it cannot launch \
                 an image it cannot name. Add `[build]` with either a cargo source \
                 (`workspace`/`package`/`bin`) or an `artifact` path to an image produced \
                 elsewhere.",
                self.name
            )
        })
    }

    /// **Everything a declaration can contradict itself about, checked at LOAD
    /// time** — before any verb has had a chance to act on it.
    ///
    /// Not at use time, because a contradictory declaration's consequences are
    /// not symmetrical: on a long-lived machine a wrong answer here means a second
    /// daemon on a large live sqlite store. It is refused while it is still
    /// a file.
    pub fn validate(&self) -> Result<()> {
        let b = self.build.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "REFUSED: the declaration for `{}` has no `[build]` section, so nothing says \
                 where this service's image comes from. There is no `[adopt]` any more and no \
                 third state: cauldron hosts what it declares, so every declaration says how \
                 its image is produced.",
                self.name
            )
        })?;
        b.validate(&self.name)?;
        match self.stop_signal.as_str() {
            "TERM" | "INT" => {}
            other => bail!(
                "REFUSED: `{}` declares `stop_signal = \"{other}\"`. The clean stop is `TERM` \
                 or `INT` — nothing else, because those are the two a daemon can install a \
                 handler for and the two the supervisor will send. `KILL` is not spellable \
                 here: it is the supervisor's last rung, reached only after the clean stop was \
                 sent and ignored, and it is gated by `escalate` rather than chosen as a \
                 service's normal stop.",
                self.name
            ),
        }
        if self.stop_grace_secs == 0 {
            bail!(
                "REFUSED: `{}` declares `stop_grace_secs = 0`. A stop with no grace is a \
                 SIGKILL by another name, and SIGKILL is the supervisor's last rung, gated by \
                 `escalate` — not a service's normal stop.",
                self.name
            );
        }
        // ── A SERVICE THAT CANNOT REGISTER CANNOT BE PROBED BY HANDSHAKE ───
        //
        // The `seam` probe compares a boot token the STARTER minted against
        // the one the listener answers with, over cauldron's line protocol. A
        // binary that does not link this crate speaks neither. Declaring
        // `probe = "seam"` for it would produce a row that fails its own check
        // forever, which is worse than a weaker check named honestly — the
        // same argument the adopted declarations carried, kept because it was
        // never about adoption.
        if !self.registers && self.probe == "seam" {
            bail!(
                "REFUSED: `{}` declares `registers = false` and `probe = \"seam\"`. The `seam` \
                 probe is a HANDSHAKE in cauldron's own line protocol against a boot token, and \
                 a binary that does not link cauldron's `env` crate speaks neither the protocol \
                 nor the token. The handshake could only ever fail, so the row would be \
                 permanently stale while the service was perfectly healthy. Use \
                 `probe = \"connect\"` (a bind check, verdict `live:unverified`) or \
                 `probe = \"none\"`. `live` is not available here and is not being weakened to \
                 reach it.",
                self.name
            );
        }
        match self.kind.as_str() {
            "daemon" => {}
            // A calendar job (launchd `StartCalendarInterval`). Cauldron has no
            // scheduler, so a `job` can be REGISTERED and never HOSTED: it is
            // only legal while something else still launches it.
            "job" if self.migration.is_some() => {}
            // R5.S3: cauldron SCHEDULES jobs now. A `[schedule]` makes a job
            // something the daemon runs itself. See `crate::hosting`.
            "job" if self.schedule.is_some() => {}
            "job" => bail!(
                "REFUSED: `{}` declares `kind = \"job\"` with neither a `[schedule]` nor a \
                 `[migration]` section. A job cauldron is the only launcher of needs a \
                 `[schedule]` saying when the holder runs it; a job something else still \
                 launches says so in `[migration]`. With neither, it would never run.",
                self.name
            ),
            other => bail!(
                "REFUSED: `{}` declares `kind = \"{other}\"`. The kinds are `daemon` (hosted: \
                 launched, watched, restarted) and `job` (a pass run on a `[schedule]` by the \
                 holder, or registered while something outside still launches it).",
                self.name
            ),
        }
        if let Some(mg) = &self.migration {
            if mg.launched_by.trim().is_empty() || mg.steps.is_empty() {
                bail!(
                    "REFUSED: `{}` has a `[migration]` section with no `launched_by` or no \
                     `steps`. The section exists to say who launches it today and how it moves \
                     under cauldron; without both it says neither.",
                    self.name
                );
            }
        }
        let job = self.kind == "job";
        match (self.registers, self.launch.is_some()) {
            (false, false) if job => {}
            (false, false) => bail!(
                "REFUSED: `{}` declares `registers = false` and has no `[launch]` section. A \
                 binary that does not link cauldron's `env` crate cannot publish the port it \
                 bound, so the supervisor has to be told where it will be listening — \
                 otherwise it could fork the process and then have no address to put in the \
                 registry and nothing to probe. Add `[launch] addr = \"127.0.0.1:<port>\"`.",
                self.name
            ),
            (true, true) => bail!(
                "REFUSED: `{}` declares a `[launch]` section and `registers = true`. Those \
                 contradict: a registering service binds 127.0.0.1:0 and PUBLISHES the address \
                 the kernel gave it, which is the whole reason two cauldron deployments cannot \
                 collide on a port. Declaring an address for it would be the supervisor \
                 deciding a port the service is going to ignore. Remove `[launch]`.",
                self.name
            ),
            _ => {}
        }
        // A service that cannot be asked must not CONFESS either. `warnings =
        // false` is a sentence a service that owes the contract types on
        // purpose, and `status` counts it as a violation and exits 4. A
        // non-registering binary never signed the contract — the compile-time
        // obligation never ran against it and cannot retroactively — so it is
        // not in breach. It is a GAP, which cauldron derives from
        // `registers = false` on its own, without accusing anybody.
        if !self.registers && matches!(self.warnings, WarningsDecl::Refused) {
            bail!(
                "REFUSED: `{}` declares both `registers = false` and `warnings = false`. Those \
                 say different things and only one of them is true. `warnings = false` is a \
                 CONFESSION by a service that owes the warnings contract, and `status` counts it \
                 as a violation. A binary that does not link cauldron's `env` crate never \
                 signed that contract, so it is not in breach of it. Remove the `warnings` \
                 line: `registers = false` already says the total does not cover this service, \
                 and says it without accusing anybody.",
                self.name
            );
        }
        // The permission declaration is checked HERE, at load, for the same
        // reason the rest of this function is: the cost of a bad value is paid
        // on a security dialog the operator is reading, and the cheapest place
        // to refuse it is while it is still a file nobody has acted on.
        self.permissions.responsibility().with_context(|| {
            format!("the `[permissions]` section of the declaration for `{}`", self.name)
        })?;
        self.permissions.claimed_identity().with_context(|| {
            format!("the `[permissions]` section of the declaration for `{}`", self.name)
        })?;
        crate::hosting::validate(self)?;
        Ok(())
    }

    /// The shape to ask this service for, or `None` if the declaration says it
    /// serves none.
    pub fn warnings_shape(&self) -> Option<&str> {
        match &self.warnings {
            WarningsDecl::Shape(n) => Some(n),
            WarningsDecl::Refused => None,
        }
    }

    /// **WHICH CARGO WORKSPACE THIS SERVICE IS BUILT FROM.**
    ///
    /// `[build] workspace` when the declaration names one, and otherwise
    /// **the cauldron workspace this cauldron binary was built from** —
    /// [`shipped_workspace`].
    ///
    /// # The defect this exists to fix, because it was the identity mechanism
    /// lying
    ///
    /// `services/echo.toml` and `services/tally.toml` carried
    /// `workspace = "/Users/testuser/code/cauldron"` as an absolute path. Every
    /// proof and every build run from a git WORKTREE of this repository
    /// therefore compiled, fingerprinted and stamped **the main checkout**
    /// rather than the tree being proved. An agent measured `cauldron build`
    /// reporting a *sibling's branch HEAD* as its own build's commit, and
    /// `prove-wave1` scoring 78/2 because of it.
    ///
    /// Under the operator's ruling that the commit IS the version identity,
    /// that is not a cosmetic bug. The whole argument for a commit over a
    /// content fingerprint is that a commit is *checkable by anybody with the
    /// repository* — and a build that silently stamps a plausible, real,
    /// checkable commit it was not built from defeats exactly that, in the one
    /// direction a reader cannot detect: the label looks right, resolves, and
    /// names someone else's code.
    ///
    /// # Why it is NOT "the directory this declaration file sits in"
    ///
    /// That was the first fix and it was wrong, and the way it was wrong is
    /// worth keeping written down because it is the more tempting answer.
    ///
    /// Declarations are looked for in TWO places ([`search_paths`]): the
    /// operator's own `<root>/services/` first, then the ones that ship with
    /// this workspace. The first is an OVERRIDE directory — on this machine it
    /// is `~/.cauldron/services/`, and `prove-wave1` uses it on purpose, to
    /// produce a second version of `echo` at a different profile. Resolving
    /// "the repository this file is in" for an override gives `~/.cauldron`,
    /// which is a data root and not a cargo workspace at all. `prove-wave1`
    /// aborted on exactly that.
    ///
    /// So the right default is not where the FILE is, it is where this BINARY
    /// came from. That is self-locating in the way the literal was not — a
    /// cauldron built in a worktree resolves to that worktree, because
    /// `CARGO_MANIFEST_DIR` was inside it at compile time — and it is correct
    /// for an override, because an override overrides a declaration that
    /// shipped with a workspace and is built from the same one.
    ///
    /// An absolute `workspace` is still honoured, because a service whose code
    /// genuinely lives in another repository has to say so — and the two real
    /// ones do not even use it, since `threads` and `tts` are installed from an
    /// `artifact` and have no cargo source at all.
    pub fn workspace_for(&self, b: &Build) -> Result<PathBuf> {
        if let Some(w) = &b.workspace {
            return Ok(PathBuf::from(w));
        }
        shipped_workspace().with_context(|| {
            format!(
                "`{}`'s `[build]` names no `workspace`, which means the cauldron workspace this \
                 binary was built from — and that could not be resolved. Name the `workspace` \
                 outright.",
                self.name
            )
        })
    }

    /// **Is the warnings aggregate able to cover this service at all?**
    ///
    /// Derived, in one place, from the one fact that decides it. A caller that
    /// wanted to ask a non-registering binary for warnings would be writing an
    /// unparseable word onto a socket — on this machine, onto the socket the
    /// operator's voice comes out of.
    pub fn warnings_askable(&self) -> bool {
        self.registers && self.warnings_shape().is_some()
    }
}

/// **`[launch]` — WHAT CAULDRON MUST BE TOLD ABOUT A CHILD THAT CANNOT TELL
/// IT ANYTHING.**
///
/// A registering service binds `127.0.0.1:0`, takes whatever port the kernel
/// hands it, and writes the real bound address into the seam table itself.
/// That is why two cauldron deployments cannot collide on a port even in
/// principle, and why `cauldron test` takes an environment rather than a port.
///
/// A binary that does not link this crate cannot do any of that. It has no way
/// to publish the port it got, so the port has to be one the supervisor
/// already knows — which means it is declared, and which means two instances
/// of one hosted service in two environments need two declared addresses
/// rather than being free. **That is a real cost of hosting a foreign binary
/// and it is stated here rather than discovered when two environments collide.**
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Launch {
    /// The address cauldron expects this child to be listening on once it has
    /// booted. It is what the row records and what `probe` connects to.
    ///
    /// Cauldron does not choose it and cannot: the port lives in the child's
    /// own configuration. On this machine `threads` takes 3651 and 3653 from
    /// compiled defaults (`config::DEFAULT_PORT` /
    /// `DEFAULT_CHASSIS_PORT`), reachable only through a `config.toml` or, for
    /// the chassis alone, `$THREADS_CHASSIS_PORT`; `tts` takes 3652 the same
    /// way. So this key is a statement of fact about the child, and
    /// `record_hosted` refuses to write the row until something really is
    /// listening there.
    pub addr: String,
    /// **THE CHILD'S WORKING DIRECTORY** (R5.S3). Absent means the service's
    /// data directory, as before. `${CAULDRON_SERVICE_DATA_DIR}` expands.
    #[serde(default)]
    pub cwd: Option<String>,
    /// **THE HTTP HEALTH CHECK** (R5.S3), required by and only legal with
    /// `probe = "http"`: a `http://host:port/path` URL that must answer a
    /// status 200. `${CAULDRON_HOSTED_ADDR}` expands. See
    /// [`crate::hosting::http_health`].
    #[serde(default)]
    pub health: Option<String>,
}

/// **WHERE A SERVICE'S IMAGE COMES FROM, AND HOW IT IS NAMED.**
///
/// Two ways of producing the bytes, one section, and every verb downstream
/// behaves identically afterwards — `pin`, `rollback`, `start`, `stop` and the
/// daemon's restart path cannot tell which was used.
///
/// 1. **A CARGO SOURCE** (`workspace` + `package` + `bin` + `profile`).
///    Cauldron runs cargo and names the version after `git rev-parse HEAD` in
///    that workspace, refusing a dirty tree. This is `echo` and `tally`.
/// 2. **AN ARTIFACT** (`artifact` + `bin`). Cauldron runs no compiler. It
///    installs the image at that path as a version by copy, and reads the
///    commit **off the bytes** by running the image with `stamp_args`.
///
/// # Why the second one exists, and why it is not a weakening
///
/// The operator's ruling is that **the commit is the version identity, read
/// off the artifact** — and `threads --build-stamp` prints exactly that:
/// `commit=<sha> provenance=pin dirty=0 epoch=<n>`, the commit having been
/// compiled in by whoever shipped the image. Reading it back off the file that
/// will actually run is *stronger* evidence than `git rev-parse` in a
/// workspace, which names the tree a build came from rather than the bytes.
///
/// It also fits the machine as it is rather than as a design would prefer it.
/// `threads` is shipped by a signing pipeline in another repository; its own
/// `build.rs` **refuses to build over a signed deploy image** (it exits 1
/// unless `THREADS_PROMOTE=1`), its launch path is `chflags uchg` immutable,
/// and a release build of it in a fresh target directory is 3.4 GB on a disk
/// with 2.6 GB free. A design in which cauldron can only host what cauldron
/// compiles would have had cauldron host nothing.
///
/// And it keeps the signature in its ruled place: *identity only, not a gate.*
/// Cauldron never reads the signature. It reads the commit, which is the thing
/// anybody with the repository can check.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Build {
    /// Cargo workspace directory holding the `Cargo.toml` to build. Required
    /// for a cargo source; absent for an artifact.
    #[serde(default)]
    pub workspace: Option<String>,
    /// Required for a cargo source; absent for an artifact.
    #[serde(default)]
    pub package: Option<String>,
    /// The binary's file NAME. Required either way: it is the name of the
    /// installed file inside the version directory, which is what the
    /// supervisor execs.
    pub bin: String,
    /// **PER SERVICE.** Nothing in the build verb forces debug or release: a
    /// service that must be debug for structural reasons says `profile =
    /// "dev"` and one whose release profile keeps assertions on says
    /// `profile = "release"`. The verb does not care, and the profile is in
    /// the version label because a debug and a release build of one commit are
    /// two different binaries.
    #[serde(default = "default_profile")]
    pub profile: String,
    /// Extra args appended to the cargo invocation.
    #[serde(default)]
    pub args: Vec<String>,
    /// **THE IMAGE, PRODUCED ELSEWHERE.** When set, cauldron runs no compiler:
    /// it copies this file into the version directory and reads the commit off
    /// it with [`Build::stamp_args`]. Mutually exclusive with `workspace` /
    /// `package`.
    #[serde(default)]
    pub artifact: Option<String>,
    /// **HOW THE IMAGE STATES ITS OWN COMMIT.** The args to run it with; its
    /// first line of stdout is parsed for `commit=<sha>` and `dirty=<0|1>`.
    ///
    /// Empty means the image cannot state a commit, which is a real and legal
    /// answer — `tts` has no `build.rs` and no `--build-stamp` subcommand, so
    /// there is nothing compiled into it to read. Then the version is named
    /// after the CONTENT FINGERPRINT with an `unstamped-` prefix, so no reader
    /// can mistake sixteen hex characters for a commit, and the registry
    /// records an empty commit rather than a guessed one. Guessing it from the
    /// repository's log is how a previous declaration carried
    /// `version = "290113e"` — a real, plausible, checkable, WRONG commit —
    /// for a day.
    #[serde(default)]
    pub stamp_args: Vec<String>,
    /// **WHICH CARGO TARGET DIRECTORY A CARGO SOURCE BUILDS INTO.** Absent
    /// means the environment's own (`data/shared/cache/target`), which is the
    /// right default: intermediates are inside the cauldron root and deletable
    /// with it.
    ///
    /// Overridable because a foreign workspace may already have a populated
    /// target tree and a fresh one may not fit on the disk. Setting it does
    /// NOT make the target tree a launch path: `build` copies the artifact
    /// into `data/shared/builds/<service>/<label>/` and the supervisor execs
    /// the copy, so the D-2 hazard — `target/release/threads` being the live
    /// daemon's launch path — is not reachable from here however this is set.
    #[serde(default)]
    pub target_dir: Option<String>,
}

impl Build {
    /// Which directory cargo drops the artifact into for this profile.
    pub fn target_subdir(&self) -> &str {
        match self.profile.as_str() {
            "dev" | "test" => "debug",
            "release" | "bench" => "release",
            other => other,
        }
    }
    /// The cargo profile flag, or nothing for the default.
    pub fn profile_args(&self) -> Vec<String> {
        if self.profile == "dev" {
            Vec::new()
        } else {
            vec!["--profile".into(), self.profile.clone()]
        }
    }
    /// Is this an image produced elsewhere rather than a cargo source?
    pub fn is_artifact(&self) -> bool {
        self.artifact.is_some()
    }
    /// Does the image state its own commit?
    pub fn stamps_itself(&self) -> bool {
        !self.stamp_args.is_empty()
    }

    /// **EXACTLY ONE SOURCE OF BYTES, checked at load time.**
    ///
    /// A section carrying both would let `cauldron build` compile one binary
    /// and install a different one under its name, which is how a supervisor
    /// ends up believing it knows what is running.
    pub fn validate(&self, service: &str) -> Result<()> {
        let cargo = self.package.is_some();
        match (cargo, self.artifact.is_some()) {
            (true, true) => bail!(
                "REFUSED: `{service}`'s `[build]` names BOTH a cargo `package` and an \
                 `artifact`. Those are two different sets of bytes and the section would be \
                 saying to compile one and install the other. Keep one."
            ),
            (false, false) => bail!(
                "REFUSED: `{service}`'s `[build]` names neither a cargo `package` nor an \
                 `artifact`, so nothing says where the image comes from."
            ),
            (true, false) => {
                if self.stamps_itself() {
                    bail!(
                        "REFUSED: `{service}`'s `[build]` is a cargo source and also declares \
                         `stamp_args`. The commit of a cargo source is the commit of the \
                         workspace it was built from, read with `git rev-parse HEAD` and refused \
                         when the tree is dirty; `stamp_args` is for an image cauldron did not \
                         build and whose provenance is only in its own bytes. Two answers to \
                         `what commit is this` is one too many."
                    );
                }
            }
            (false, true) => {
                if self.target_dir.is_some() {
                    bail!(
                        "REFUSED: `{service}`'s `[build]` names an `artifact` and also a \
                         `target_dir`. Cauldron runs no compiler for an artifact, so there is \
                         no build to direct anywhere."
                    );
                }
                if !self.args.is_empty() {
                    bail!(
                        "REFUSED: `{service}`'s `[build]` names an `artifact` and also cargo \
                         `args`. There is no cargo invocation to append them to."
                    );
                }
            }
        }
        if self.workspace.is_some() && self.package.is_none() {
            bail!(
                "REFUSED: `{service}`'s `[build]` names a `workspace` and no `package`. A \
                 workspace is a directory full of packages; without one named there is nothing \
                 to build."
            );
        }
        if self.bin.trim().is_empty() {
            bail!("REFUSED: `{service}`'s `[build]` has an empty `bin`.");
        }
        crate::hosting::validate_bin(service, self)?;
        Ok(())
    }
}

fn default_kind() -> String {
    "daemon".into()
}
fn default_probe() -> String {
    "seam".into()
}
fn default_profile() -> String {
    "dev".into()
}
fn default_start_timeout() -> u64 {
    5000
}
fn default_registers() -> bool {
    true
}
fn default_stop_signal() -> String {
    "TERM".into()
}
fn default_escalate() -> bool {
    true
}
fn default_stop_grace_secs() -> u64 {
    15
}

/// Where declarations are looked for, in order: the root's overrides first,
/// then the ones that ship with this workspace, so `cauldron start echo` works
/// with no setup and can still be overridden per root by dropping a file in
/// `<root>/services/`.
pub fn search_paths(root_services: &Path) -> Vec<PathBuf> {
    vec![
        root_services.to_path_buf(),
        // `<this workspace>/services`, resolved without `..` so the path
        // prints readably in a "looked in" message.
        //
        // DEPTH-DEPENDENT: `nth(2)` is the workspace root because this crate
        // lives TWO directories down (`libs/env`). Move this crate to another
        // depth and this silently points somewhere else — `cauldron status`
        // prints the whole search path, which is how you check it.
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap_or(Path::new("/"))
            .join("services"),
    ]
}

/// **THE CAULDRON WORKSPACE THIS BINARY WAS BUILT FROM.**
///
/// The parent of the shipped `services/` directory, which is the second entry
/// of [`search_paths`] — so the two cannot drift, and a `[build]` with no
/// `workspace` builds from the same tree whose declarations it is reading.
///
/// **DEPTH-DEPENDENT**, exactly as [`search_paths`] is and for the same
/// reason: `nth(2)` is the workspace root because this crate lives two
/// directories down (`libs/env`). Move the crate and this silently points
/// somewhere else; `cauldron status` prints the whole search path, which is
/// how you check it.
pub fn shipped_workspace() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(|p| p.to_path_buf())
        .context("resolving the cauldron workspace this binary was built from")
}

pub fn load(root_services: &Path, name: &str) -> Result<Declaration> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("`{name}` is not a usable service name ([A-Za-z0-9_-])");
    }
    let mut tried = Vec::new();
    for dir in search_paths(root_services) {
        let p = dir.join(format!("{name}.toml"));
        if p.exists() {
            let text = std::fs::read_to_string(&p)
                .with_context(|| format!("reading {}", p.display()))?;
            return parse(&text, &p, name);
        }
        tried.push(p);
    }
    bail!(
        "no declaration for service `{name}`. Looked in:\n{}",
        tried.iter().map(|p| format!("  {}", p.display())).collect::<Vec<_>>().join("\n")
    )
}

/// **`~` AND `~/…` AT THE START OF A PATH-VALUED DECLARATION VALUE.**
///
/// A declaration is shared by every machine the repository is checked out on,
/// so a path under the operator's home cannot be written as a literal without
/// either naming him or naming nobody (REL-4.S4-I F3). `~` is the portable
/// spelling, and this is the one place it is given a meaning: `$HOME` when it
/// is set and absolute (the supervisor's own environment, which is what a
/// launchd agent is handed), else the passwd home. Only a LEADING `~` alone or
/// followed by `/` expands; `~user`, a `~` mid-string, and everything else are
/// returned untouched, so this is not a template language either.
pub fn expand_tilde(v: &str) -> String {
    let rest = if v == "~" {
        ""
    } else if let Some(r) = v.strip_prefix("~/") {
        r
    } else {
        return v.to_string();
    };
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
        .or_else(crate::guard::passwd_home);
    match home {
        Some(h) if rest.is_empty() => h.display().to_string(),
        Some(h) => h.join(rest).display().to_string(),
        None => v.to_string(),
    }
}

impl Declaration {
    /// Applies [`expand_tilde`] to every path-valued key: `[launch] cwd`,
    /// `[build] artifact` / `workspace` / `target_dir`, and every `[env]` value.
    fn expand_home(&mut self) {
        if let Some(l) = self.launch.as_mut() {
            if let Some(c) = l.cwd.as_mut() {
                *c = expand_tilde(c);
            }
        }
        if let Some(b) = self.build.as_mut() {
            for v in [&mut b.artifact, &mut b.workspace, &mut b.target_dir].into_iter().flatten() {
                *v = expand_tilde(v);
            }
        }
        for v in self.env.values_mut() {
            *v = expand_tilde(v);
        }
    }
}

/// **ONE PARSER FOR BOTH PLACES A DECLARATION IS READ FROM** — the shipped or
/// override file ([`load`]) and a build directory's snapshot ([`snapshot`]) —
/// so the two cannot drift on what "loads" means: same name check, same
/// `validate`, same `loaded_from` discipline.
fn parse(text: &str, p: &Path, name: &str) -> Result<Declaration> {
    let mut d: Declaration = toml::from_str(text)
        .with_context(|| format!("parsing the service declaration {}", p.display()))?;
    // SET HERE AND NOWHERE ELSE. It is what makes a `[build]` with no
    // `workspace` mean "the repository this file is in" rather than a
    // hardcoded absolute path that names the main checkout from every
    // worktree. See `Declaration::workspace_for`.
    d.loaded_from = p.to_path_buf();
    d.text = text.to_string();
    // `~` in a path-valued key means the home of the user cauldron runs as.
    // Expanded HERE, once, so no consumer sees a literal `~` and treats it as
    // a relative directory called `~` (REL-4.S4-I F3: nothing downstream
    // expanded it, so a `cwd = "~/…"` named a directory that does not exist).
    d.expand_home();
    if d.name != name {
        bail!(
            "declaration {} names service `{}` but was loaded as `{name}`",
            p.display(),
            d.name
        );
    }
    // Checked HERE so that no verb can act on a contradictory
    // declaration. See `Declaration::validate`.
    d.validate().with_context(|| format!("in {}", p.display()))?;
    Ok(d)
}

// ═══════════════════════════════════════ THE DECLARATION A BUILD CARRIES ════
//
// A declaration is per ROOT, not per version: `load` reads `$CAULDRON_HOME/
// services/<svc>.toml` then the shipped file beside the supervisor's build,
// with no version check, and the daemon re-reads it on every poll and uses it
// at every launch, restart and rollback. Measured on 2026-10-10 (R3.S3, the
// tts cut-over): the moment a migrated service's new declaration landed in the
// shipped directory, the PINNED OLD build relaunched under the new declaration
// and died — old = hosted artifact with `TTS_HOME`, `[launch] addr`,
// `probe = "connect"`; new = registering cargo source, `probe = "seam"`.
// Worse, `auto_rollback` re-pinned the previous good label and launched it
// under the SAME new declaration, so a rollback that restored a binary
// without its contract turned one dead daemon into two.
//
// So a build directory now carries the declaration it was built under, byte
// for byte, and every launch of a PINNED version reads THAT copy. The shipped
// file governs `build` — and services that are not pinned, which have no
// build directory to carry anything.

/// The file name of a build directory's declaration snapshot:
/// `data/shared/builds/<service>/<label>/declaration.toml`.
pub const DECLARATION_SNAPSHOT: &str = "declaration.toml";

/// sha256, lower-case hex. The one hash the snapshot mechanism uses, named
/// once: it is checked at every launch, so it is a real digest and not the
/// FNV fingerprint a version LABEL used to be.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let d = sha2::Sha256::digest(bytes);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// What a build directory says about the declaration it was built under.
pub enum Snapshot {
    /// `declaration.toml` is there, its sha256 is the one `build.json`
    /// records, and it loads. `source` is the path the bytes were copied
    /// from — the shipped file or the root override — as `build.json` has it.
    Present { decl: Declaration, sha256: String, source: String },
    /// No `declaration.toml` in the build directory. Either the build predates
    /// snapshots (prod's four at the time of writing: threads
    /// `5a17b66ba4ef-release`, `9235e8e1943f-release`; tts
    /// `008d966dca5c-release`, `2303409776b0-dev`) or somebody deleted the
    /// file on purpose — which is the documented one-line reversal of a
    /// backfill. `recorded_sha256` is non-empty in the second case: the
    /// record still names a hash and the file is gone, and `status` says so.
    Absent { recorded_sha256: String },
}

/// **Read the declaration a build directory carries, checked against its
/// record.** `Ok(Absent)` is a real answer and the caller decides what it
/// means (the launch path falls back to the shipped file and SAYS so).
/// `Err` is a refusal: a snapshot that is present but cannot be trusted is
/// never launched under, because a declaration is the contract the binary
/// runs under and garbage is not a contract.
pub fn snapshot(build_dir: &Path, rec: &BuildRecord, name: &str) -> Result<Snapshot> {
    let p = build_dir.join(DECLARATION_SNAPSHOT);
    let text = match std::fs::read(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Snapshot::Absent { recorded_sha256: rec.declaration_sha256.clone() });
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
    };
    let actual = sha256_hex(&text);
    if rec.declaration_sha256.trim().is_empty() {
        bail!(
            "REFUSED: {} exists but {}'s build.json records no `declaration_sha256`, so the \
             snapshot cannot be checked against anything. A declaration a launch cannot verify \
             is not launched under. `cauldron build --snapshot-declaration {name} {} --force` \
             writes a snapshot AND records its hash; or delete the file to fall back to the \
             shipped declaration.",
            p.display(),
            build_dir.display(),
            rec.label
        );
    }
    if actual != rec.declaration_sha256 {
        bail!(
            "REFUSED: the declaration snapshot {} has sha256 {actual} but build.json records \
             sha256 {} for it. The file has been changed since `cauldron build` wrote it, and \
             a launch is not going to run version {} under a declaration that is not the one it \
             was built under. Restore the file, re-snapshot it deliberately \
             (`cauldron build --snapshot-declaration {name} {} --force`), or delete it to fall \
             back to the shipped declaration.",
            p.display(),
            rec.declaration_sha256,
            rec.label,
            rec.label
        );
    }
    let text = String::from_utf8(text)
        .with_context(|| format!("{} is not UTF-8", p.display()))?;
    let decl = parse(&text, &p, name)
        .with_context(|| format!("the declaration snapshot in {}", build_dir.display()))?;
    Ok(Snapshot::Present { decl, sha256: actual, source: rec.declaration_source.clone() })
}

/// **Write `declaration.toml` into a build directory** — a temporary name and
/// a rename, so a launch reading the directory never sees a torn file — and
/// return its sha256 for the record. The caller writes the record; this
/// function writes nothing else and refuses nothing: whether an existing
/// snapshot may be replaced is the VERB's decision (`build` always may, for
/// the directory it is installing into; the backfill verb needs `--force`).
pub fn write_snapshot(build_dir: &Path, text: &str) -> Result<String> {
    let p = build_dir.join(DECLARATION_SNAPSHOT);
    let tmp = build_dir.join(format!(".{DECLARATION_SNAPSHOT}.{}", std::process::id()));
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &p).with_context(|| format!("renaming into {}", p.display()))?;
    Ok(sha256_hex(text.as_bytes()))
}

/// **R9.C7f3 (C-7, ledger R9.C7f2-1): replace `path` whole, never in place.**
/// The text goes to a temporary sibling (same directory, so the rename cannot
/// cross a filesystem) and is renamed over `path`. A reader therefore sees the
/// old file or the new one, never a torn one. `env.toml` is read by the holder
/// every pass and written by the CLI and the holder: a truncate-and-write
/// (`std::fs::write`) let a reader parse a prefix — one that can still be valid
/// TOML missing the services after the cut — and a writer that then saved
/// what it read dropped those pins. The temporary is unique per process and
/// call, and is removed when the write or the rename fails.
///
/// **R9.C7f4 (C-7, inspect R9.C7f3 F2): and DURABLE.** The temporary is
/// fsynced before the rename and the directory after it, so a power loss
/// leaves the old file or the new one — never an empty `env.toml` whose
/// rename reached the disk before its data did.
pub fn write_atomically(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow::Error::new(e).context(format!("writing {}", tmp.display())));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        anyhow::Error::new(e).context(format!("renaming {} over {}", tmp.display(), path.display()))
    })?;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("syncing the directory {} after replacing {}", dir.display(), path.display()))
}

/// The result of scanning every declaration file: the ones that load, and
/// the ones that do not, each with the reason it did not.
///
/// **A broken declaration is a FACT ABOUT ONE SERVICE, not about the scan.**
/// Before this, `all` was a `Result` collect — one unparsable file, one
/// misnamed file, one that failed `validate` — and every bare verb
/// (`cauldron status`, `start`, `stop` with no service named) died with it,
/// taking every healthy service's answer down too. The daemon never made that
/// mistake: its pinned loop loads per service and `error_at!`s the one that
/// fails (`cli/src/daemon.rs`, the `pinned` loop). The CLI now gets the same
/// shape, and the type forces each caller to say what a broken file means
/// for ITS verb rather than letting `?` decide for all of them.
pub struct Scan {
    /// Every declaration that loaded, sorted by name, root shadowing shipped.
    pub ok: Vec<Declaration>,
    /// Every declaration that did not: `(service name, why)`, sorted by name.
    pub broken: Vec<(String, anyhow::Error)>,
}

/// Every declared service, root overrides shadowing shipped ones — the
/// loadable ones in `ok`, the rest named in `broken` with their errors.
pub fn all(root_services: &Path) -> Scan {
    let mut names: Vec<String> = Vec::new();
    for dir in search_paths(root_services) {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("toml") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    if !names.iter().any(|n| n == stem) {
                        names.push(stem.to_string());
                    }
                }
            }
        }
    }
    names.sort();
    let mut scan = Scan { ok: Vec::new(), broken: Vec::new() };
    for n in &names {
        match load(root_services, n) {
            Ok(d) => scan.ok.push(d),
            Err(e) => scan.broken.push((n.clone(), e)),
        }
    }
    scan
}

// --------------------------------------------------------------- the manifest

/// `<data_root>/env.toml` — the environment's version pins **and its
/// supervision policy**.
///
/// Two sections, and they answer the two questions an environment gets to
/// answer for itself: *which version of each service runs here* (`[services.
/// <name>]`) and *how hard this environment tries to stay up* (`[supervisor]`).
/// What a service IS stays in `services/<name>.toml`, shared by every
/// environment, because that does not change when you change rooms.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Manifest {
    /// **THIS ENVIRONMENT'S SUPERVISION POLICY.** See [`Supervisor`].
    #[serde(default)]
    pub supervisor: Supervisor,
    #[serde(default)]
    pub services: BTreeMap<String, Pin>,
}

/// **`[supervisor]` — how hard this environment tries to stay up.**
///
/// The operator's ruling, verbatim: *"the processes can be separate for each
/// environment, and we can configure which are supposed to restart
/// automatically in the env config. E.g. prod should auto-restart on computer
/// restart or failure, with auto rollback to the previous version as needed."*
///
/// Three sentences and three facts, so this struct has exactly those three
/// things in it and the defaults are the safe end of each.
///
/// # Why [`Supervisor::auto_restart`] is ONE key for two layers
///
/// *"on computer restart or failure"* is two mechanisms — launchd bringing
/// cauldron back at login, and cauldron bringing a service back when it dies —
/// and it is one decision. An environment that wants to come back at boot but
/// not to relaunch a crashed service, or the reverse, has not been asked for
/// and would be two keys for one sentence. So this key is read in both places:
/// [`crate::launchd`] writes it into the generated plist as `RunAtLoad` and
/// `KeepAlive`, and the daemon reads it before relaunching a child.
///
/// # Why the defaults are `false`
///
/// Because a new environment is a TEST environment. `cauldron test` and every
/// proof script create one by naming it, and an environment that silently
/// started relaunching things and installing launchd jobs the moment it
/// existed would be a supervisor the operator did not ask for. `prod`'s
/// `env.toml` says `true` because somebody wrote `true` in it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Supervisor {
    /// **Does this environment come back by itself?**
    ///
    /// `true`: `cauldron install` writes a launchd job with `RunAtLoad` and an
    /// unconditional `KeepAlive`, so the holder returns at login and after any
    /// exit; and the daemon relaunches a hosted service that has gone away.
    ///
    /// `false`: `cauldron install` still writes the job, but with `RunAtLoad`
    /// and `KeepAlive` false — the environment is held only while somebody is
    /// holding it — and the daemon WATCHES without relaunching, reporting a
    /// service that went down and leaving it down. Watching without acting is
    /// a real and useful mode: it is what a test environment wants, and it is
    /// the mode in which a restart loop is diagnosable.
    #[serde(default)]
    pub auto_restart: bool,
    /// **When the pinned version will not come up, does this environment fall
    /// back to the previous one that did?**
    ///
    /// `true`: after [`Supervisor::max_start_failures`] consecutive failures to
    /// bring a service up on its pinned version, the daemon re-pins the
    /// environment to [`Manifest::previous_good`] and launches that. The
    /// re-pin is WRITTEN to `env.toml`, so the rollback survives the daemon
    /// and the operator can read what happened in the file he already reads.
    ///
    /// `false`: the daemon keeps trying the pinned version, with backoff, and
    /// says so. It never changes what the operator pinned.
    ///
    /// Requires `auto_restart`: an environment that does not relaunch cannot
    /// reach a start failure to count, and a rollback that re-pins without
    /// launching would be a silent edit to his file. Checked by
    /// [`Supervisor::validate`].
    #[serde(default)]
    pub auto_rollback: bool,
    /// How many consecutive failed starts of one service before
    /// [`Supervisor::auto_rollback`] fires. After the rollback's own version
    /// fails this many times too, the service is PARKED: the daemon stops
    /// trying and keeps saying so. Parking rather than looping is the old
    /// supervisor's one unambiguously right decision and it is kept.
    #[serde(default = "default_max_start_failures")]
    pub max_start_failures: u32,
    /// Seconds between liveness checks on the services this environment
    /// hosts. The old supervisor's 4, kept: a stall is not a death, and the
    /// interval is what gives a slow boot room to finish.
    #[serde(default = "default_poll_secs")]
    pub poll_secs: u64,
    /// How many consecutive failed liveness checks before a service is
    /// declared down. **Not one**, deliberately: the single most expensive
    /// misreading available to a supervisor is treating a stall as a death and
    /// launching a competitor, and this number is the whole of the defence.
    #[serde(default = "default_down_after")]
    pub down_after: u32,
    /// `ThrottleInterval` for the generated launchd job: the minimum seconds
    /// between launchd starting the holder twice. A crash loop must not spin.
    #[serde(default = "default_throttle_secs")]
    pub throttle_secs: u64,
    /// **RESTART BACKOFF — the first relaunch is immediate, each one after it
    /// waits twice as long as the last.** After the `n`th consecutive launch of
    /// one service the next is not attempted for
    /// `min(backoff_base_secs * 2^(n-1), backoff_max_secs)` seconds. "Consecutive"
    /// is reset by the service staying up for [`Supervisor::stable_after_secs`].
    ///
    /// Without it a service that boots and then dies — which never counts as a
    /// FAILED start, so never reaches park or rollback — was relaunched every
    /// `down_after * poll_secs` seconds forever: a crash loop at the
    /// supervisor's own poll rate. `0` disables the wait and is legal: it is
    /// the forced-red configuration the restart tests use.
    #[serde(default = "default_backoff_base_secs")]
    pub backoff_base_secs: u64,
    /// The ceiling on a single backoff wait.
    #[serde(default = "default_backoff_max_secs")]
    pub backoff_max_secs: u64,
    /// How long a launched service must stay live before its consecutive-launch
    /// count, and so its backoff, resets to zero.
    #[serde(default = "default_stable_after_secs")]
    pub stable_after_secs: u64,
    /// **The reverse-DNS prefix of this environment's launchd label**
    /// (REL-4.S3). The label is `<label_prefix>.<process name>`, so `prod`
    /// is `<label_prefix>.cauldron` and any other environment is
    /// `<label_prefix>.cauldron-<env>`.
    ///
    /// Absent, `cauldron install` uses the neutral default
    /// (`launchd::DEFAULT_LABEL_PREFIX`) — UNLESS a job for this environment is
    /// already installed under the pre-release prefix, in which case it keeps
    /// that one, so an existing install is never renamed underneath its own
    /// running job. Setting it here pins the choice explicitly; the operator's
    /// machine pins its existing jobs with `label_prefix = "com.broomstick"`.
    ///
    /// REL-4.S3b: `cauldron install` (before writing a plist) and
    /// `cauldron uninstall` (before deleting one) WRITE the prefix they
    /// resolved here when it is absent, so the label stops depending on the
    /// plist file continuing to exist. Nothing else invents it: a plain
    /// `save` (a rollback re-pin) skips it when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label_prefix: Option<String>,
}

impl Supervisor {
    /// The wait before the next launch, given how many consecutive launches
    /// there have already been. Pure, so the schedule is unit-testable.
    pub fn backoff_secs(&self, launches: u32) -> u64 {
        if launches == 0 || self.backoff_base_secs == 0 {
            return 0;
        }
        let shift = (launches - 1).min(32);
        self.backoff_base_secs.saturating_mul(1u64 << shift).min(self.backoff_max_secs)
    }
}

fn default_backoff_base_secs() -> u64 {
    2
}
fn default_backoff_max_secs() -> u64 {
    300
}
fn default_stable_after_secs() -> u64 {
    60
}

fn default_max_start_failures() -> u32 {
    3
}
fn default_poll_secs() -> u64 {
    4
}
fn default_down_after() -> u32 {
    3
}
fn default_throttle_secs() -> u64 {
    10
}

impl Default for Supervisor {
    fn default() -> Self {
        Self {
            auto_restart: false,
            auto_rollback: false,
            max_start_failures: default_max_start_failures(),
            poll_secs: default_poll_secs(),
            down_after: default_down_after(),
            throttle_secs: default_throttle_secs(),
            backoff_base_secs: default_backoff_base_secs(),
            backoff_max_secs: default_backoff_max_secs(),
            stable_after_secs: default_stable_after_secs(),
            label_prefix: None,
        }
    }
}

/// **Is `s` usable as a launchd label prefix?** Dot-separated, non-empty
/// segments of ASCII letters, digits and `-`. No spaces, slashes or quotes:
/// it is spliced into a label and into a file name under `~/Library/LaunchAgents`.
pub fn is_reverse_dns(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|seg| {
            !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

impl Supervisor {
    pub fn validate(&self, env: &str) -> Result<()> {
        if self.auto_rollback && !self.auto_restart {
            bail!(
                "REFUSED: environment `{env}` sets `auto_rollback = true` with \
                 `auto_restart = false`. Rollback is reached by COUNTING failed starts, and an \
                 environment that does not relaunch a service never has a failed start to \
                 count — so this combination would either do nothing at all or re-pin the \
                 operator's `env.toml` without launching anything, which is a silent edit to a \
                 file he reads. Set `auto_restart = true`, or drop the rollback."
            );
        }
        if let Some(p) = &self.label_prefix {
            if !is_reverse_dns(p) {
                bail!(
                    "REFUSED: environment `{env}` sets `label_prefix = {p:?}`, which is not a \
                     reverse-DNS prefix (dot-separated segments of letters, digits and `-`, \
                     e.g. \"ai.broomstick\"). It becomes a launchd label and a plist file name."
                );
            }
        }
        if self.poll_secs == 0 {
            bail!("REFUSED: environment `{env}` sets `poll_secs = 0`, which is a spin, not a poll.");
        }
        if self.down_after == 0 {
            bail!(
                "REFUSED: environment `{env}` sets `down_after = 0`, which would declare every \
                 service down without checking anything. One is the minimum and three is the \
                 default, because a stall is not a death."
            );
        }
        if self.backoff_base_secs > self.backoff_max_secs {
            bail!(
                "REFUSED: environment `{env}` sets `backoff_base_secs = {}` above \
                 `backoff_max_secs = {}`, so the first wait would already exceed the ceiling.",
                self.backoff_base_secs,
                self.backoff_max_secs
            );
        }
        if self.max_start_failures == 0 {
            bail!(
                "REFUSED: environment `{env}` sets `max_start_failures = 0`, which would park \
                 or roll back before trying once."
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Pin {
    /// A build label, or the literal `latest` (newest build in the root).
    pub version: String,
    /// **The labels this environment has PROVEN GOOD, newest first.**
    ///
    /// A version is good in an environment when it was started there and
    /// `cauldron start` called it started — which already means two handshakes
    /// with the running process: it registered its own seam carrying this
    /// start's boot token, and it answered for its warnings shape on its own
    /// socket. Nothing else is accepted as evidence, and in particular nothing
    /// a declaration or a build record *says* counts: good means it ran here.
    ///
    /// **It is per environment and that is the point.** A version proven good
    /// in `dev` is not thereby good in `prod`, because the thing that differs
    /// between them is the data root, and the data root is what a bad build
    /// breaks. `prod`'s list is written by `prod`'s own successful starts.
    ///
    /// `good[0]` is the current good version and `good[1]` is the PREVIOUS
    /// good version — the one [`Manifest::previous_good`] returns and
    /// `cauldron rollback` re-pins. Bounded by [`GOOD_HISTORY`], so this file
    /// cannot grow without limit on a machine that restarts often.
    #[serde(default)]
    pub good: Vec<String>,
    /// **THIS ENVIRONMENT'S ADDRESS FOR A HOSTED SERVICE**, overriding the
    /// declaration's `[launch] addr`.
    ///
    /// A registering service binds `127.0.0.1:0` and needs nothing here: two
    /// environments cannot collide on a kernel-assigned port. A HOSTED service
    /// cannot publish its port, so its declaration names one — and a
    /// declaration is shared by every environment in a root. Without this key
    /// a second environment hosting `threads` would be refused at
    /// `launch_hosted`'s pre-fork connect, because prod already holds 3653.
    ///
    /// So the address is the ENVIRONMENT's fact, written where the
    /// environment's other facts are: `[services.threads] addr =
    /// "127.0.0.1:4653"` in `data/<env>/env.toml`. The child is told the
    /// effective address through `${CAULDRON_HOSTED_ADDR}` /
    /// `${CAULDRON_HOSTED_PORT}` in its declaration's `[env]` values (and the
    /// same names exported as variables), so a declaration can pass the port
    /// on without naming it. Absent, the declaration's address is used, which
    /// is exactly what `prod` does today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    /// **R9.C7f (C-7, E18-15): THIS PIN'S OWN DRAIN TERMS**, written by
    /// `cauldron pin --at-cap/--drain-cap` and read by the holder that drains
    /// the running build for this re-pin. Each field present replaces the
    /// running declaration's `[quiet.drain]` value; absent, the declaration
    /// governs. Bound to the pin it was given with ([`PinDrain::for_version`]),
    /// and CLEARED (R9.C7f2) once that pin completes: the holder drops it after
    /// its re-pin restart or a failed start of the pin, and every re-pin
    /// (`pin`, `rollback`, auto-rollback) drops it through
    /// [`Manifest::set_version`] — so it can never apply again on a later
    /// return to the same version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain: Option<PinDrain>,
}

/// `[services.<name>.drain]` in `env.toml`: a pin's drain override (R9.C7f).
/// Validated by the CLI that writes it and again by the holder that reads it;
/// this type only carries it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct PinDrain {
    /// The pin (`Pin::version`, as written) this override was given with.
    pub for_version: String,
    /// `handoff` or `extend`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_cap: Option<String>,
    /// Seconds, 1..=3600.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_secs: Option<u64>,
}

impl Default for Pin {
    fn default() -> Self {
        Self { version: "latest".into(), good: Vec::new(), addr: None, drain: None }
    }
}

/// How many proven-good labels an environment remembers per service.
///
/// Rollback needs exactly one — the previous — so anything beyond that is for
/// the operator to read, not for the mechanism to use. Ten is enough history to
/// see a pattern and small enough that `env.toml` stays a file he can read.
pub const GOOD_HISTORY: usize = 10;

/// **R9.C7f4 (C-7, ledger R9.C7f3-2): the env.toml writers' lock.** Held
/// from the load to the save of every read-modify-write of an environment's
/// manifest — `cauldron pin`, `rollback`, `start`, `test`, the holder's
/// auto-rollback, good-recording and `pin_completed` — so two writers can no
/// longer both load the same file and have the second save drop the first's
/// change. An exclusive `flock` on a sibling file (`.env.toml.lock`), not on
/// env.toml itself: the atomic save REPLACES env.toml's inode, so a lock on
/// it would be a lock on a file nobody opens next. The sibling is never
/// removed (removing it would let a waiter hold a lock on an unlinked inode
/// while a newcomer locks a fresh one). Released when this value drops,
/// and by the kernel when the process dies.
#[derive(Debug)]
pub struct ManifestLock {
    _file: std::fs::File,
}

impl Manifest {
    fn open_lock(path: &Path) -> Result<(std::fs::File, PathBuf)> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let lp = path.with_file_name(format!(".{name}.lock"));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lp)
            .with_context(|| format!("opening the env.toml lock {}", lp.display()))?;
        Ok((file, lp))
    }

    /// Take the exclusive lock for the manifest at `path` (blocking). For a
    /// CLI verb, which the operator can interrupt; the holder never calls
    /// this — it uses [`Manifest::lock_within`].
    pub fn lock(path: &Path) -> Result<ManifestLock> {
        let (file, lp) = Self::open_lock(path)?;
        file.lock().with_context(|| format!("locking {}", lp.display()))?;
        Ok(ManifestLock { _file: file })
    }

    /// **R9.C7f5 (C-7, inspect R9.C7f4 low 2): the lock, or an error after
    /// `bound`.** Polls a non-blocking `try_lock` until `bound` has passed, so
    /// a writer that is stuck holding the lock (a `cauldron pin` stopped with
    /// Ctrl-Z, under a debugger) costs the caller at most `bound`, never its
    /// liveness. The error says the lock was busy; the caller retries on its
    /// next pass.
    pub fn lock_within(path: &Path, bound: std::time::Duration) -> Result<ManifestLock> {
        let (file, lp) = Self::open_lock(path)?;
        let deadline = std::time::Instant::now() + bound;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(ManifestLock { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
                        bail!(
                            "the env.toml lock {} is held by another writer; gave up after {}ms \
                             (retried on the next pass)",
                            lp.display(),
                            bound.as_millis()
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(e).with_context(|| format!("locking {}", lp.display()));
                }
            }
        }
    }

    /// [`Manifest::update`] under [`Manifest::lock_within`]: the holder's
    /// read-modify-write, which may fail with "busy" but never waits past
    /// `bound` for the lock.
    pub fn update_within<T>(
        path: &Path,
        bound: std::time::Duration,
        f: impl FnOnce(&mut Manifest) -> Result<T>,
    ) -> Result<T> {
        let _held = Self::lock_within(path, bound)?;
        let mut m = Self::load(path)?;
        let out = f(&mut m)?;
        m.save(path)?;
        Ok(out)
    }

    /// **R9.C7f5 (C-7, inspect R9.C7f4 low 1): first-create, under the
    /// writers' lock.** Writes `Manifest::default()` to `path` only if no
    /// manifest is there once the lock is held, and says whether it did. The
    /// existence check and the save are one critical section with every
    /// other env.toml writer, so a `cauldron pin` that landed between a
    /// caller's own unlocked `exists()` and this call is never overwritten by
    /// an empty manifest. The directory holding `path` must exist.
    pub fn create_if_absent(path: &Path) -> Result<bool> {
        let _held = Self::lock(path)?;
        if std::fs::symlink_metadata(path).is_ok() {
            return Ok(false);
        }
        Self::default().save(path)?;
        Ok(true)
    }

    /// Load, change and save the manifest at `path` under [`Manifest::lock`].
    /// `f` returning an error saves nothing.
    pub fn update<T>(path: &Path, f: impl FnOnce(&mut Manifest) -> Result<T>) -> Result<T> {
        let _held = Self::lock(path)?;
        let mut m = Self::load(path)?;
        let out = f(&mut m)?;
        m.save(path)?;
        Ok(out)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let m: Self = match std::fs::read_to_string(path) {
            Ok(t) => toml::from_str(&t)
                .with_context(|| format!("parsing the environment manifest {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        // The policy is validated on the way IN, so no reader of a `Manifest`
        // has to wonder whether it is coherent and no writer can save an
        // incoherent one past a round trip.
        m.supervisor
            .validate(&path.display().to_string())
            .with_context(|| format!("in {}", path.display()))?;
        Ok(m)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        self.supervisor.validate(&path.display().to_string())?;
        let header = "# The environment's version pins AND its supervision policy.\n\
                      #\n\
                      # [supervisor] is how hard THIS environment tries to stay up.\n\
                      #   auto_restart = true   launchd brings this environment's cauldron back\n\
                      #                         at login and after any exit, and cauldron\n\
                      #                         relaunches a service that has gone away.\n\
                      #   auto_rollback = true  after `max_start_failures` failed starts of a\n\
                      #                         service, re-pin to the previous version that was\n\
                      #                         good HERE and launch that. The re-pin is written\n\
                      #                         into this file, so a rollback is something you\n\
                      #                         read rather than something you are told.\n\
                      # Both default to false, because a new environment is a test environment.\n\
                      #\n\
                      # [services.<name>] `version` is a build label printed by\n\
                      # `cauldron build <service>`, or the literal `latest`. This file is the\n\
                      # per-seam version knob: edit one line to bump one service in one\n\
                      # environment, and nothing else moves. A service runs in this environment\n\
                      # IF AND ONLY IF it is pinned here: presence is the pin, and there is no\n\
                      # second list.\n\n";
        let body = toml::to_string_pretty(self)?;
        write_atomically(path, &format!("{header}{body}"))
    }
    /// **The version this environment pins `service` to, or `None` because it
    /// does not run it at all.**
    ///
    /// # Why this is an `Option` and not a `String` with a default
    ///
    /// It used to return `"latest"` for a service that is absent from
    /// `env.toml`, and that is the SAME CONFLATION the `ensure_manifest`
    /// ruling deleted from the code path — *"declaration is the only source
    /// of truth"*, and the semantics that followed it: **a service runs in an
    /// environment if and only if it is pinned there — presence is the pin,
    /// and there is no second list. A declaration says what a service IS, not
    /// that this environment runs it.**
    ///
    /// The deletion landed in the code path and survived in the
    /// OPERATOR-VISIBLE SURFACE, which is worse: `cauldron status --json`
    /// reported `pinned = "latest"` for `threads` and `tts` in an environment
    /// that pins neither, `cauldron versions` told him a service he does not
    /// run "is pinned to `latest`", and `cauldron pin` announced a first pin
    /// as `latest -> <label>`, naming a prior pin that never existed. Three
    /// surfaces, one default.
    ///
    /// So the fallback is REMOVED rather than corrected at each surface, and
    /// the return type is what enforces it: there is no longer a value this
    /// function can return that reads as a version for a service that has
    /// none. A caller that genuinely wants `latest` — the LAUNCH path, where
    /// "no pin named" really does mean "resolve the newest build" — says so
    /// in its own words at its own call site, where the choice is visible.
    ///
    /// `latest` is still a legal PIN: it is what `Pin::default()` carries and
    /// what an operator may write by hand. The difference this type draws is
    /// between *this environment pins the question `latest`* and *this
    /// environment names no version at all*, which no `String` could hold.
    pub fn pin(&self, service: &str) -> Option<String> {
        self.services.get(service).map(|p| p.version.clone())
    }

    /// **The address THIS environment hosts `service` on, if it overrides the
    /// declaration's.** See [`Pin::addr`]. `None` means "use `[launch] addr`",
    /// which is what every environment did before environments could run side
    /// by side, and what `prod` still does.
    pub fn hosted_addr(&self, service: &str) -> Option<String> {
        self.services.get(service).and_then(|p| p.addr.clone())
    }

    /// **Record that `label` ran successfully in this environment.**
    ///
    /// Idempotent at the head: starting the same version twice does not push a
    /// second entry, because "good" is a property of a version and not a count
    /// of boots. A label that appears further down the list is MOVED to the
    /// head rather than duplicated — re-proving an older version good is real
    /// news and makes it the thing to roll back TO next time, and a history
    /// with the same label twice could not answer "what was the previous one"
    /// at all.
    pub fn record_good(&mut self, service: &str, label: &str) {
        if label.is_empty() || label == "latest" {
            // `latest` is a question, not an answer: it resolves to a different
            // build tomorrow. Recording it as a known-good version would make
            // rollback re-pin a moving target, which is the one thing a
            // rollback target may not be.
            return;
        }
        let p = self.services.entry(service.to_string()).or_default();
        if p.good.first().map(|g| g == label).unwrap_or(false) {
            return;
        }
        p.good.retain(|g| g != label);
        p.good.insert(0, label.to_string());
        p.good.truncate(GOOD_HISTORY);
    }

    /// The version this environment ran successfully BEFORE the current good
    /// one, or `None` when there is no such thing.
    ///
    /// `None` is a real answer and the caller must say so rather than pick
    /// something: an environment with one good version has nothing to roll back
    /// to, and a rollback that silently re-pins the version you are already on
    /// is a rollback that reports success and changes nothing.
    pub fn previous_good(&self, service: &str) -> Option<String> {
        self.services.get(service).and_then(|p| p.good.get(1).cloned())
    }

    /// **THE VERSION TO ROLL BACK TO, GIVEN WHAT IS PINNED NOW.**
    ///
    /// The newest label this environment has proven good that is NOT the one
    /// currently pinned.
    ///
    /// # Why this is not [`Manifest::previous_good`], which it was
    ///
    /// Because `previous_good` is `good[1]`, and that is the right answer to
    /// only one of the two situations a rollback happens in.
    ///
    /// * **The pin IS `good[0]`** — a version that started here, ran, and
    ///   turned out to be bad for some reason a start could not catch. Then
    ///   the thing before it is `good[1]`, and `previous_good` is correct.
    /// * **The pin was never good at all** — it will not start, so it never
    ///   entered the history. Then the newest good version *is* `good[0]`, and
    ///   `previous_good` skips straight past the very version that works.
    ///
    /// The second case is the one the daemon's `auto_rollback` is for, and
    /// using `previous_good` for it was a real defect found by proving it: a
    /// freshly promoted broken version in an environment with ONE good version
    /// behind it had no rollback target at all, so the service parked while a
    /// known-good version sat in the history unused. The symptom was a proof
    /// asserting `AUTO-ROLLBACK` and never seeing it.
    ///
    /// This function answers both, because "not the current pin" is the whole
    /// of the distinction: it returns `good[0]` when the pin is not good, and
    /// `good[1]` when the pin is `good[0]`. `previous_good` is kept for what
    /// it genuinely describes — the shape of the history, which
    /// `cauldron versions` prints as "good (PREVIOUS — rollback target)".
    ///
    /// `None` is a real answer and the caller must say so rather than pick
    /// something: an environment whose only good version is the one already
    /// pinned has nowhere to go, and a rollback that re-pins what you are on
    /// and reports success is a rollback that did nothing.
    ///
    /// # A release build first (R9.C4, gap C-4)
    ///
    /// When a RELEASE build other than the pin is on the good list, the
    /// target is the newest such one — even when a dev or unstamped build was
    /// proven good more recently. tts was pinned to a `-release` build with
    /// only dev and unstamped builds behind it, so a rollback would have
    /// moved the operator's service onto a debug build. A release build is a
    /// label ending in exactly `-release` ([`is_release_label`]); a dirty one
    /// (`-release-dirty.<x>`) is not, because its commit does not name it.
    ///
    /// **When no release build other than the pin is good**, the target is
    /// the newest non-pinned good build of whatever kind — dev, dirty or
    /// unstamped — which is what this always returned. A build proven good
    /// HERE is a better place to land than a park: the service runs, slower
    /// perhaps, and the re-pin is written to `env.toml` where he reads it.
    pub fn rollback_target(&self, service: &str, pinned: &str) -> Option<String> {
        let p = self.services.get(service)?;
        let mut candidates = p.good.iter().filter(|g| g.as_str() != pinned);
        p.good
            .iter()
            .find(|g| g.as_str() != pinned && is_release_label(g))
            .or_else(|| candidates.next())
            .cloned()
    }

    /// Every proven-good label for `service`, newest first.
    pub fn good(&self, service: &str) -> Vec<String> {
        self.services.get(service).map(|p| p.good.clone()).unwrap_or_default()
    }

    /// Re-pin `service` to `label`. Creates the entry if the service has never
    /// been pinned in this environment.
    ///
    /// **R9.C7f2 (C-7, E19-8): a re-pin ENDS the previous pin's drain
    /// override.** Every path that names a version — `pin`, `rollback`, the
    /// daemon's auto-rollback, `test` — goes through here, so the override is
    /// dropped on all of them. Without this, an override given for pin X
    /// stayed in `env.toml` and came back into force on any later return to X
    /// (rollback, auto-rollback, pin back), applying to a drain it was never
    /// given for. `cauldron pin --at-cap/--drain-cap` writes its own override
    /// AFTER calling this.
    pub fn set_version(&mut self, service: &str, label: &str) {
        let p = self.services.entry(service.to_string()).or_default();
        p.version = label.to_string();
        p.drain = None;
    }

    /// **R9.C7f2: the pin `for_version` has COMPLETED, so its drain override
    /// is spent.** Clears `service`'s override when it was given for that pin
    /// AND that is still the pin (an override a newer `pin` wrote is left
    /// alone). `true` when something was cleared, so the caller saves.
    pub fn clear_pin_drain(&mut self, service: &str, for_version: &str) -> bool {
        match self.services.get_mut(service) {
            Some(p) if p.version == for_version
                && p.drain.as_ref().map(|d| d.for_version == for_version).unwrap_or(false) =>
            {
                p.drain = None;
                true
            }
            _ => false,
        }
    }
}

/// **Is `label` a clean RELEASE build?** `label_for` names a build
/// `<commit12>-<profile>` when clean and `<commit12>-<profile>-dirty.<x>` when
/// not; an image that cannot state a commit is `unstamped-<fingerprint>`. Only
/// the first, with the `release` profile, is a release build.
pub fn is_release_label(label: &str) -> bool {
    label.ends_with("-release")
}

/// What a build label directory records about itself.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BuildRecord {
    pub service: String,
    /// The build's name, and since commits became the version identity it is
    /// DERIVED from [`BuildRecord::commit`] — see `label_for` in the CLI. It
    /// stays a separate field because it is also a directory name under
    /// `data/shared/builds/<service>/`, and a dirty build needs a label the
    /// commit alone cannot give it.
    pub label: String,
    pub bin: String,
    pub package: String,
    pub profile: String,
    pub workspace: String,
    pub built_at: u64,
    /// **The commit this build came from. The version identity.**
    ///
    /// `serde(default)` for one reason only: build records already written to
    /// `data/shared/builds/` before this field existed must still PARSE, so
    /// that an operator with older builds in his root gets a reading of them
    /// rather than an error. An empty commit on a record read back is therefore
    /// a real state — it means *this build predates commit identity* — and
    /// [`BuildRecord::names_a_commit`] is how a caller asks instead of
    /// guessing. Nothing writes it empty.
    #[serde(default)]
    pub commit: String,
    /// Was the working tree dirty when this was built? A dirty build's commit
    /// does not identify it, which is why the label says so too.
    #[serde(default)]
    pub dirty: bool,
    /// The FNV-1a fingerprint of the installed bytes.
    ///
    /// **Kept, and demoted.** This used to BE the label, i.e. the whole version
    /// identity, and it is retained because it still answers the one question
    /// it was always good for: are these two files the same bytes? What it
    /// could never do is name a version — you cannot look a fingerprint up, put
    /// it in order, diff it, or ask a repository anything about it. So it is now
    /// a property OF a build rather than the name of one.
    #[serde(default)]
    pub content: String,
    /// The old field, still written, so anything already reading it keeps
    /// working. It is [`BuildRecord::commit`] in an `Option`, and `commit` is
    /// the one to read.
    pub source_commit: Option<String>,
    /// **sha256 of the `declaration.toml` this build directory carries** —
    /// the exact bytes of the file `cauldron build` resolved (root override
    /// first, then shipped) at the moment it built. Checked at every launch
    /// by [`snapshot`]. `serde(default)` for the same reason `commit` is:
    /// records written before R3.S2.0b must still parse, and an empty value
    /// on read-back MEANS "this build predates snapshots" — see
    /// [`Snapshot::Absent`]. Nothing writes it empty.
    #[serde(default)]
    pub declaration_sha256: String,
    /// The path the snapshot was copied FROM, for a reader asking "which
    /// file was in force when this was built". Informational: the snapshot
    /// is the contract, this names its provenance.
    #[serde(default)]
    pub declaration_source: String,
}

impl BuildRecord {
    /// **Does this record name the commit it was built from?**
    ///
    /// `false` for a build installed before commits were the identity. Asked
    /// rather than assumed, because the honest handling of such a record is to
    /// say what it is — a version nobody can trace to a source — and not to
    /// treat an empty string as a commit.
    pub fn names_a_commit(&self) -> bool {
        !self.commit.trim().is_empty()
    }

    /// The commit, short, with dirtiness visible — for one column of a table.
    pub fn commit_short(&self) -> String {
        if !self.names_a_commit() {
            return "-".into();
        }
        let short: String = self.commit.chars().take(12).collect();
        if self.dirty {
            format!("{short}+dirty")
        } else {
            short
        }
    }
}

/// **A per-environment copy's label: `<base>+<env>`, then `<base>+<env>.<n>`.**
///
/// REL-4.S8c. `cauldron build --env-copy` installs a build under one of these
/// instead of under `<base>` (the commit and the profile), because `<base>` is
/// SHARED by every environment in the root and another one — prod — may be
/// pinned to it: rebuilding it there renames fresh, unsigned bytes over the
/// image prod's next launch execs. A `+<env>` label is one no other
/// environment pins. `n` 1 is the bare `+<env>`.
pub fn env_copy_label(base: &str, env_name: &str, n: u32) -> String {
    if n <= 1 {
        format!("{base}+{env_name}")
    } else {
        format!("{base}+{env_name}.{n}")
    }
}

/// **Which environment owns this label**, if it is a per-environment copy
/// (`<base>+<env>` or `<base>+<env>.<n>`); `None` for a shared build. A label
/// `label_for` makes never contains `+`, so the first `+` is the split.
pub fn env_copy_owner(label: &str) -> Option<&str> {
    let (_, suffix) = label.split_once('+')?;
    Some(match suffix.rsplit_once('.') {
        Some((owner, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => owner,
        _ => suffix,
    })
}

/// Resolve a pin to a concrete build directory. `latest` is the newest label
/// by record timestamp; anything else must exist exactly, and a missing pin is
/// an error rather than a fallback — an environment that silently ran a
/// different version than it pinned would make the pin worthless.
///
/// **`latest` is per ENVIRONMENT (REL-4.S8c, inspect REL-4.S8b-I N2).** The
/// candidates are the SHARED builds plus `env_name`'s OWN per-environment
/// copies ([`env_copy_owner`]); another environment's copy is never a
/// candidate, so installing `dev` cannot move prod's `latest` onto dev's
/// image. Ties on `built_at` go to the environment's own copy, then to the
/// greater label, so the answer never depends on `readdir` order.
pub fn resolve_version(
    builds_for_service: &Path,
    pin: &str,
    env_name: &str,
) -> Result<(String, BuildRecord)> {
    let read = |label: &str| -> Result<BuildRecord> {
        let p = builds_for_service.join(label).join("build.json");
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("reading the build record {}", p.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))
    };
    if pin != "latest" {
        let rec = read(pin).with_context(|| {
            format!(
                "the environment pins version `{pin}`, which is not built in this root. \
                 Run `cauldron build <service>` and pin the label it prints."
            )
        })?;
        return Ok((pin.to_string(), rec));
    }
    let mut best: Option<(String, BuildRecord)> = None;
    let rd = std::fs::read_dir(builds_for_service).with_context(|| {
        format!(
            "no builds for this service in {}. Run `cauldron build <service>` first.",
            builds_for_service.display()
        )
    })?;
    let key = |label: &str, rec: &BuildRecord| {
        (rec.built_at, env_copy_owner(label).is_some(), label.to_string())
    };
    for e in rd.flatten() {
        if let Some(label) = e.file_name().to_str() {
            if env_copy_owner(label).is_some_and(|owner| owner != env_name) {
                continue;
            }
            if let Ok(rec) = read(label) {
                if best.as_ref().map(|(l, b)| key(label, &rec) > key(l, b)).unwrap_or(true) {
                    best = Some((label.to_string(), rec));
                }
            }
        }
    }
    best.with_context(|| {
        format!(
            "no usable build under {}. Run `cauldron build <service>` first.",
            builds_for_service.display()
        )
    })
}

#[cfg(test)]
mod manifest_save_tests {
    use super::Manifest;
    use std::io::Read;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "env-c7f3-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pinned(services: &[(&str, &str)]) -> Manifest {
        let mut m = Manifest::default();
        for (s, v) in services {
            m.set_version(s, v);
        }
        m
    }

    /// GUARD (R9.C7f3 c): `Manifest::save` REPLACES env.toml, it does not
    /// rewrite it in place. A reader holding the file open across a save (the
    /// holder mid-`load`) still reads the WHOLE old file — under the old
    /// truncate-and-write it read the new text, or a torn prefix of it, through
    /// the same inode. Paired: the path itself reads the new file, so "never
    /// writes" fails too; and no temporary is left beside it.
    #[test]
    fn c7f3_c_a_save_replaces_env_toml_so_an_open_reader_never_sees_a_torn_file() {
        let d = dir("atomic");
        let path = d.join("env.toml");
        let old = pinned(&[("alpha", "a1-release"), ("beta", "b1-release"), ("gamma", "g1-release")]);
        old.save(&path).unwrap();
        let old_text = std::fs::read_to_string(&path).unwrap();
        let mut reader = std::fs::File::open(&path).unwrap();
        let new = pinned(&[("alpha", "a2-release")]);
        new.save(&path).unwrap();
        let new_text = std::fs::read_to_string(&path).unwrap();
        // Sentinel: the two texts are different things.
        assert_ne!(old_text, new_text, "sentinel: two different manifests");
        let mut seen = String::new();
        reader.read_to_string(&mut seen).unwrap();
        assert_eq!(seen, old_text, "a reader open across the save saw something other than the whole old file");
        // Paired: the path is the new file, and it loads with the new pins.
        let back = Manifest::load(&path).unwrap();
        assert_eq!(back.pin("alpha").map(|p| p.to_string()), Some("a2-release".to_string()));
        assert!(back.pin("beta").is_none(), "{new_text}");
        let left: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "env.toml")
            .collect();
        assert!(left.is_empty(), "temporaries left beside env.toml: {left:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// R9.C7f5 (C-7): the bounded lock and the locked first-create.
#[cfg(test)]
mod manifest_lock_c7f5_tests {
    use super::{Manifest, PinDrain};
    use std::time::{Duration, Instant};

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "env-c7f5-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Held by another open file description (a stuck writer), `lock_within`
    /// gives up after its 300 ms bound — and not before it (the sentinel that
    /// the lock was really contended); free, it is taken at once.
    #[test]
    fn c7f5_lock_within_300ms_gives_up_while_held_and_takes_it_when_free() {
        let d = scratch("within");
        let p = d.join("env.toml");
        let stuck = Manifest::lock(&p).unwrap();
        let t0 = Instant::now();
        let e = Manifest::lock_within(&p, Duration::from_millis(300)).unwrap_err();
        let took = t0.elapsed();
        assert!(format!("{e:#}").contains("gave up after 300ms"), "{e:#}");
        assert!(took >= Duration::from_millis(300), "returned before its bound: {took:?}");
        assert!(took < Duration::from_millis(1300), "waited past its bound: {took:?}");
        drop(stuck);
        let t0 = Instant::now();
        let _l = Manifest::lock_within(&p, Duration::from_millis(300)).unwrap();
        assert!(t0.elapsed() < Duration::from_millis(250), "a free lock was not taken at once");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// First-create writes a default manifest only when there is none, and
    /// never replaces one that is there (a pin that landed first survives).
    #[test]
    fn c7f5_create_if_absent_creates_once_and_never_overwrites() {
        let d = scratch("create");
        let p = d.join("env.toml");
        assert!(Manifest::create_if_absent(&p).unwrap(), "did not create an absent manifest");
        assert!(Manifest::load(&p).unwrap().services.is_empty());
        let mut m = Manifest::load(&p).unwrap();
        m.set_version("s1", "v1");
        m.services.get_mut("s1").unwrap().drain = Some(PinDrain { for_version: "v1".into(), at_cap: None, cap_secs: Some(5) });
        m.save(&p).unwrap();
        assert!(!Manifest::create_if_absent(&p).unwrap(), "claimed to create over an existing manifest");
        let back = Manifest::load(&p).unwrap();
        assert_eq!(back.services.get("s1").map(|p| p.version.as_str()), Some("v1"), "the existing pin was overwritten");
        let _ = std::fs::remove_dir_all(&d);
    }
}
