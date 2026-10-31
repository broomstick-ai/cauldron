//! # The envelope — the OUTER FRAME every face parses
//!
//! [`crate::wire`] is what comes *through* the door; this module is the door
//! itself. Everything here is something a consumer must be able to PARSE before
//! it can talk to a chassis-served producer at all: the handshake frames, the
//! request/response envelope, the bearer the handshake issues, the closed set of
//! refusal codes, and the `/health` body.
//!
//! It sits in the unflagged floor with `wire`, `mirror`, `operation`, `cell` and
//! `surface`, because the harness TUI and AUI vendor exactly that floor and link
//! it with `default-features = false`. Nothing in here needs tokio, axum or
//! cauldron's `env`, and nothing in here may acquire such a need: the two faces
//! build this file from `src/` with a manifest they write themselves.
//!
//! ## Why the envelope is not part of `wire`
//!
//! A shape frame and the envelope that carries it are different KINDS of thing.
//! A shape frame is a producer's state crossing the wire; the envelope is the
//! protocol a connection speaks regardless of which shapes exist. Keeping them
//! in separate modules means a change to one is not a merge in the other — and
//! the split survived into this tree for that reason rather than for tidiness.
//!
//! ## The typed refusal code
//!
//! Every refusal used to reach a consumer as `Display` prose and nothing else,
//! so a consumer that had to tell *unknown shape* from *dead fold* from *absurd
//! `at`* had to parse English. The chassis owns dispatch, therefore it owns the
//! envelope, therefore it owns the code: every refusal is typed at the single
//! point they all pass through. A producer's own error type is typed when it
//! implements [`Coded`], and carries [`codes::ERROR`] when it does not — the old
//! behaviour spelled out as a value instead of left as an absent field.
//!
//! ## What is frozen here
//!
//! All of it. Every serde tag, every variant name, every `type` string, every
//! code value, every `/health` key and their order. Two faces and a live daemon
//! parse these frames, and the conformance transcripts are byte comparisons.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::wire::ShapeWire;

// ─── 1. Who is answering ─────────────────────────────────────────────────────

/// **THE PRODUCER'S IDENTITY, SUPPLIED ONCE.**
///
/// The `welcome` frame and the `/health` body are both built from this struct
/// and from nothing else, so the handshake and the deployment probe cannot
/// disagree about what is running. They could, and did, while each read its own
/// constants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// The daemon name — `cauldron`, `threads`, … Also the stem of the consumer
    /// ledger's filename.
    pub daemon: String,
    /// The producer crate's version.
    pub version: String,
    /// Transport protocol version.
    pub proto: u16,
    /// Minted once per process: it changes exactly when the producer restarted,
    /// which is how a consumer tells a reconnect from a resume.
    pub boot_id: String,
}

// ─── 2. The closed code set ──────────────────────────────────────────────────

