//! **Permission identity** — the code identity a macOS permission prompt names.
//!
//! On this machine a permission prompt reads *"Claude is requesting to access
//! data from other apps"*, and the operator's complaint about it is the reason
//! this module exists: *"I need a way to see what is actually requesting the
//! permission and give fine-grained access, because macos just says 'Claude is
//! requesting to access data from other apps' over and over, and the truth is
//! more nuanced."* The instruction that followed was specific: *"note that the
//! permissions for the claude commands are given to a system process instead of
//! owned by threads. We should actually make this permissioned to cauldron so
//! permission requests all display 'Cauldron is trying to access ...'."*
//!
//! # The mechanism, as measured on this machine rather than as assumed
//!
//! macOS keys TCC consent to the **designated requirement** of the
//! **responsible process**, and responsibility is **inherited by every
//! descendant** unless a child disclaims it. Three consequences, and all three
//! are load-bearing:
//!
//! 1. **The identity on the prompt is not the identity of the thing asking.**
//!    It is the identity of whatever sits at the top of that process's
//!    responsibility chain. Today `threads` and `tts` are started by
//!    `launchd` → `/bin/bash` → `supervise-service.sh` → the daemon, so the
//!    responsible process is a *system binary* — which is exactly the
//!    "permissions are given to a system process" the operator reported.
//!    Giving a service its own stable signature does **not** change this;
//!    responsibility is about the chain, not about the leaf.
//! 2. **An ad-hoc signature is a new application on every rebuild.** Cargo's
//!    linker-signed output names no identifier and pins only a cdhash, so TCC
//!    has nothing durable to record consent against. `tts` is the live example:
//!    its identifier is `tts-adeff24ca3de5dea`, a content hash, and it changes
//!    whenever the binary does. [`Subject::AdHoc`] is that state, and
//!    [`Identity::parse`] refuses to accept such a string as an identity at
//!    all.
//! 3. **A claimed identity is worth nothing; a read one is worth something.**
//!    This crate already holds that commit is the version identity and is read
//!    off the artifact rather than recorded by whoever built it. The same rule
//!    applies here and for the same reason, so [`Subject::read`] asks
//!    `codesign` what is actually on the file and [`Claim::check`] compares that
//!    answer to what the declaration claimed.
//!
//! # What this module does NOT do, deliberately
//!
//! It does not grant anything, and it contains no path by which anything could.
//! TCC's database is protected by SIP and cannot be written by this or any
//! other unprivileged code; the only non-interactive way to pre-grant a
//! permission on macOS is a device management profile, and a standing operator
//! ruling forbids both broad grants and proposing them. So the whole surface
//! here is *naming* and *reporting*: which identity a prompt will name, whether
//! that identity is durable, and whether the artifact agrees with the claim.
//!
//! Making the prompt actually say `Cauldron` additionally requires cauldron to
//! *be* the process that launches the service — the supervisor's job, being
//! built separately. [`Responsibility`] is the declaration field that work will
//! read; this module records the intent so the supervisor has something to
//! honour rather than a decision to re-make.
//!
//! # One identity per ENVIRONMENT, which is what makes a grant divisible
//!
//! The operator's ruling: *"I was thinking one per environment. cauldron sets
//! it per-environment based on the config. Maybe the envs can have their one
//! process name, e.g. 'cauldron' for prod or 'cauldron-<env>' for other envs.
//! That way each has its own signing identity and I can selectively approve
//! permissions per env."*
//!
//! That is the answer to the device-wide permission profile, and it is a better
//! answer than a profile would have been. A grant is made against an identity,
//! so the identity is the unit a decision can be made about. One identity for
//! the machine would mean one decision for the machine — approving the
//! microphone for `prod` would approve it for every throwaway `test`
//! environment an agent creates, with no way to say yes to one and no to the
//! other. [`Identity::for_env`] is where the split lives, and it costs nothing,
//! because the environment already exists and already names itself.
//!
//! It is also the reason this is not a broad grant and does not become one: the
//! number of things he is asked about does not go up, and what each grant
//! covers goes DOWN.
//!
//! # What this model needs FROM the supervisor, stated so it can be honoured
//!
//! Nothing here can put a name on a prompt by itself. Three things are needed
//! from the work that makes cauldron host its services, and they are listed as
//! requirements rather than built as a parallel mechanism:
//!
//! 1. **launchd must start cauldron's own binary, not a shell.** Today
//!    `com.harness.threads` runs `/bin/bash supervise-service.sh threads`, so
//!    the responsible process is `/bin/bash`. A `ProgramArguments` whose first
//!    element is an interpreter makes the interpreter the subject, whatever is
//!    signed further down.
//! 2. **One launchd job and one signed artifact per environment**, named by
//!    [`process_name`] and signed with that environment's own
//!    [`Identity::for_env`]. Two environments sharing one artifact share one
//!    identity, and then one grant covers both — which is the thing the split
//!    exists to prevent.
//! 3. **Cauldron must spawn its services without disclaiming responsibility.**
//!    Inheritance is the default on macOS, so this is a requirement not to do
//!    something: `responsibility_spawnattrs_setdisclaim` must NOT be set for a
//!    hosted service, or the child becomes its own subject and the prompt names
//!    the service again. [`Responsibility::Own`] is the opt-in for the one case
//!    where that is wanted.
//!
//! **A signature here is an IDENTITY, never a gate.** The operator ruled that a
//! signature says who shipped a thing while a commit says what it is, and the
//! registry wants the second. So nothing in this module refuses a launch, and
//! an unsigned or ad-hoc service is reported as a GAP rather than blocked —
//! which also means adding a per-environment identity cannot stop a service
//! starting, and is therefore safe to land before the supervisor exists.

use std::path::Path;

use anyhow::{bail, Context, Result};

/// The reverse-DNS prefix every cauldron identity is built from.
///
/// Deliberately **not** carrying a service's name: the point of the identity is
/// that one name covers every service a given cauldron hosts.
pub const IDENTIFIER_PREFIX: &str = "ai.broomstick";

/// The environment whose cauldron wears the unsuffixed name.
///
/// **This is the one place in this crate where an environment name is special,
/// and it is special by instruction rather than by design.** The operator:
/// *"Maybe the envs can have their one process name, e.g. `cauldron` for prod
/// or `cauldron-<env>` for other envs."* Everywhere else `prod` is just the
/// default name with no special case, and that still holds — what is special
/// here is only the *spelling* of one identifier, which is cosmetic. Nothing
/// branches on it.
pub const UNSUFFIXED_ENV: &str = "prod";

/// Cauldron's process name in a given environment: `cauldron` in `prod`,
/// `cauldron-<env>` elsewhere.
///
/// This is the name that shows up in `ps`, and it is deliberately the same
/// shape as the signing identifier's last component, so that the thing the
/// operator sees in a process list and the thing he approves in a permission
/// dialog are recognisably the same object.
pub fn process_name(env: &str) -> String {
    if env == UNSUFFIXED_ENV {
        "cauldron".to_string()
    } else {
        format!("cauldron-{env}")
    }
}

/// The code-signing identity (the keychain's name for the private key) that can
/// mint a cauldron identity.
///
/// This is the identity the machine already holds and already uses unattended;
/// nothing here creates, imports or modifies a keychain item.
pub const SIGNING_IDENTITY: &str = "harness-codesign";

/// A permission identity: the name a prompt will read.
///
/// Constructed only through [`Identity::parse`], so an identity that could not
/// durably hold consent cannot be built in the first place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    identifier: String,
}

impl Identity {
    /// **Cauldron's identity in one environment** — `ai.broomstick.cauldron` in
    /// `prod`, `ai.broomstick.cauldron-<env>` elsewhere.
    ///
    /// One identity per environment is the operator's ruling and it is the
    /// whole permission mechanism: *"I was thinking one per environment.
    /// cauldron sets it per-environment based on the config... That way each
    /// has its own signing identity and I can selectively approve permissions
    /// per env."*
    ///
    /// Why per-environment rather than one identity for the machine: a grant is
    /// made against an identity, so one identity per machine would mean one
    /// decision for the machine. Granting `prod` the microphone would silently
    /// grant it to every throwaway `test` environment an agent spins up, and
    /// the operator would have no way to say yes to the first and no to the
    /// second. Splitting the identity is what makes the grant divisible, and it
    /// costs nothing: the environment already exists and already names itself.
    pub fn for_env(env: &str) -> Result<Self> {
        Self::parse(&format!("{IDENTIFIER_PREFIX}.{}", process_name(env)))
    }