/// **THE CLOSED SET OF REFUSAL CODES.**
///
/// The resource half is the set `docs/chassis-resources.md` §2.5 drew, used
/// verbatim. Three codes exist beyond it because that set was drawn for the
/// RESOURCE door and says nothing about the RPC one: [`DEAD`] (a fold whose task
/// died — `shapes.md` 2.6 requires a refused link to be distinguishable and
/// terminal), [`UNAUTHORIZED`] and [`UNKNOWN_METHOD`].
///
/// The recurring judgment in here is worth stating once, because it is why the
/// set is as wide as it is: **two refusals share a code only when they share a
/// REMEDY.** A code a consumer can act on must never be indistinguishable from
/// one it cannot.
pub mod codes {
    /// This producer never knew the thing — an unknown shape, an unknown
    /// resource id. Do not retry.
    pub const UNKNOWN: &str = "unknown";
    /// The shape's fold task died. It cannot be subscribed to until the producer
    /// restarts: terminal, and meant to be drawn as terminal.
    pub const DEAD: &str = "dead";
    /// The request does not parse under the schema it claims — a missing param,
    /// an `at` that is not RFC3339, an id that fails the id scheme.
    pub const MALFORMED: &str = "malformed";
    /// A per-connection cap is reached. Back off; it is the consumer's own
    /// doing.
    ///
    /// **Also the answer when something producer-side was momentarily
    /// contended** and the remedy is still *call it again* — a
    /// `durable`/`barrier` checkpoint that could not take its lock inside the
    /// store's busy timeout. Same code because the code names the REMEDY and not
    /// the cause: a consumer that backs off and retries is correct in both
    /// cases, and a second code would be a second spelling of one instruction.
    pub const BUSY: &str = "busy";
    /// This connection has not authenticated.
    pub const UNAUTHORIZED: &str = "unauthorized";
    /// No handler answers this method name.
    pub const UNKNOWN_METHOD: &str = "unknown_method";
    /// **A KEYLESS SUBSCRIBE TO A SHAPE WITH NO INVENTORY TO GIVE.**
    ///
    /// Its own code and NOT [`MALFORMED`], because the two are different answers
    /// and this one is actionable: name a key. `malformed` says the request was
    /// not understood; this says it was understood and cannot be served as
    /// asked. Collapsing them makes a fixable refusal look like a bug in the
    /// caller.
    pub const KEY_REQUIRED: &str = "key_required";
    /// A Mutable resource id that WAS current and is not.
    pub const SUPERSEDED: &str = "superseded";
    /// The thing existed and was deleted.
    pub const GONE: &str = "gone";
    /// This consumer may not have it.
    pub const FORBIDDEN: &str = "forbidden";
    /// The bytes on disk do not match the declared digest. A producer-side
    /// defect; the consumer cannot repair it.
    pub const INTEGRITY: &str = "integrity";
    /// **THIS PRODUCER DOES NOT OFFER THIS DOOR AT ALL.**
    ///
    /// Its own code and NOT [`UNKNOWN`]: the two make different claims and
    /// licence different moves. `unknown` is about ONE id — the consumer's
    /// summary is stale or came from another producer, so try a different id.
    /// `unsupported` is about the DOOR — this producer serves no resources, or
    /// serves reads and not writes — so stop asking with any id. Nor is it
    /// [`MALFORMED`]: the request was well formed and understood, and the answer
    /// is a capability rather than a parse.
    ///
    /// The resource route exists on every chassis precisely so this answer is
    /// reachable. A bare 404 from an absent route cannot be told apart from a
    /// 404 for an unknown id, which would make a producer with no resources
    /// indistinguishable from one that has lost one.
    pub const UNSUPPORTED: &str = "unsupported";
    /// **A RANGE OUTSIDE THE RESOURCE.**
    ///
    /// Its own code and NOT [`MALFORMED`] — the `Range` header was syntactically
    /// fine and the request was understood; what is wrong is the consumer's
    /// belief about how long the resource is. And NOT [`SUPERSEDED`], even
    /// though the move is the same one (re-read the row): nothing was
    /// superseded and no pointer moved, so a code claiming one had would send
    /// the consumer hunting a generation change that never happened.
    pub const RANGE_UNSATISFIABLE: &str = "range_unsatisfiable";
    /// **AN UPLOAD WHOSE BYTES DO NOT HASH TO THE DIGEST IT WAS SENT UNDER.**
    ///
    /// Its own code and NOT [`INTEGRITY`], for the reason [`KEY_REQUIRED`] is
    /// not [`MALFORMED`]. `integrity` is the PRODUCER's stored bytes disagreeing
    /// with the producer's own declared digest — nothing the consumer can fix.
    /// This is the CONSUMER's body disagreeing with the name the consumer chose
    /// for it, and the repair is to re-send or to fix its hasher.
    pub const DIGEST_MISMATCH: &str = "digest_mismatch";
    /// **A BODY LARGER THAN THE DOOR WILL BUFFER**, carrying `limit`.
    ///
    /// `chassis-resources.md` §2.5 defined this for a whole-resource fetch of a
    /// chunked resource, and `chassis-writes.md` §2.3 withdrew it from the read
    /// side, where `Range` replaced it. It is still owed on the WRITE side: an
    /// upload is buffered whole so its digest can be checked before anything is
    /// stored, and an unbounded body is a memory fault rather than a large file.
    pub const TOO_LARGE: &str = "too_large";
    /// **THIS CONSUMER WAS BUILT AGAINST A SHAPE VERSION THIS PRODUCER DOES NOT
    /// SERVE** — rule 2.6, refused at link time, naming the version wanted and
    /// the versions served in its prose.
    ///
    /// **Its own code and NOT [`MALFORMED`]**, where every other *"cannot be
    /// served as written"* subscribe refusal lands. This is [`KEY_REQUIRED`]'s
    /// argument taken one step further: those refusals are about a REQUEST and
    /// their repair is a different request on the same build. This one is about
    /// the BUILD. No param fixes it, retrying is wrong at every interval, and
    /// rule 2.6 requires the consuming chassis to put the shape into its
    /// TERMINAL state on receipt — drawn the way a dead fold is drawn and never
    /// repainted as healthy. A mirror cannot decide to go terminal off a code
    /// that also means *"your `at` did not parse"*, so sharing `malformed` would
    /// have made the one refusal with a terminal consequence indistinguishable
    /// from the four with a retryable one by English alone.
    ///
    /// **And NOT [`SUPERSEDED`]**, which is taken and means a Mutable RESOURCE
    /// ID that was current and is not: a different subject with a different
    /// remedy (re-read the row, fetch the new id). A link served on an older
    /// contract is not refused at all — that is rule 2.6's middle state, and it
    /// rides the epoch's `superseded` on every export instead of arriving as a
    /// code.
    pub const VERSION_REFUSED: &str = "version_refused";
    /// **THE HONEST DEFAULT.** A producer error with no [`super::Coded`] impl.
    /// Exactly as informative as every refusal was before the codes existed, and
    /// written out rather than left absent so a consumer can tell *"this
    /// producer has not typed this path"* from *"this producer is old"*.
    pub const ERROR: &str = "error";
    /// **THE ACT IS ON THE SURFACE AND THE FACT OR METHOD THAT RESOLVES IT
    /// DOES NOT EXIST ON THIS PRODUCER YET.**
    ///
    /// Its own code and NOT [`UNSUPPORTED`], which says *this producer does
    /// not offer this door*; this says *the door is open, the act is real, and
    /// the thing behind it is not built* — the prose names the method path and
    /// what is missing. And NOT [`UNKNOWN_METHOD`], which would make a surface
    /// built ahead of its runtime indistinguishable from a typo: an agent
    /// reading `unknown_method` concludes the tool does not exist; reading this
    /// it concludes the tool exists and must not be retried until the daemon
    /// says otherwise. `docs/managed-subagents.md` §2's façade exposes every
    /// tool with its full schema and refuses the unbuilt ones with this, so
    /// the surface is whole and honest (rule 2.7: a refusal reaches the caller
    /// where it can see it, never as an absence).
    pub const NOT_AVAILABLE_YET: &str = "not_available_yet";
    /// **A PARAMETER THIS ACT DOES NOT TAKE. REFUSED, NEVER IGNORED.**
    ///
    /// `docs/managed-subagents.md` §1.4: *"A spawn that sends `budget_key` is
    /// refused with `unknown_param`. It is not ignored. Ignoring a parameter
    /// the caller meant is the silent-divergence class this codebase spent a
    /// stage deleting."* Its own code and NOT [`MALFORMED`]: the request
    /// parsed and every required thing is there; what is wrong is one name,
    /// and the prose says which so the remedy is to drop it.
    pub const UNKNOWN_PARAM: &str = "unknown_param";
    /// **THE SUBJECT A VERB NAMES IS NOT HERE NOW** — a thread id, an attached
    /// session's cookie — and the producer does not claim whether it ever was.
    ///
    /// Its own code and NOT [`UNKNOWN`], which says *this producer never knew
    /// the id* and whose remedy is *your id is stale, try a different one*. A
    /// detach whose cookie this producer released a moment ago (an explicit
    /// detach that lost the race to the pid reaper) is not "never known", and
    /// its remedy is the opposite one: there is nothing left to release, so
    /// the caller's session is over and it must not retry. And NOT [`GONE`],
    /// which claims the thing existed: a thread id nobody ever created is
    /// answered the same way and that claim would be false. And never
    /// [`BUSY`]: a caller told *"held, back off and retry"* about a thread that
    /// does not exist waits for a turn that is not running (DEPLOY-9i).
    /// Introduced at SH-A2c for `threads/{thread_id}/attach` and
    /// `threads/{thread_id}/detach` (DEPLOY-9k finding 2: a client branching on
    /// `code` saw `unknown`, with `not_found` only in the prose).
    pub const NOT_FOUND: &str = "not_found";