    /// The identifier, as it appears in a designated requirement.
    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    /// Parse an identifier, refusing the shapes that cannot hold consent.
    ///
    /// The refusals are not stylistic. Each one names a string that macOS would
    /// accept as a signing identifier and that would then fail to do the one
    /// job an identity has.
    pub fn parse(s: &str) -> Result<Self> {
        let id = s.trim();

        if id.is_empty() {
            bail!(
                "REFUSED: an empty string is not a permission identity. The identifier \
                 is what a prompt NAMES and what consent is recorded against; there is \
                 no default, because an identity nobody chose is an identity nobody can \
                 recognise on a dialog."
            );
        }

        if let Some(hash) = cargo_content_hash(id) {
            bail!(
                "REFUSED: `{id}` is cargo's own ad-hoc signing identifier, not a \
                 permission identity — the `{hash}` suffix is a content hash of the \
                 binary, so this name CHANGES ON EVERY REBUILD. macOS keys consent to \
                 the identifier, which makes each rebuild a new application that has to \
                 be approved again. This is the `tts` defect, and accepting the string \
                 here would record it as an identity instead of reporting it."
            );
        }

        if !id.contains('.') {
            bail!(
                "REFUSED: `{id}` is not usable as a permission identity: it carries no \
                 `.` and so is not a reverse-DNS identifier. A prompt shows this string \
                 to the operator, and a bare word is indistinguishable from the \
                 incidental name of whatever binary happened to ask."
            );
        }

        if let Some(c) = id.chars().find(|c| {
            c.is_whitespace() || *c == '"' || *c == '\\' || c.is_control()
        }) {
            bail!(
                "REFUSED: `{id}` contains {c:?}, which cannot appear in a permission \
                 identity. The identifier is embedded verbatim in a designated \
                 requirement expression; a quote, a backslash or whitespace there does \
                 not produce a different identity, it produces an unparseable \
                 requirement."
            );
        }

        Ok(Self { identifier: id.to_string() })
    }
}

/// If `id` is cargo's ad-hoc `<name>-<16 hex>` form, return the hash half.
///
/// Split out and `pub(crate)` so the test suite can force it both ways: a real
/// content hash, and a legitimate identifier that merely ends in a hyphenated
/// word.
pub(crate) fn cargo_content_hash(id: &str) -> Option<&str> {
    let (_, tail) = id.rsplit_once('-')?;
    let is_hash = tail.len() == 16 && tail.bytes().all(|b| b.is_ascii_hexdigit());
    is_hash.then_some(tail)
}

/// **What binds an identifier to a key in a designated requirement.**
///
/// An identifier on its own is not an identity: any key could mint the same
/// name, so consent against it would be consent to a name nothing owns. What
/// makes it durable is the constraint sitting beside it, and there are three
/// real shapes on this machine — all three durable, which is the correction
/// this type exists to carry. The first version of this module recognised only
/// `LeafHash` and therefore reported Apple-signed and Developer-ID-signed
/// binaries as UNSIGNED, including `com.anthropic.claude-code`, which is the
/// single most important identity in this whole subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pin {
    /// `certificate leaf = H"…"` — a specific certificate, by hash. What a
    /// self-signed local identity like `harness-codesign` produces, and what
    /// cauldron's own identity will produce.
    LeafHash(String),
    /// `anchor apple` — signed by Apple itself. A platform binary.
    AppleAnchor,
    /// `anchor apple generic and … certificate leaf[subject.OU] = <team>` — a
    /// Developer ID. `claude` is this, under team `Q6L2SF6YDW`.
    DeveloperId(String),
    /// Some other constraint bound the identifier. Recorded rather than
    /// guessed at: it is durable because something pins it, and this says we
    /// did not recognise what.
    Other,
}

impl Pin {
    pub fn describe(&self) -> String {
        match self {
            Pin::LeafHash(h) => format!("pinned to certificate {}", &h[..h.len().min(12)]),
            Pin::AppleAnchor => "anchored to Apple".to_string(),
            Pin::DeveloperId(team) => format!("Developer ID, team {team}"),
            Pin::Other => "pinned by a constraint this reader does not name".to_string(),
        }
    }
}

/// What macOS will actually record consent against for a given artifact.
///
/// Read off the file with [`Subject::read`]; never recorded by whoever built it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// The designated requirement names an identifier AND binds it to a key.
    /// Consent recorded against this survives a rebuild, because a rebuild
    /// produces the same identifier signed by the same key. `pin` says what
    /// does the binding — see [`Pin`].
    Durable { identity: Identity, pin: Pin },
    /// The requirement pins a cdhash and names no identifier — cargo's
    /// linker-signed output. Every rebuild is a different application to TCC,
    /// so no consent against it is durable.
    AdHoc { cdhash: String },
    /// No signature at all, so nothing to attach consent to.
    Unsigned,
}

impl Subject {
    /// Whether consent recorded against this subject survives a rebuild.
    pub fn is_durable(&self) -> bool {
        matches!(self, Subject::Durable { .. })
    }

    /// The identity a prompt would name, if there is a durable one.
    pub fn identity(&self) -> Option<&Identity> {
        match self {
            Subject::Durable { identity, .. } => Some(identity),
            _ => None,
        }
    }

    /// One line for a report, saying what the subject is and what it costs.
    pub fn describe(&self) -> String {
        match self {
            Subject::Durable { identity, pin } => {
                format!("{} ({})", identity.identifier(), pin.describe())
            }
            Subject::AdHoc { cdhash } => {
                format!("ad-hoc cdhash {cdhash} — a NEW application on every rebuild")
            }
            Subject::Unsigned => {
                "unsigned — nothing for consent to attach to".to_string()
            }
        }
    }

    /// Read the subject off an artifact, by asking `codesign` for its
    /// designated requirement.
    ///
    /// The requirement is the right question and `Identifier=` is the wrong one:
    /// the `Identifier=` line is what the signer *said*, while the requirement
    /// is what the kernel will *check*. The same distinction is already drawn by
    /// the deploy path's `is_stably_signed`.
    pub fn read(binary: &Path) -> Result<Self> {
        if !binary.exists() {
            bail!(
                "REFUSED: cannot read a permission subject off `{}`: no such file. The \
                 subject is a fact about bytes, so there is no answer to give when the \
                 bytes are absent — and guessing one would be the claim this module \
                 exists to avoid.",
                binary.display()
            );
        }

        let out = std::process::Command::new("/usr/bin/codesign")
            .arg("-d")
            .arg("-r-")
            .arg(binary)
            .output()
            .with_context(|| {
                format!("running codesign -d -r- on {}", binary.display())
            })?;

        // codesign writes the requirement to stdout and its notes to stderr,
        // and reports an unsigned file through stderr with a non-zero status.
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(parse_requirement(&text))
    }
}

/// Parse `codesign -d -r-` output into a [`Subject`].
///
/// A pure function over text, so the test suite can force every branch without
/// needing a signed, an ad-hoc and an unsigned binary on hand.
pub fn parse_requirement(text: &str) -> Subject {
    // codesign prints the requirement two different ways and only one of them
    // is bare: for an ad-hoc signature it emits the line as a COMMENT, with a
    // leading `# `, because a cdhash requirement is not something you could
    // usefully re-use as input. Stripping that prefix is what makes the ad-hoc
    // branch below reachable at all; without it every cargo-built binary parses
    // as `Unsigned` and the report blames the wrong thing.
    let designated = text
        .lines()
        .map(str::trim)
        .map(|l| l.strip_prefix("# ").unwrap_or(l))
        .find_map(|l| l.strip_prefix("designated =>"))
        .map(str::trim);

    let Some(req) = designated else {
        return Subject::Unsigned;
    };

    if let Some(identifier) = between(req, "identifier \"", "\"") {
        // An identifier alone is not durable: with nothing binding it, any key
        // could mint the same name, so the requirement names an identity that
        // nothing owns. Any ONE of the three bindings below makes it durable.
        let pin = if let Some(leaf) = between(req, "certificate leaf = H\"", "\"") {
            Some(Pin::LeafHash(leaf.to_string()))
        } else if let Some(team) = between(req, "certificate leaf[subject.OU] = ", " ")
            .or_else(|| after(req, "certificate leaf[subject.OU] = "))
        {
            Some(Pin::DeveloperId(team.trim().to_string()))
        } else if req.contains("anchor apple") {
            Some(Pin::AppleAnchor)
        } else if req.contains("certificate") || req.contains("anchor") {
            Some(Pin::Other)
        } else {
            None
        };

        if let (Some(pin), Ok(identity)) = (pin, Identity::parse(identifier)) {
            return Subject::Durable { identity, pin };
        }
    }

    if let Some(cdhash) = between(req, "cdhash H\"", "\"") {
        return Subject::AdHoc { cdhash: cdhash.to_string() };
    }

    Subject::Unsigned
}

/// Everything after `open`, trimmed. For a constraint that ends the line.
fn after<'a>(hay: &'a str, open: &str) -> Option<&'a str> {
    let rest = hay.split_once(open)?.1.trim();
    (!rest.is_empty()).then_some(rest)
}

fn between<'a>(hay: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let rest = hay.split_once(open)?.1;
    let (inner, _) = rest.split_once(close)?;
    (!inner.is_empty()).then_some(inner)
}