    /// **EVERY CODE ABOVE — and the set being CLOSED is a testable claim.**
    ///
    /// `the_code_set_is_closed` reads this module's own source text and asserts
    /// that every `pub const` declared above appears here. That test exists
    /// because the list had already fallen behind by one in substrate:
    /// [`KEY_REQUIRED`] was added to the module and not to `ALL`, so the single
    /// test that pinned "the set" was pinning a subset and reporting it as the
    /// set.
    pub const ALL: &[&str] = &[
        UNKNOWN,
        DEAD,
        MALFORMED,
        BUSY,
        UNAUTHORIZED,
        UNKNOWN_METHOD,
        KEY_REQUIRED,
        SUPERSEDED,
        GONE,
        FORBIDDEN,
        INTEGRITY,
        UNSUPPORTED,
        RANGE_UNSATISFIABLE,
        DIGEST_MISMATCH,
        TOO_LARGE,
        VERSION_REFUSED,
        ERROR,
        NOT_AVAILABLE_YET,
        UNKNOWN_PARAM,
        NOT_FOUND,
    ];
}

/// **A REFUSAL, TYPED.** One code out of [`codes`], plus the prose that used to
/// be the only thing on the wire.
///
/// Not a serde type: what reaches a consumer is [`Outcome::Error`], and this is
/// the in-process value that becomes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The untyped form — [`codes::ERROR`]. What a producer that has not
    /// implemented [`Coded`] gets.
    pub fn untyped(message: impl Into<String>) -> Self {
        Self::new(codes::ERROR, message)
    }

    pub fn malformed(message: impl Into<String>) -> Self {
        Self::new(codes::MALFORMED, message)
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// **WHAT A PRODUCER IMPLEMENTS TO GET ITS OWN ERRORS TYPED.**
///
/// The default is [`codes::ERROR`], so a producer that does nothing keeps the
/// behaviour it already had: nothing about typing the envelope is a silent
/// change of meaning for an existing producer.
pub trait Coded {
    fn code(&self) -> &'static str {
        codes::ERROR
    }
}