/// Who the TCC subject is for a service cauldron runs.
///
/// This is the field that makes permissions part of cauldron's identity model
/// rather than per-service configuration. Today the equivalent knob lives
/// outside this repo entirely, as `DEPLOY_IDENTIFIER` in
/// `~/.threads-supervisor/services/<name>.conf` — one file per service, with
/// `tts` simply missing from it, which is why `tts` is under no signature gate
/// at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Responsibility {
    /// The service inherits cauldron's identity, so a prompt it provokes names
    /// **Cauldron**. The default, because it is the whole of the operator's ask.
    #[default]
    Inherit,
    /// The service disclaims and becomes its own TCC subject, so a prompt names
    /// the service's own signature.
    ///
    /// Worth having rather than hardcoding `Inherit` for one real reason: a
    /// child that carries a *better* identity than its parent should keep it.
    /// The precedent on this machine is the `claude` spawn seam, where
    /// disclaiming moved consent off a self-signed debug binary and onto
    /// Anthropic's Developer ID signature.
    Own,
}

impl Responsibility {
    /// The declaration spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Responsibility::Inherit => "inherit",
            Responsibility::Own => "own",
        }
    }

    /// Parse the declaration spelling, refusing anything else by name.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim() {
            "inherit" => Ok(Responsibility::Inherit),
            "own" => Ok(Responsibility::Own),
            other => bail!(
                "REFUSED: `{other}` is not a responsibility. It is one of `inherit` \
                 (the service inherits cauldron's identity, so a prompt names Cauldron) \
                 or `own` (the service disclaims and is its own TCC subject). Nothing is \
                 guessed, because a misread value here silently changes which name the \
                 operator is shown on a security dialog."
            ),
        }
    }

    /// The identity a prompt will name under this disposition, in `env`.
    ///
    /// Under `inherit` that is the hosting cauldron's own per-environment
    /// identity, which is the point: the operator approves `prod` without
    /// thereby approving every throwaway environment.
    pub fn subject_identity(&self, env: &str, service_subject: &Subject) -> Option<Identity> {
        match self {
            Responsibility::Inherit => Identity::for_env(env).ok(),
            Responsibility::Own => service_subject.identity().cloned(),
        }
    }
}

/// A service's claim about its own permission identity, and what checking it
/// against the artifact found.
#[derive(Debug, Clone)]
pub struct Claim {
    pub service: String,
    pub responsibility: Responsibility,
    /// The identifier the declaration claims is on the artifact, if it claims
    /// one. Empty is the honest answer when nothing is claimed, exactly as an
    /// empty `commit` is.
    pub claimed: Option<Identity>,
}

/// The verdict of checking a [`Claim`] against what is on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The claim and the artifact agree.
    Agrees { identity: Identity },
    /// Nothing was claimed and nothing durable is on the artifact. Honest, and
    /// the state `tts` is in.
    NoClaimNoIdentity { found: Subject },
    /// Nothing was claimed but the artifact carries a durable identity. Not an
    /// error — the artifact is the authority — but worth naming so the
    /// declaration can be completed.
    Unclaimed { found: Identity },
    /// A claim was made and the artifact does not bear it out. The one verdict
    /// that is a defect.
    Disagrees { claimed: Identity, found: Subject },
}

impl Claim {
    /// Compare the claim to what is actually on the artifact.
    pub fn check(&self, found: &Subject) -> Verdict {
        match (&self.claimed, found.identity()) {
            (Some(claimed), Some(actual)) if claimed == actual => {
                Verdict::Agrees { identity: actual.clone() }
            }
            (Some(claimed), _) => Verdict::Disagrees {
                claimed: claimed.clone(),
                found: found.clone(),
            },
            (None, Some(actual)) => Verdict::Unclaimed { found: actual.clone() },
            (None, None) => Verdict::NoClaimNoIdentity { found: found.clone() },
        }
    }
}

impl Verdict {
    /// Whether this verdict is a defect a report should exit non-zero on.
    ///
    /// Only [`Verdict::Disagrees`] is. A service with no identity at all is a
    /// *gap*, not a violation — the same distinction `cauldron status` already
    /// draws for an adopted seam, and for the same reason: the service never
    /// signed the contract, so counting it as a breach would make the one real
    /// breach unfindable in the noise.
    pub fn is_defect(&self) -> bool {
        matches!(self, Verdict::Disagrees { .. })
    }

    /// One line for a report.
    pub fn describe(&self) -> String {
        match self {
            Verdict::Agrees { identity } => {
                format!("ok        {} on the artifact, as declared", identity.identifier())
            }
            Verdict::NoClaimNoIdentity { found } => {
                format!("gap       no identity claimed and none found: {}", found.describe())
            }
            Verdict::Unclaimed { found } => format!(
                "unclaimed the artifact carries {}, which the declaration does not name",
                found.identifier()
            ),
            Verdict::Disagrees { claimed, found } => format!(
                "DEFECT    declared `{}` but the artifact carries {}",
                claimed.identifier(),
                found.describe()
            ),
        }
    }
}