impl Coded for crate::wire::ShapeError {
    /// **THE TYPED FORM A SHAPE REFUSAL NEVER HAD ON THE WIRE.**
    ///
    /// Seven of the twelve variants answer [`codes::MALFORMED`] — the request as
    /// written cannot be served and the prose carries the remedy — which makes
    /// the exceptions the interesting part of this match:
    ///
    /// * `Unknown` / `Dead` are about the SHAPE, not the request.
    /// * `TooManyTimers` / `TooManyKeys` are [`codes::BUSY`]: well-formed
    ///   request, well producer, full per-connection capacity. The repair is to
    ///   release a slot rather than to write a different request, and a code of
    ///   their own would say nothing `busy` does not.
    /// * `KeyRequired` is [`codes::KEY_REQUIRED`]: understood, unserviceable as
    ///   asked, and repaired by naming a key.
    /// * `VersionRefused` is [`codes::VERSION_REFUSED`]: the only entry here
    ///   whose consequence on the consuming side is TERMINAL, which is precisely
    ///   why it may not share `malformed` with refusals a consumer fixes in a
    ///   param.
    fn code(&self) -> &'static str {
        use crate::wire::ShapeError as E;
        match self {
            E::Unknown(_) => codes::UNKNOWN,
            E::Dead { .. } => codes::DEAD,
            E::TooManyTimers { .. } | E::TooManyKeys { .. } => codes::BUSY,
            E::KeyRequired { .. } => codes::KEY_REQUIRED,
            E::VersionRefused { .. } => codes::VERSION_REFUSED,
            // Rule 2.8 REFUSES; it does not clamp — and the refusal is
            // `malformed` rather than `busy` (the producer is fine) or a code of
            // its own (the prose names the ceiling, which is the whole remedy).
            // `NoRetention` / `NoBeat` say the parameter selects nothing on this
            // shape; `TooFast` names the floor and says ask for MORE. Different
            // facts, one remedy class: send a different request.
            E::BadAt(_)
            | E::BadDetail(_)
            | E::AboveMax { .. }
            | E::NoRetention { .. }
            | E::NoBeat { .. }
            | E::TooFast { .. } => codes::MALFORMED,
        }
    }
}

impl From<crate::wire::ShapeError> for Refusal {
    /// The code is ADDITIVE: the `Display` prose is carried through unchanged,
    /// so nothing a consumer read before this existed has moved.
    fn from(e: crate::wire::ShapeError) -> Self {
        Refusal::new(e.code(), e.to_string())
    }
}

// ─── 3. The bearer: the grant, not the identity ──────────────────────────────

/// **A CONNECTION'S BEARER TOKEN, ISSUED BY THE HANDSHAKE.**
///
/// Operator ruling, 2026-09-26. The id names the thing; the bearer is the grant.
/// The rejected alternative is worth keeping on the record: deriving a
/// per-consumer resource id, so that a consumer never sees the general id and no
/// bearer is needed. It was rejected for **consistency across restarts** — a
/// consumer would have to persist its consumer key, and losing that key would
/// lose access to its own old data. A bearer has no such failure mode, because
/// it is re-issued on every connect and nothing persists it.
///
/// **Scope, set explicitly: cross-consumer access is NOT a concern.** These are
/// processes on one device. There is no ACL, no per-consumer authorization and
/// no policy hook, and any authenticated consumer being able to fetch any id it
/// learns is ACCEPTED rather than an oversight being designed around. The
/// bearer proves one thing: *you completed the handshake on this daemon.*
///
/// **It is redacted in both `Debug` and `Display`, deliberately.** A secret
/// escapes through a `tracing` field or a `{:?}` on some struct that happens to
/// contain it, and a `welcome` frame is exactly such a struct. Reaching the
/// token requires [`Bearer::expose`] — one word, greppable in review.
///
/// On the wire it is `#[serde(transparent)]`: a bare JSON string, never an
/// object. The `welcome` frame's `bearer` key is what the resource door's
/// `Authorization: Bearer <token>` header is filled from, and the conformance
/// transcripts compare those bytes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bearer(String);

impl Bearer {
    pub fn from_token(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The token itself. Named to be conspicuous at the call site.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Bearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Bearer(<redacted>)")
    }
}

impl std::fmt::Display for Bearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

// ─── 4. The request/response envelope ────────────────────────────────────────

/// The success / error / promise switch.
///
/// `code` appears only on the error arm and only after `error`, so a consumer
/// reading by key sees what it always saw plus one field. That additivity is the
/// whole reason the typed code could be introduced without a proto bump.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    Success {
        payload: Value,
    },
    Error {
        error: String,
        /// One of [`codes`]. **Never absent**: an untyped producer error carries
        /// [`codes::ERROR`] rather than omitting the key.
        code: String,
    },
    Promise {
        promise: String,
    },
}

impl Outcome {
    pub fn ok(payload: Value) -> Self {
        Outcome::Success { payload }
    }

    /// A typed refusal.
    pub fn refused(r: Refusal) -> Self {
        Outcome::Error {
            error: r.message,
            code: r.code.to_string(),
        }
    }

    /// The untyped form, for a producer error with no [`Coded`] impl.
    pub fn err(msg: impl Into<String>) -> Self {
        Outcome::refused(Refusal::untyped(msg))
    }

    /// The code, for a caller that must branch on it rather than on prose.
    pub fn code(&self) -> Option<&str> {
        match self {
            Outcome::Error { code, .. } => Some(code),
            _ => None,
        }
    }
}

/// Client → producer frames the CHASSIS owns.
///
/// Deliberately not an exhaustive `ClientMsg`. A producer keeps its own control
/// frames, and the chassis forwards any other `type` to the producer rather than
/// claiming to know the whole vocabulary. See [`ClientFrame`] and [`classify`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChassisMsg {
    /// Authenticate this connection.
    Auth { secret: String },
    /// An RPC request.
    Request {
        id: String,
        method: String,
        #[serde(default)]
        params: Value,
    },
}

/// What one client text frame turned out to be.
#[derive(Debug)]
pub enum ClientFrame {
    /// The chassis owns it.
    Chassis(ChassisMsg),
    /// The producer may own it. `ty` is the `type` tag as it arrived.
    Producer { ty: String, raw: Value },
    /// Nobody owns it, and this is the sentence the consumer gets.
    Invalid(String),
}

/// Sort one client text frame into a [`ClientFrame`].
///
/// **The `type` tag is read before the body is bound to a type.** That is the
/// point of this function, and the cost of not having it is a measured one: a
/// single `serde` enum over every frame kind produced
/// ``unknown variant `foo`, expected one of `auth`, `request`, `subscribe`,
/// `unsubscribe` `` — a sentence only something that knew every producer's frame
/// vocabulary could write. The chassis must be able to reject a typo in a
/// producer's frame without knowing that producer's frames.
pub fn classify(text: &str) -> ClientFrame {
    let raw: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return ClientFrame::Invalid(format!("invalid frame: {e}")),
    };
    // Owned up front: the tag outlives the borrow so `raw` can be moved into the
    // producer arm below.
    let Some(tag) = raw.get("type").and_then(Value::as_str).map(str::to_owned) else {
        return ClientFrame::Invalid(
            "invalid frame: missing field `type` (a string naming the frame kind)".into(),
        );
    };
    if tag == "auth" || tag == "request" {
        return match serde_json::from_value::<ChassisMsg>(raw) {
            Ok(m) => ClientFrame::Chassis(m),
            Err(e) => ClientFrame::Invalid(format!("invalid frame: {e}")),
        };
    }
    ClientFrame::Producer { ty: tag, raw }
}

/// Producer → client frames the CHASSIS owns.
///
/// A producer's own frames are forwarded as already-serialised text (the door's
/// `ExtraFrame`) and never pass through this enum. That is what makes
/// byte-identity of a legacy wire a CONSTRUCTION rather than a discipline: there
/// is no chassis type a producer frame is re-encoded through, so there is no
/// re-encode that could reorder a key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChassisServerMsg {
    /// The first frame on a fresh connection.
    Welcome {
        proto: u16,
        daemon: String,
        daemon_version: String,
        boot_id: String,
        auth_required: bool,
        /// **THE BEARER, WHEN THE SOCKET HAS NOTHING TO PROVE.** Present iff
        /// `auth_required` is false — such a connection completed its handshake
        /// by arriving. When a secret IS configured this key is absent and the
        /// bearer rides the `auth_ack` instead: issuing it here would hand the
        /// grant to a connection that has not authenticated, which is the one
        /// thing the secret exists to prevent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bearer: Option<Bearer>,
    },
    /// Answers an `auth` frame.
    AuthAck {
        ok: bool,
        /// Present iff `ok`. The handshake is what gates issuing it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bearer: Option<Bearer>,
    },
    /// Answers a `request.id`.
    Response { correlate: String, outcome: Outcome },
    /// An export, or one derived event from a fold.
    Shape(ShapeWire),
}

// ─── 5. /health ──────────────────────────────────────────────────────────────

/// **`/health`'S PAYLOAD — A DEPLOYMENT DEPENDENCY, NOT A DESIGN CHOICE.**
///
/// `core/tools/promote` hardcodes the `/health` path on the daemon's port and
/// reads this body for `--self-check` and `promote status`. The path and these
/// six keys are load-bearing for the absorb-and-verify cycle of a LIVE daemon:
/// breaking either does not fail a test, it destroys the ability to confirm what
/// is running. Adding a key is additive; moving the path or renaming a key is a
/// deployment break and gets a refusal in review, not a version bump.
///
/// **The fields are declared in the order `serde_json`'s map emits them** —
/// alphabetical, because the body this replaced was hand-built with `json!` over
/// a `BTreeMap`. That ordering is what makes the two byte-identical rather than
/// merely equivalent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthWire {
    pub boot_id: String,
    pub daemon: String,
    /// A CAPABILITY ADVERTISEMENT, read by the supervisor's escalation ladder.
    /// Its ABSENCE is the informative case: an older image does not emit it, and
    /// the supervisor must then not claim it asked such a daemon to shut down
    /// cleanly. Chassis-supplied and always `true` — a producer served by this
    /// chassis is served with graceful shutdown.
    pub graceful_shutdown: bool,
    pub proto: u16,
    pub status: String,
    pub version: String,
    /// **PRODUCER EXTRAS.** Flattened in at the top level, after the fixed keys,
    /// and they may never shadow one — [`HealthWire::build`] refuses.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extras: BTreeMap<String, Value>,
}

/// The keys no producer extra may take.
pub const HEALTH_FIXED_KEYS: &[&str] = &[
    "boot_id",
    "daemon",
    "graceful_shutdown",
    "proto",
    "status",
    "version",
];

impl HealthWire {
    /// Build the payload from an [`Identity`] plus a producer's extras.
    ///
    /// # Panics
    ///
    /// When an extra shadows one of [`HEALTH_FIXED_KEYS`]. A panic and not a
    /// silent drop: a producer that believes it is reporting `version` and is
    /// not is exactly the confidently-wrong state this struct exists to
    /// prevent, and the only moment to say so is the one where a human is
    /// looking.
    pub fn build(id: &Identity, extras: BTreeMap<String, Value>) -> Self {
        if let Some(k) = extras
            .keys()
            .find(|k| HEALTH_FIXED_KEYS.contains(&k.as_str()))
        {
            panic!(
                "producer health extra `{k}` shadows a fixed /health key; the six \
                 fixed keys are a deployment dependency and cannot be overridden"
            );
        }
        Self {
            boot_id: id.boot_id.clone(),
            daemon: id.daemon.clone(),
            graceful_shutdown: true,
            proto: id.proto,
            status: "ok".to_string(),
            version: id.version.clone(),
            extras,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::ShapeError as E;

    fn identity() -> Identity {
        Identity {
            daemon: "threads".into(),
            version: "0.1.0".into(),
            proto: 1,
            boot_id: "194b96ba".into(),
        }
    }

    // ── /health ─────────────────────────────────────────────────────────────

    /// **THE DEPLOYMENT CONTRACT, PINNED AS BYTES.** Not "the keys are present":
    /// the exact serialisation, because `promote` reads this body and a
    /// reordered or renamed key is a deployment break no other test can see.
    #[test]
    fn health_serialises_to_the_exact_bytes_promote_reads() {
        let body = serde_json::to_string(&HealthWire::build(&identity(), BTreeMap::new())).unwrap();
        assert_eq!(
            body,
            r#"{"boot_id":"194b96ba","daemon":"threads","graceful_shutdown":true,"proto":1,"status":"ok","version":"0.1.0"}"#
        );
    }

    /// The fixed key set is the thing `promote` depends on. Named once here so
    /// that removing one has to be deliberate.
    #[test]
    fn the_six_fixed_health_keys_are_exactly_these() {
        assert_eq!(
            HEALTH_FIXED_KEYS,
            &[
                "boot_id",
                "daemon",
                "graceful_shutdown",
                "proto",
                "status",
                "version"
            ]
        );
    }

    /// The accepting half of the shadow rule: a non-colliding extra is served,
    /// and it lands AFTER the fixed keys rather than interleaved among them.
    #[test]
    fn a_producer_extra_is_appended_after_the_fixed_keys() {
        let mut extras = BTreeMap::new();
        extras.insert("pool".to_string(), Value::from(4));
        let body = serde_json::to_string(&HealthWire::build(&identity(), extras)).unwrap();
        assert!(
            body.ends_with(r#""version":"0.1.0","pool":4}"#),
            "an extra is appended, never interleaved: {body}"
        );
    }

    /// The refusing half.
    #[test]
    #[should_panic(expected = "shadows a fixed /health key")]
    fn a_producer_extra_may_not_shadow_a_fixed_key() {
        let mut extras = BTreeMap::new();
        extras.insert("version".to_string(), Value::from("lies"));
        HealthWire::build(&identity(), extras);
    }

    // ── The typed code ──────────────────────────────────────────────────────

    /// Every variant of a shape refusal, and the code it reaches a consumer as.
    /// The whole enum, so an implementation that answered one code for
    /// everything fails here rather than passing on the subset that happens to
    /// share `malformed`.
    #[test]
    fn every_shape_error_carries_its_own_code() {
        let cases: Vec<(E, &str)> = vec![
            (E::Unknown("nope".into()), codes::UNKNOWN),
            (
                E::Dead {
                    shape: "accounts".into(),
                    reason: "panicked".into(),
                },
                codes::DEAD,
            ),
            (E::BadAt("not rfc3339".into()), codes::MALFORMED),
            (E::BadDetail("keys".into()), codes::MALFORMED),
            (
                E::TooManyTimers {
                    shape: "accounts".into(),
                    max: 4,
                },
                codes::BUSY,
            ),
            (
                E::TooManyKeys {
                    shape: "accounts".into(),
                    max: 8,
                },
                codes::BUSY,
            ),
            (
                E::KeyRequired {
                    shape: "accounts".into(),
                    max_rows: 500,
                },
                codes::KEY_REQUIRED,
            ),
            (
                E::AboveMax {
                    shape: "accounts".into(),
                    param: "retain".into(),
                    asked: "900".into(),
                    max: "100".into(),
                },
                codes::MALFORMED,
            ),
            (
                E::NoRetention {
                    shape: "accounts".into(),
                },
                codes::MALFORMED,
            ),
            (
                E::NoBeat {
                    shape: "accounts".into(),
                },
                codes::MALFORMED,
            ),
            (
                E::TooFast {
                    shape: "accounts".into(),
                    asked_ms: 100,
                    min_ms: 1000,
                },
                codes::MALFORMED,
            ),
            (version_refused(), codes::VERSION_REFUSED),
        ];
        for (e, want) in cases {
            let r: Refusal = e.clone().into();
            assert_eq!(r.code, want, "{e:?} must be distinguishable without prose");
            // The prose survives the move: the code is additive, never a swap.
            assert_eq!(r.message, e.to_string());
        }
    }

    fn version_refused() -> E {
        E::VersionRefused {
            shape: "accounts".into(),
            wanted: 2,
            serves: vec![4, 3],
        }
    }

    /// **RULE 2.6's REFUSAL, AS A NON-EQUALITY.** The defect worth catching is
    /// not a missing code — it is this one silently collapsing into `malformed`
    /// with the fixable-request refusals, which would leave the only refusal
    /// with a TERMINAL consequence on the consuming side indistinguishable from
    /// them. A mirror goes terminal off this code, and it cannot do that off one
    /// that also means *"your `at` did not parse"*.
    #[test]
    fn a_wrong_build_is_not_spelled_like_a_wrong_request() {
        assert_ne!(
            version_refused().code(),
            E::BadAt("not rfc3339".into()).code(),
            "a version refusal must not share `malformed` with the refusals a \
             consumer can fix in a param"
        );
        // And rule 2.6's sentence is what reaches the wire, both halves of it.
        let text = version_refused().to_string();
        assert!(text.contains("built against v2"), "{text}");
        assert!(
            text.contains("[4, 3]"),
            "the refusal names what the producer SERVES, which is the actionable \
             half: {text}"
        );
    }

    #[test]
    fn an_untyped_producer_error_is_spelled_error_and_not_left_absent() {
        let o = Outcome::err("unknown method: threads.nope");
        assert_eq!(o.code(), Some(codes::ERROR));
        assert_eq!(
            serde_json::to_string(&o).unwrap(),
            r#"{"type":"error","error":"unknown method: threads.nope","code":"error"}"#
        );
        // The other two arms carry no code at all, which is what makes `code`
        // additive rather than a new required field on the envelope.
        assert_eq!(Outcome::ok(Value::Null).code(), None);
        assert_eq!(
            serde_json::to_string(&Outcome::ok(Value::from(1))).unwrap(),
            r#"{"type":"success","payload":1}"#
        );
        assert_eq!(
            serde_json::to_string(&Outcome::Promise {
                promise: "7".into()
            })
            .unwrap(),
            r#"{"type":"promise","promise":"7"}"#
        );
    }

    /// **THE CODE SET IS CLOSED, AND THIS IS WHAT MAKES THAT A FACT.**
    ///
    /// A hand-maintained `ALL` is a list that falls behind, and in substrate it
    /// had: the `KEY_REQUIRED` change added a code to the module and not to
    /// `ALL`, so the test that read "the whole set" read eleven of twelve and
    /// passed. Rust cannot enumerate a module's items, so the SOURCE is the
    /// enumeration.
    ///
    /// The two things compared are genuinely different: the declaration text in
    /// this file, scanned at run time, against the compiled `codes::ALL` array.
    /// Neither is derived from the other.
    #[test]
    fn the_code_set_is_closed() {
        let src = include_str!("envelope.rs");
        // Bounded to the codes module's body, so the scan cannot drift onto a
        // `pub const` declared elsewhere in the file.
        let body = src
            .split_once("pub mod codes {")
            .expect("the codes module")
            .1
            .split_once("pub const ALL:")
            .expect("`ALL` closes the module")
            .0;

        let declared: Vec<&str> = body
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub const "))
            .filter_map(|rest| rest.split(':').next())
            .map(str::trim)
            .collect();

        // The positive sentinel: a scan that finds nothing must be shown capable
        // of finding something.
        assert!(
            declared.len() >= 16,
            "the scan found only {} codes, so it has stopped seeing the module: {declared:?}",
            declared.len()
        );

        // Every declared code's VALUE is in `ALL`. The value is what is on the
        // wire, so values are what the claim is about.
        for name in &declared {
            let value = body
                .split_once(&format!("pub const {name}:"))
                .unwrap()
                .1
                .split_once('"')
                .unwrap()
                .1
                .split_once('"')
                .unwrap()
                .0;
            assert!(
                codes::ALL.contains(&value),
                "code `{value}` is declared in `mod codes` and missing from `ALL`; \
                 the set is only closed if the list is complete"
            );
        }
        assert_eq!(
            codes::ALL.len(),
            declared.len(),
            "`ALL` carries {} codes and the module declares {}",
            codes::ALL.len(),
            declared.len()
        );

        // No duplicates, or "closed" would be hiding a collision.
        let mut sorted: Vec<&str> = codes::ALL.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "two codes share a value");
    }

    // ── classify ────────────────────────────────────────────────────────────

    /// The chassis claims `auth` and `request` and nothing else; every other
    /// `type` is the producer's to own or to disown. Four arms, so an
    /// implementation that answered one variant for everything fails.
    #[test]
    fn classify_routes_an_unknown_type_to_the_producer_rather_than_refusing_it() {
        match classify(r#"{"type":"subscribe","threads":null}"#) {
            ClientFrame::Producer { ty, .. } => assert_eq!(ty, "subscribe"),
            other => panic!("a producer frame must reach the producer: {other:?}"),
        }
        match classify(r#"{"type":"auth","secret":"s"}"#) {
            ClientFrame::Chassis(ChassisMsg::Auth { secret }) => assert_eq!(secret, "s"),
            other => panic!("auth is the chassis's: {other:?}"),
        }
        match classify(r#"{"type":"request","id":"1","method":"shapes.list"}"#) {
            // `params` defaults, so a request that omits it is not malformed.
            ClientFrame::Chassis(ChassisMsg::Request { id, method, params }) => {
                assert_eq!((id.as_str(), method.as_str()), ("1", "shapes.list"));
                assert_eq!(params, Value::Null);
            }
            other => panic!("request is the chassis's: {other:?}"),
        }
        match classify("not json") {
            ClientFrame::Invalid(m) => assert!(m.starts_with("invalid frame: "), "{m}"),
            other => panic!("garbage is invalid: {other:?}"),
        }
        match classify(r#"{"no":"type"}"#) {
            ClientFrame::Invalid(m) => assert!(m.contains("missing field `type`"), "{m}"),
            other => panic!("a frame with no type is invalid: {other:?}"),
        }
    }

    // ── The bearer ──────────────────────────────────────────────────────────

    /// **THE REDACTION, AND THE ONE WAY PAST IT.** Both halves, because either
    /// alone is passed by a constant: a `Debug` that printed the token fails the
    /// first pair, and an `expose` that returned `""` fails the second.
    #[test]
    fn a_bearer_is_redacted_everywhere_except_expose_and_the_wire() {
        let b = Bearer::from_token("5f1c0f7a8e1d4a6cb0d2e3f4a5b6c7d8");
        assert_eq!(format!("{b:?}"), "Bearer(<redacted>)");
        assert_eq!(format!("{b}"), "<redacted>");
        assert!(!format!("{b:?} {b}").contains("5f1c0f7a"));

        assert_eq!(b.expose(), "5f1c0f7a8e1d4a6cb0d2e3f4a5b6c7d8");
        // `transparent`: a bare JSON string on the wire, never an object, and it
        // round-trips — the resource door's `Authorization` header is filled
        // from exactly these bytes.
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(json, r#""5f1c0f7a8e1d4a6cb0d2e3f4a5b6c7d8""#);
        assert_eq!(serde_json::from_str::<Bearer>(&json).unwrap(), b);
    }

    /// **THE `welcome` FRAME, BOTH WAYS, AS BYTES.** The frame that issues the
    /// grant is the frame the transcripts compare, and the absent-vs-present
    /// bearer is the whole of the handshake's gating: `skip_serializing_if`
    /// means a connection with a secret to satisfy gets no `bearer` key at all,
    /// not a `null` one.
    #[test]
    fn welcome_carries_the_bearer_only_when_nothing_is_left_to_prove() {
        let open = ChassisServerMsg::Welcome {
            proto: 1,
            daemon: "threads".into(),
            daemon_version: "0.1.0".into(),
            boot_id: "194b96ba".into(),
            auth_required: false,
            bearer: Some(Bearer::from_token("deadbeef")),
        };
        assert_eq!(
            serde_json::to_string(&open).unwrap(),
            r#"{"type":"welcome","proto":1,"daemon":"threads","daemon_version":"0.1.0","boot_id":"194b96ba","auth_required":false,"bearer":"deadbeef"}"#
        );
        // And a redacting `Debug` on the frame, which is where a bearer would
        // otherwise reach a log line.
        assert!(!format!("{open:?}").contains("deadbeef"), "{open:?}");

        let gated = ChassisServerMsg::Welcome {
            proto: 1,
            daemon: "threads".into(),
            daemon_version: "0.1.0".into(),
            boot_id: "194b96ba".into(),
            auth_required: true,
            bearer: None,
        };
        assert_eq!(
            serde_json::to_string(&gated).unwrap(),
            r#"{"type":"welcome","proto":1,"daemon":"threads","daemon_version":"0.1.0","boot_id":"194b96ba","auth_required":true}"#
        );
    }

    /// `auth_ack`, on the same terms: present iff `ok`, and absent rather than
    /// `null` when not.
    #[test]
    fn auth_ack_carries_the_bearer_only_when_the_secret_was_right() {
        assert_eq!(
            serde_json::to_string(&ChassisServerMsg::AuthAck {
                ok: true,
                bearer: Some(Bearer::from_token("deadbeef")),
            })
            .unwrap(),
            r#"{"type":"auth_ack","ok":true,"bearer":"deadbeef"}"#
        );
        assert_eq!(
            serde_json::to_string(&ChassisServerMsg::AuthAck {
                ok: false,
                bearer: None,
            })
            .unwrap(),
            r#"{"type":"auth_ack","ok":false}"#
        );
    }

    /// A `response` frame's envelope, so the `correlate`/`outcome` keys and the
    /// nesting of the outcome's own tag are pinned where a face can see them.
    #[test]
    fn a_response_nests_the_outcome_under_its_own_type_tag() {
        let r = ChassisServerMsg::Response {
            correlate: "17".into(),
            outcome: Outcome::refused(Refusal::new(codes::UNKNOWN_METHOD, "no such method")),
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"type":"response","correlate":"17","outcome":{"type":"error","error":"no such method","code":"unknown_method"}}"#
        );
    }
}
