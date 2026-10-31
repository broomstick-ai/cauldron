//! # The subscriber ledger — rule 2.1: who is subscribed to what, and how they got there
//!
//! The rule numbers in this module are `docs/shapes.md`'s, and that document
//! lives in **substrate-interface-layer**, not in this tree; see the crate root
//! for the same pointer. Where a sentence below cites a substrate file, a
//! substrate producer or a substrate incident it says so, because none of them
//! has been migrated here yet and a bare path would read as a path in cauldron.
//!
//! ## The question nothing could answer
//!
//! Substrate's own record of why this module exists, carried here as HISTORY:
//! on 2026-09-22 two board processes were folding one producer's shape at once.
//! One was three days old and spinning. `shapes.list` reported `subscribers: 2`
//! and could not say who either was; the daemon log recorded frames, which have
//! no identities on them; `ps` showed two identical command lines and could not
//! say which one the daemon was still feeding. The operator's ask, in his own
//! words, was in two halves — *"at subscribe time we have the subscriber input
//! their identity and we log the subscribers to each shape"*, and *"a ledger of
//! all connects and disconnects… this should land in `~/.cauldron/`
//! somewhere"*.
//!
//! ## Two halves, because there are two questions
//!
//! | half | the question it answers | where it lives | can it rot? |
//! |---|---|---|---|
//! | the **REGISTRY** | *who is subscribed right now* | memory, this process | no, by construction |
//! | the **JOURNAL** | *who has ever been, and when* | an append-only JSONL file | it is history; staleness is not a category for it |
//!
//! **[`ConsumerLedger::consumers`] — which is what `shapes.list` answers from —
//! reads the REGISTRY and never replays the file.** That is the whole reason the
//! two halves are separate types rather than one. A `disconnect` line that never
//! got written cannot make the live answer wrong, because the live answer was
//! never computed from lines; and a file that cannot be opened at all costs the
//! producer nothing but its history.
//!
//! ## An entry closes from the TRANSPORT. There is no reaper.
//!
//! Operator ruling, on what happens when a consumer vanishes without
//! unsubscribing: *"the chassis should handle this. It's built on a websocket
//! daemon so we can detect disconnects & reconnects."*
//!
//! So *the connection ended* and *the entry closed* are ONE event rather than
//! two facts that can disagree: the producer's connection task holds a guard on
//! its own stack whose `Drop` calls [`ConsumerLedger::dropped`]. There is
//! deliberately **no TTL on an entry, no heartbeat vote and no liveness probe**
//! — each of those decides on evidence weaker than the socket's own state,
//! which is how a bookkeeping layer starts being confidently wrong about the
//! one thing it exists to report.
//!
//! Two exits skip ordinary control flow and are why the call belongs in a
//! `Drop` rather than at the end of the task: a connection task that PANICS
//! unwinds straight past its last statement, and one that is ABORTED never
//! reaches it. A `Drop` runs under both — this crate unwinds, it does not
//! `panic = "abort"`, and an aborted task drops its locals at the suspension
//! point.
//!
//! **A ZOMBIE consumer is not closed, and must not be.** A process still
//! holding its socket, still answering, still folding superseded code is
//! genuinely subscribed. It is precisely what the operator could not see, and
//! the registry naming it — by name, claimed contract, build and pid — is the
//! point of the module.
//!
//! Entries left in the FILE by a generation that died running no code at all
//! are closed by the generation sweep, once, at the first write of the next
//! generation: [`Journal::close_orphans`].
//!
//! ## Compression is load-bearing, not tidiness
//!
//! > *"note that this ledger can utilize some compression mechanisms, like
//! > count of the same events, so we don't turn a re-subscribe fit into 10,000
//! > lines in a log."*
//!
//! The chassis contract has **no re-emit verb and no replay**: a consumer that
//! doubts its state recovers by subscribing again, and that is the only
//! recovery path in the design. So a consumer stuck in a bad state re-subscribes
//! in a LOOP by construction — it is the expected failure, not an exotic one —
//! and a naive append buries every other consumer's history under it.
//!
//! Three mechanisms, in the order they apply, each named here so a reader can
//! tell which one collapsed a line they are looking at:
//!
//! 1. **A re-subscribe is ONE event.** Rule 2.5 is *"subscribing twice on one
//!    connection yields a fresh export and nothing else"*, so the subscription
//!    never ended and the journal says [`EVENT_RESUBSCRIBE`] rather than
//!    manufacturing a `disconnect`/`connect` pair — which would ALTERNATE and
//!    therefore never compress under mechanism 3.
//! 2. **A close is deferred by exactly one event.** An `unsubscribe` followed
//!    immediately by a `subscribe` on the same connection and shape is one
//!    recovery cycle wearing two verbs, so it collapses into the same single
//!    `resubscribe`. One event of lookahead ([`ConsumerLedger::deferred`]), no
//!    timers.
//! 3. **A run of identical records becomes a count.** Consecutive records that
//!    are equal apart from their timestamps are written once, and the
//!    repetitions behind them become one [`EVENT_REPEAT`] record carrying
//!    `repeats`, `first_repeat_at` and `last_repeat_at`. A 10,000-cycle fit is
//!    one `resubscribe` line and about ten `repeat` lines.
//!
//! **What deliberately does NOT collapse.** A consumer that drops its SOCKET
//! and reconnects gets a fresh connection id, so each of its cycles costs two
//! lines. Collapsing across sockets was rejected: two subscriptions on two
//! connections are not knowably the same consumer, and merging them would make
//! the journal assert something it does not know — the exact failure class the
//! module exists to prevent. [`LEDGER_MAX_BYTES`] is the backstop for that case.
//!
//! ## It records a CLAIM; it never negotiates on one
//!
//! [`ConsumerIdentity::shape_version`] is a string the consumer says about
//! itself — *"say what you were built against, even if it is a date"*. This
//! module writes it down, shows it to a human, and **never compares it, never
//! branches on it, and never refuses a subscribe because of it.** It is
//! evidence, not a handshake: it is free text, two builds three days apart did
//! declare the same value, and [`ConsumerIdentity::build`] exists precisely
//! because a declared contract string cannot tell them apart.
//!
//! That ruling is about THAT FIELD, and rule 2.6's version is a different thing
//! one rung away. The distinction, because conflating them is how a
//! no-negotiation rule turns into a negotiation:
//!
//! * **No version on a FRAME.** Rule 2.6's *cheapening property*: additive
//!   changes need no bump, so the ordering rule stays one line and a mirror
//!   ignores what it does not know.
//! * **A version on a LINK.** Rule 2.6's `VERSION`/`SERVES`, an INTEGER, stated
//!   once when a subscription is established and compared exactly once, by
//!   [`crate::wire::ShapeContract::link`]. A mismatch is refused there, loudly,
//!   naming what the producer serves.
//!
//! So [`ConsumerLedger::connected_link`] records a LINK STATE as well as a
//! claim, and it still compares nothing: it is handed a
//! [`crate::wire::Superseded`] somebody else decided and writes it down.
//!
//! ## Nothing here knows what a shape IS
//!
//! This module takes a shape NAME, a connection id and a claimed identity, and
//! would behave identically for a shape nobody has written yet. It is chassis
//! bookkeeping, and the one invariant it owes the rest of the crate is that a
//! bookkeeping failure never fails a subscribe — see [`Journal::write_record`].
//!
//! ## Where the file goes, and the one place this tree departs from substrate
//!
//! Substrate's `ledger.rs` resolved its own path: it read `$CAULDRON_HOME`,
//! `$HOME`, `$CAULDRON_ENV` and `$ENV`, re-implemented cauldron's environment
//! precedence and its 1–32-character name rule, and carried a comment saying it
//! must not drift from `libs/env/src/paths.rs`.
//!
//! **In this tree it resolves nothing.** The crate root's own contract is that
//! *"the data root, the environment name … come from cauldron's `env` crate,
//! and nothing in here reads `$HOME` or `$CAULDRON_ENV` itself"*, and
//! `Cargo.toml` declares `ledger = []` — this module has no dependency it could
//! read them WITH, because `env` is reachable only behind `seam`. A mirrored
//! copy of a precedence rule is a thing that drifts; the repair is to not have a
//! second copy. So [`ledger_path`] is pure arithmetic over a directory the
//! caller already resolved — `Environment::service_data(chassis)` in cauldron's
//! `libs/env` — and the only layout fact this module still owns is the one it is
//! the owner of: the `links/` segment and the file's name.

use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::wire::ConnId;

// ─────────────────────────────────────────────────────────────────────────────
// 1. The on-disk contract: the directory, the file name, the two bounds
// ─────────────────────────────────────────────────────────────────────────────

/// The `kind` segment a chassis keeps its journal under, inside its own service
/// directory: `data/<env>/<chassis>/links/`.
///
/// It is `links` and not `chassis`. The operator's ruling is *"chassis is a
/// concept not an application"* — a directory in his data root is owned by a
/// SERVICE, something that can be `du`'d, deleted without a glob and handed to
/// a process as a path it owns — so the segment names the THING IN IT, which is
/// the set of live links between producers and consumers, and it sits inside
/// the service that writes it.
///
/// A directory rather than a bare file because *"by default all chassis should
/// do this"*: one folder per producer, one file in it, so two producers never
/// interleave appends and no cross-process lock is needed anywhere in this
/// module.
pub const LEDGER_DIR: &str = "links";

/// The producer half's file name, given the chassis name:
/// `<chassis>.producer.jsonl`.
///
/// Two decisions sit in that string.
///
/// * **`<chassis>` is repeated, having already named the directory.** One file
///   per producer is what makes appends safe with no `flock` and no
///   half-written line from a second writer; the redundancy is kept so a file
///   copied or quoted out of context still says who wrote it.
/// * **`.producer.` is the half the operator called *"both ends"*.** The
///   consumer half — a subscriber recording who IT is subscribed to — is not
///   built here, and reserving `<consumer>.consumer.jsonl` now is what stops it
///   being retrofitted into an ambiguous name later.
pub fn ledger_file_name(chassis: &str) -> String {
    format!("{chassis}.producer.jsonl")
}

/// **THE JOURNAL'S PATH, from a directory the caller already resolved** —
/// `<service_data>/links/<chassis>.producer.jsonl`.
///
/// `service_data` is `data/<env>/<chassis>/` and this module does not find it.
/// In cauldron it comes from `libs/env`'s `Environment::service_data`, which is
/// the one owner of the root knob, the environment knob, the reserved-name
/// refusals and the "guarded inside the data root" check. See the module doc
/// for why this is pure arithmetic here and was environment resolution in
/// substrate.
///
/// It creates nothing and touches no filesystem. Directory creation is the
/// journal's, lazily, at its first record — see [`Journal`].
pub fn ledger_path(service_data: &Path, chassis: &str) -> PathBuf {
    service_data.join(LEDGER_DIR).join(ledger_file_name(chassis))
}

/// **THE SIZE BACKSTOP — not the primary bound.**
///
/// Compression is the primary bound: the events that can actually run away
/// collapse into counts, so the file grows with DISTINCT events rather than
/// total ones. This is what catches the growth anyway — the reconnect-per-socket
/// cycle, which deliberately does not collapse (see the module doc), or simply
/// years of ordinary use.
///
/// 4 MiB of JSONL is on the order of a hundred thousand records. Rotation at
/// that point is unconditional and needs nobody's attention.
pub const LEDGER_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// **ONE PREVIOUS GENERATION IS KEPT, AND NO MORE.**
///
/// The journal's job is *who is connected now, and who was connected recently
/// enough to explain what I am looking at*. Deep history belongs to an event
/// store. One `.1` bounds the whole thing at `2 × LEDGER_MAX_BYTES` — 8 MiB,
/// forever, with no reaper, no cron and no knob to get wrong.
pub const LEDGER_GENERATIONS: usize = 1;

/// **HOW MANY REPETITIONS ACCUMULATE BEFORE A `repeat` RECORD IS FLUSHED.**
///
/// A run is held in memory until it ends, so the count of a run in progress is
/// not durable: a `SIGKILL` mid-run loses it. Flushing every 1,024 bounds that
/// loss and gives a long fit periodic evidence on disk rather than one line at
/// the very end.
///
/// What is NOT at risk: the FIRST occurrence of any record is written
/// immediately, so a lost run loses its count and never the fact that the event
/// happened.
pub const LEDGER_RUN_MAX: u64 = 1024;

// ─────────────────────────────────────────────────────────────────────────────
// 2. The event vocabulary — these strings are the wire
// ─────────────────────────────────────────────────────────────────────────────

/// A subscription opened. `consumer` is `null` for an unidentified one.
pub const EVENT_CONNECT: &str = "connect";

/// A subscription closed, carrying a [`CloseReason`].
pub const EVENT_DISCONNECT: &str = "disconnect";

/// **THE RECOVERY CYCLE, AS ONE EVENT.** The subscription never ended; see the
/// module doc's mechanism 1 for why this is not a `disconnect` and a `connect`.
///
/// It does **not** cover a subscription taking an interest in a SECOND key —
/// that is [`EVENT_WIDEN`]. Two acts wearing one word is the disease this module
/// was written against, and this was an instance of it.
pub const EVENT_RESUBSCRIBE: &str = "resubscribe";

/// **A SUBSCRIPTION TOOK AN INTEREST IN ANOTHER KEY.** Carries `key` (the new
/// one) and `keys` (all of them, after).
///
/// Why it is not [`EVENT_RESUBSCRIBE`], which is what substrate first recorded
/// it as: `resubscribe` is rule 2.5's RECOVERY act — *this consumer doubted its
/// state and repaired it* — and it is the symptom an operator reads the column
/// to find. *This consumer now also wants key `b`* is a consumer doing its job.
/// Recording the second as the first puts a healthy consumer in the same column
/// as a looping one, and the column is read precisely to find the looping one.
///
/// It was also what made the registry lie: the `resubscribe` arm OVERWROTE the
/// entry's key with the new one, so a consumer holding `a` and `b` was reported
/// as holding `b` alone.
pub const EVENT_WIDEN: &str = "widen";

/// **A SUBSCRIPTION DROPPED ONE KEY AND IS STILL LIVE.** Carries `key` (the one
/// dropped) and `keys` (all of them, after). The inverse of [`EVENT_WIDEN`], and
/// not an [`EVENT_DISCONNECT`] for the same reason: the subscription did not end.
/// Dropping the LAST key does end it, and that writes a `disconnect` instead —
/// see [`ConsumerLedger::narrowed`].
pub const EVENT_NARROW: &str = "narrow";

/// **A RUN OF IDENTICAL RECORDS, COLLAPSED.** Carries `repeats`,
/// `first_repeat_at`, `last_repeat_at` and `of` (the record being repeated).
///
/// **It deliberately carries no top-level `conn` and no top-level `shape`.** The
/// generation sweep pairs connects with disconnects by `(boot, conn, shape)`,
/// and a `repeat` that exposed those three at the top level would be read as
/// whichever of the two it was summarising. Keeping them nested inside `of` is
/// what makes a `repeat` invisible to the pairing by construction rather than by
/// a rule the sweep has to remember.
pub const EVENT_REPEAT: &str = "repeat";

/// This chassis generation started. Carries `pid` and `version`, plus the three
/// fields every record carries.
pub const EVENT_CHASSIS_START: &str = "chassis_start";

// ─────────────────────────────────────────────────────────────────────────────
// 3. The identity a consumer states, and the one sentence that refuses silence
// ─────────────────────────────────────────────────────────────────────────────

/// **WHO THE CONSUMER SAYS IT IS.** Four fields, and each one earns its place.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConsumerIdentity {
    /// **REQUIRED.** What this consumer is called — `"threads-tui"`, `"aui"`.
    /// Without it there is no ledger, only the row count the daemon already had.
    pub name: String,
    /// **REQUIRED.** The shape contract this consumer claims it was built
    /// against. A string, uninterpreted; see the module doc on why this is never
    /// a negotiation.
    pub shape_version: String,
    /// **OPTIONAL, and the field that makes a finding ACTIONABLE.** Knowing a
    /// stale consumer exists and being able to end it are different things;
    /// substrate's 2026-09-22 incident had two indistinguishable processes and
    /// no way to tell which to kill. This is that way.
    ///
    /// Optional because a consumer across a socket may not be a local process at
    /// all, and a pid that is not on this machine is worse than no pid.
    pub pid: Option<u32>,
    /// **OPTIONAL, and the field the motivating incident actually needed.** A
    /// build id — a commit sha, a build stamp, whatever the consumer knows about
    /// itself.
    ///
    /// Why it earns a place beside `shape_version`: both board processes on
    /// 2026-09-22 would have declared the SAME `shape_version`. They agreed
    /// about the contract and disagreed by three days of code. A declared
    /// contract version cannot distinguish two builds that claim the same
    /// contract, and distinguishing them is the whole ask.
    pub build: Option<String>,
}

/// The one sentence a door says when a subscribe states no identity at all.
///
/// A const for the reason [`crate::wire::VERSION_PARAM_REFUSAL`] is one: more
/// than one door reaches this rule, and a hand-written copy in each is one rule
/// telling two consumers two different things. A door differs only in the
/// envelope it wraps this in; the sentence is this.
pub const IDENTITY_REQUIRED_REFUSAL: &str =
    "`consumer` is required and must be an object: {name, shape_version, pid?, \
     build?} — say who you are. A subscribe that names no consumer cannot be \
     tracked, and tracking who is connected is what this param is for";

/// The refusal for a `consumer` that is present but is not an object.
///
/// Its own sentence, and not [`IDENTITY_REQUIRED_REFUSAL`], because *you sent a
/// string* and *you sent nothing* are different mistakes with different repairs:
/// one needs the shape, the other needs to be told the param exists.
const IDENTITY_SHAPE_REFUSAL: &str =
    "`consumer` must be an object: {name, shape_version, pid?, build?}";

/// The refusal for a missing or blank `consumer.name`.
const NAME_REQUIRED_REFUSAL: &str =
    "`consumer.name` is required and must be a non-empty string";

/// The refusal for a missing or blank `consumer.shape_version`.
const SHAPE_VERSION_REQUIRED_REFUSAL: &str =
    "`consumer.shape_version` is required and must be a non-empty string: say what you \
     were built against, even if it is a date";

impl ConsumerIdentity {
    /// Parse the `consumer` param of `shapes.subscribe`.
    ///
    /// **IDENTITY IS COMPULSORY. Absent is refused, and so is half-filled.**
    ///
    /// Operator ruling, 2026-10-08, and it INVERTED what this function used to
    /// do: *"what I want is consumers to be forced to say who they are so we can
    /// track consumers."* Before it, anonymity was served and recorded as
    /// UNIDENTIFIED while a blank `name` was refused — which is backwards for
    /// tracking, because the case a ledger cannot name is exactly the anonymous
    /// one, so it is the case that has to be refused.
    ///
    /// * Absent or `null` → refused with [`IDENTITY_REQUIRED_REFUSAL`]. `null`
    ///   reads as absent here, as it does for `version`: a client filling a
    ///   fixed request shape writes `null` for *nothing to say*.
    /// * Present but not an object → refused, in its own sentence.
    /// * Missing or blank `name` / `shape_version` → refused. A half-filled
    ///   identity is worse than none, because it looks like an answer; `""` and
    ///   `"  "` are treated as absent for the same reason.
    ///
    /// The cost — this is a BREAKING change for any anonymous subscriber — is
    /// accepted rather than denied.
    ///
    /// **What it does NOT do.** It does not compare
    /// [`ConsumerIdentity::shape_version`] against anything. Being IDENTIFIED
    /// and being VERSION-CHECKED are different rules (2.1 and 2.6) over
    /// different fields, and the ruling moved only the first.
    pub fn parse(params: &Value) -> std::result::Result<Self, String> {
        let stated = params
            .get("consumer")
            .filter(|c| !c.is_null())
            .ok_or_else(|| IDENTITY_REQUIRED_REFUSAL.to_string())?;
        let obj = stated
            .as_object()
            .ok_or_else(|| IDENTITY_SHAPE_REFUSAL.to_string())?;
        // Blank reads as absent, so one helper decides it for both required
        // fields and neither can be strict in a different way from the other.
        let stated_text = |k: &str| {
            obj.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        Ok(Self {
            name: stated_text("name")
                .ok_or_else(|| NAME_REQUIRED_REFUSAL.to_string())?
                .to_string(),
            shape_version: stated_text("shape_version")
                .ok_or_else(|| SHAPE_VERSION_REQUIRED_REFUSAL.to_string())?
                .to_string(),
            pid: obj.get("pid").and_then(Value::as_u64).map(|p| p as u32),
            build: stated_text("build").map(str::to_string),
        })
    }
}

/// **ONE CURRENTLY-SUBSCRIBED CONSUMER** — re-exported from [`crate::wire`].
///
/// It is DATA a consumer parses: it rides inside `ShapeInfo` on `shapes.list`,
/// so it lives with the rest of the frame types and this module keeps the
/// machinery. The path `crate::ledger::ConsumerEntry` resolving is the point of
/// the re-export — the ledger is where a reader looks for it.
pub use crate::wire::ConsumerEntry;

/// **WHY A JOURNAL ENTRY CLOSED.** The wire strings are the contract: a reader
/// tallying stale consumers cares a great deal which of these it is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// The consumer called `shapes.unsubscribe`. The clean case.
    Unsubscribed,
    /// The connection went away — a close frame, a read error, or a write that
    /// failed, which is how a vanished peer is discovered.
    ConnectionClosed,
    /// **A DIFFERENT CONSUMER TOOK OVER THIS CONNECTION AND SHAPE.** The rare
    /// case: the same socket subscribing again while claiming a different name,
    /// claimed contract or build.
    ///
    /// The ordinary repeat subscribe is NOT this — it is one subscription
    /// throughout (rule 2.5) and is recorded as [`EVENT_RESUBSCRIBE`], which is
    /// both truer and what makes a fit compressible. This reason exists for the
    /// case where merging the two would HIDE a fact: a consumer that came back
    /// as a different build is exactly what the operator could not see, so its
    /// arrival gets its own close and its own open.
    Resubscribed,
    /// **THE CHASSIS THAT OPENED THIS ENTRY IS GONE.** Written by the sweep at
    /// the next chassis start, against the OLD generation's boot id, which is
    /// what keeps a crash, a `SIGKILL` or an `execv` from leaving an entry open
    /// in the file forever.
    ChassisRestart,
}

impl CloseReason {
    /// The wire string. Four reasons, four distinct words, and a reader counts
    /// them.
    pub fn as_str(self) -> &'static str {
        match self {
            CloseReason::Unsubscribed => "unsubscribed",
            CloseReason::ConnectionClosed => "connection_closed",
            CloseReason::Resubscribed => "resubscribed",
            CloseReason::ChassisRestart => "chassis_restart",
        }
    }
}

/// The chassis's own RFC3339 stamp, to milliseconds, in UTC.
///
/// Four lines here rather than a dependency on a producer's store module, and
/// the format must not drift from what a journal file already holds.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. The registry — the half that answers `shapes.list`
// ─────────────────────────────────────────────────────────────────────────────

/// One live subscription, registry-side.
#[derive(Debug, Clone)]
struct Subscription {
    /// **EVERY KEY THIS SUBSCRIPTION NAMES, AND NOT THE LAST ONE IT NAMED.**
    ///
    /// Substrate had `key: Option<String>` — a scalar — and a scalar cannot hold
    /// what a subscription can be. A connection widening from `a` to `a` + `b`
    /// is one subscription holding two keys; the scalar was OVERWRITTEN, so
    /// `shapes.list` reported that consumer as watching `b` while the hub was
    /// quite correctly sending it both.
    ///
    /// `None` is a MEMBER of this set — the keyless subscribe, *"every key you
    /// know about"* — and not a hole in it. A `BTreeSet<Option<String>>` rather
    /// than a set plus a bool for the reason [`crate::wire::Resolving`] keeps
    /// its keyless slot unspellable as a key: membership is then one question
    /// with one answer, and no key a producer could mint can collide with the
    /// shape level by spelling.
    keys: BTreeSet<Option<String>>,
    /// When this SUBSCRIPTION opened. The operator's *"three days old"* is read
    /// off this field, which is why it survives a recovery cycle and must
    /// survive nothing else.
    since: String,
    identity: Option<ConsumerIdentity>,
}

/// **WHAT A SUBSCRIBE TURNED OUT TO BE.** Five outcomes, decided once, by the
/// registry, while it holds its own lock — and then matched once to pick the
/// record.
///
/// Substrate decided this inline, with three booleans (`continues`,
/// `already_held`, `replaced_identity == identity`) read in three places and a
/// chain of early returns. Naming the outcome is this rewrite's main
/// decomposition change: the decision has exactly one site, each outcome is
/// nameable in a test, and a sixth case cannot be added by accident because the
/// match is exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    /// Case 1 — this subscribe COMPLETES a deferred close on the same
    /// connection, shape and consumer. The recovery cycle, as one event.
    Resumed,
    /// Case 2a — the connection already held this shape AND this key, same
    /// consumer. Rule 2.5's *"a fresh export and nothing else"*.
    Repeated,
    /// Case 2b — the connection already held this shape, same consumer, and
    /// this key is new to it.
    Widened,
    /// Case 3, first half — a DIFFERENT consumer on a connection that already
    /// held this shape. Gets its own close before its own open; merging it into
    /// [`Admission::Repeated`] is what would hide a build change.
    Replaced,
    /// Case 3, second half — nothing was here. A genuinely new subscription.
    Opened,
}

/// The registry: every live subscription, keyed by connection and shape.
///
/// A named type rather than a bare `BTreeMap` inside [`ConsumerLedger`] so that
/// the one genuinely intricate decision in this module — what a subscribe is,
/// what `since` it inherits and which keys it starts from — happens in one
/// method with one lock held, instead of in the middle of the function that also
/// writes the records.
#[derive(Default)]
struct Registry(BTreeMap<(ConnId, String), Subscription>);

/// What the registry decided, handed back to the record writer.
struct Admitted {
    admission: Admission,
    /// Every key the subscription holds AFTER this call, as the `keys` field
    /// goes on the wire. `null` is the keyless member, spelled as itself.
    keys: Vec<Value>,
}

impl Registry {
    /// **ADMIT A SUBSCRIBE AND SAY WHAT IT WAS.**
    ///
    /// `resumes` is whether a deferred close for this exact `(conn, shape,
    /// consumer)` was waiting — the caller knows that, because it holds the
    /// deferral. `resumed_since` is that deferral's `since`, carried so a
    /// subscribe which turns out to CONTINUE a subscription can put its age
    /// back.
    fn admit(
        &mut self,
        conn: ConnId,
        shape: &str,
        key: Option<&str>,
        identity: &Option<ConsumerIdentity>,
        resumes: bool,
        resumed_since: Option<&str>,
    ) -> Admitted {
        let slot = (conn, shape.to_string());
        let held = self.0.get(&slot);
        let same_consumer = held.is_some_and(|h| &h.identity == identity);

        // ── IS THIS THE SAME SUBSCRIPTION? ───────────────────────────────────
        //
        // Everything below turns on this one question. Cases 1 and 2 continue a
        // subscription; case 3 starts one, even when it is reusing a socket.
        let continuing = resumes || same_consumer;

        // ── `since` IS ONE SUBSCRIPTION'S OWN AGE ────────────────────────────
        //
        // It survives a recovery cycle — that is the whole reason the field is
        // worth reporting — and it must survive nothing else.
        //
        // * Case 2 reads it off the entry still in the map.
        // * Case 1 had its entry removed by `disconnected`, so it comes back off
        //   the deferral.
        // * **Case 3 takes now, INCLUDING when it replaces an entry.** A
        //   different consumer on the same socket is a different subscription;
        //   letting it inherit the old one's age would report today's build as
        //   three days old, which is the exact reading the operator needed and
        //   the exact way to get it wrong.
        let since = match (held, continuing) {
            (Some(h), true) => h.since.clone(),
            (None, true) => resumed_since.map(str::to_string).unwrap_or_else(now_rfc3339),
            _ => now_rfc3339(),
        };

        // ── THE KEY SET IS ADDED TO, NEVER OVERWRITTEN ───────────────────────
        //
        // A continuing subscription joins this key to the ones it already holds.
        // Case 3 starts from this key alone, for the same reason it takes a
        // fresh `since`: inheriting yesterday's keys would report today's
        // consumer as watching what yesterday's did.
        let member = key.map(str::to_string);
        let mut keys = match (held, continuing) {
            (Some(h), true) => h.keys.clone(),
            _ => BTreeSet::new(),
        };
        let was_already_held = keys.contains(&member);
        keys.insert(member);

        let admission = if resumes {
            Admission::Resumed
        } else if same_consumer {
            if was_already_held {
                Admission::Repeated
            } else {
                Admission::Widened
            }
        } else if held.is_some() {
            Admission::Replaced
        } else {
            Admission::Opened
        };

        let wire_keys = key_list(&keys);
        self.0.insert(
            slot,
            Subscription {
                keys,
                since,
                identity: identity.clone(),
            },
        );
        Admitted {
            admission,
            keys: wire_keys,
        }
    }

    /// Drop one key from a live subscription. Returns the keys it holds after,
    /// and its identity, or `None` when nothing was subscribed.
    fn narrow(
        &mut self,
        conn: ConnId,
        shape: &str,
        key: &str,
    ) -> Option<(BTreeSet<Option<String>>, Option<ConsumerIdentity>)> {
        let held = self.0.get_mut(&(conn, shape.to_string()))?;
        held.keys.remove(&Some(key.to_string()));
        Some((held.keys.clone(), held.identity.clone()))
    }

    /// Take one subscription out of the registry.
    fn remove(&mut self, conn: ConnId, shape: &str) -> Option<Subscription> {
        self.0.remove(&(conn, shape.to_string()))
    }

    /// Every shape this connection holds. Collected rather than borrowed because
    /// the caller closes each one, which mutates this map.
    fn shapes_on(&self, conn: ConnId) -> Vec<String> {
        self.0
            .keys()
            .filter(|(c, _)| *c == conn)
            .map(|(_, shape)| shape.clone())
            .collect()
    }

    /// Who is subscribed to one shape, oldest connection first — which is
    /// `BTreeMap`'s own order over `(conn, shape)`, since connection ids are
    /// minted upwards.
    fn rows_for(&self, shape: &str) -> Vec<ConsumerEntry> {
        self.0
            .iter()
            .filter(|((_, s), _)| s == shape)
            .map(|((conn, _), held)| ConsumerEntry {
                conn: *conn,
                // The keyless member is its OWN FIELD on the wire, never a
                // `null` inside the key list — see `wire::ConsumerEntry::keys`.
                keys: held.keys.iter().flatten().cloned().collect(),
                keyless: held.keys.contains(&None),
                since: held.since.clone(),
                name: held.identity.as_ref().map(|i| i.name.clone()),
                shape_version: held.identity.as_ref().map(|i| i.shape_version.clone()),
                pid: held.identity.as_ref().and_then(|i| i.pid),
                build: held.identity.as_ref().and_then(|i| i.build.clone()),
            })
            .collect()
    }
}

/// A key set as the `keys` field goes on the wire: `null` for the keyless
/// member, spelled as itself, and `BTreeSet` order otherwise — which puts
/// `null` first, since `None < Some(_)`.
fn key_list(keys: &BTreeSet<Option<String>>) -> Vec<Value> {
    keys.iter()
        .map(|k| match k {
            Some(s) => Value::String(s.clone()),
            None => Value::Null,
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. The journal — the append-only half
// ─────────────────────────────────────────────────────────────────────────────

/// Which chassis generation wrote a record. Every line carries `chassis` and
/// `boot`; `pid` and `version` ride the `chassis_start` line.
///
/// `boot` is what makes the generation sweep possible at all: it is how a line
/// written by a chassis that is gone is told from one written by the chassis
/// that is running.
struct Generation {
    chassis: String,
    boot: String,
    pid: u32,
    version: String,
}

/// A run of identical records. `of` is the record MINUS its timestamp, so it is
/// both the comparison key and the payload of the [`EVENT_REPEAT`] record.
struct Run {
    of: Value,
    repeats: u64,
    first_at: String,
    last_at: String,
}

/// The file half. Absent until [`ConsumerLedger::attach`] wires it.
struct Journal {
    path: PathBuf,
    generation: Generation,
    /// **LAZY.** The directory is not created and the sweep does not run until
    /// the first record. A chassis nobody ever subscribes to writes NOTHING —
    /// not a directory, not a `chassis_start` line — which is what keeps every
    /// unit test that merely builds a producer out of the operator's home, and
    /// is honest besides: a producer with no consumers has no consumer history.
    started: bool,
    /// The run in progress — mechanism 3 of the compression scheme.
    run: Option<Run>,
    /// The rotation threshold. A field rather than a direct read of
    /// [`LEDGER_MAX_BYTES`] so that the MECHANISM can be exercised cheaply at a
    /// small bound. It does not excuse testing the policy at its real constant,
    /// which is [`LEDGER_MAX_BYTES`] and is crossed by one of this module's
    /// tests for real.
    max_bytes: u64,
}

impl Journal {
    /// **PUT ONE RECORD IN THE JOURNAL**, compressing a run and opening the file
    /// on the way if this is the generation's first record.
    fn push(&mut self, record: Value) {
        if !self.started {
            self.started = true;
            if let Some(dir) = self.path.parent() {
                if let Err(e) = std::fs::create_dir_all(dir) {
                    tracing::warn!(
                        dir = %dir.display(), error = %e,
                        "subscriber ledger: cannot create directory"
                    );
                    return;
                }
            }
            self.close_orphans();
            let opening = self.with_generation(serde_json::json!({
                "event": EVENT_CHASSIS_START,
                "pid": self.generation.pid,
                "version": self.generation.version,
            }));
            self.write_record(opening);
        }
        // ── MECHANISM 3: RUN-LENGTH COMPRESSION ──────────────────────────────
        //
        // A record identical to the last one WRITTEN does not produce a second
        // line; it becomes a repetition of the run in progress, summarised into
        // one `repeat` record when the run ends — or every `LEDGER_RUN_MAX`, so
        // a long fit leaves periodic evidence on disk.
        //
        // The comparison is against the record BEFORE it is stamped, because
        // every record carries `at` and two stamped records are never equal.
        if let Some(run) = self.run.as_mut() {
            if run.of == record {
                if run.repeats == 0 {
                    run.first_at = now_rfc3339();
                }
                run.repeats += 1;
                run.last_at = now_rfc3339();
                if run.repeats >= LEDGER_RUN_MAX {
                    self.settle_run();
                }
                return;
            }
        }
        self.settle_run();
        let line = self.with_generation(record.clone());
        self.write_record(line);
        self.run = Some(Run {
            of: record,
            repeats: 0,
            first_at: String::new(),
            last_at: String::new(),
        });
    }

    /// Emit the run in progress as one [`EVENT_REPEAT`] record and reset its
    /// count, KEEPING its key so a continuing fit keeps compressing.
    ///
    /// A run with no repetitions writes nothing: its single occurrence is
    /// already on disk.
    fn settle_run(&mut self) {
        let summary = {
            let Some(run) = self.run.as_mut() else { return };
            if run.repeats == 0 {
                return;
            }
            let summary = serde_json::json!({
                "event": EVENT_REPEAT,
                "repeats": run.repeats,
                "first_repeat_at": run.first_at,
                "last_repeat_at": run.last_at,
                "of": run.of,
            });
            run.repeats = 0;
            summary
        };
        let line = self.with_generation(summary);
        self.write_record(line);
    }

    /// Add the three fields every record carries, in ONE place, so no event type
    /// can forget one.
    fn with_generation(&self, mut record: Value) -> Value {
        if let Some(fields) = record.as_object_mut() {
            fields.insert("at".into(), Value::String(now_rfc3339()));
            fields.insert("chassis".into(), Value::String(self.generation.chassis.clone()));
            fields.insert("boot".into(), Value::String(self.generation.boot.clone()));
        }
        record
    }

    /// **A LEDGER FAILURE NEVER FAILS A SUBSCRIBE.** Every error here is warned
    /// and swallowed. Bookkeeping that can refuse service is a worse defect than
    /// the blindness it was added to cure, and the REGISTRY — which is what
    /// `shapes.list` answers from — is unaffected by a file that cannot be
    /// written at all.
    fn write_record(&self, record: Value) {
        let mut line = match serde_json::to_string(&record) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "subscriber ledger: unserialisable record");
                return;
            }
        };
        line.push('\n');
        self.rotate_for(line.len() as u64);
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(line.as_bytes()) {
                    tracing::warn!(
                        path = %self.path.display(), error = %e,
                        "subscriber ledger: write failed"
                    );
                }
            }
            Err(e) => tracing::warn!(
                path = %self.path.display(), error = %e,
                "subscriber ledger: open failed"
            ),
        }
    }

    /// **ROTATION: rename to `.1`, drop whatever `.1` held.**
    ///
    /// A rename and not a copy-and-truncate, so there is no instant at which a
    /// reader sees half a file and a rotation costs one syscall whatever the
    /// size. [`LEDGER_GENERATIONS`] is 1, so the total is bounded at twice
    /// [`LEDGER_MAX_BYTES`] with nothing left to prune.
    fn rotate_for(&self, incoming: u64) {
        let held = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if held + incoming <= self.max_bytes {
            return;
        }
        let previous = previous_generation_path(&self.path, LEDGER_GENERATIONS);
        if let Err(e) = std::fs::rename(&self.path, &previous) {
            tracing::warn!(
                path = %self.path.display(), error = %e,
                "subscriber ledger: rotation failed"
            );
        }
    }

    /// **THE GENERATION SWEEP — the answer to "a disconnect that never
    /// arrives".**
    ///
    /// A chassis can die in ways that run no code: `SIGKILL`, a panic in the
    /// runtime, an `execv` for a self-update. Every subscription open at that
    /// instant has a `connect` line in the file and will never get a
    /// `disconnect` one.
    ///
    /// So at the FIRST write of a new generation, replay what is on disk, find
    /// every `(boot, conn, shape)` that opened and never closed, and close each
    /// one explicitly with [`CloseReason::ChassisRestart`], stamped with
    /// `closed_by` naming the generation that did the closing. A reader that
    /// does nothing but pair connects with disconnects then gets the right
    /// answer with no rule about boot ids to know.
    ///
    /// It reads the rotated generation FIRST and the live file second, so an
    /// entry whose `connect` fell into `.1` before its `disconnect` landed is
    /// still paired. Bounded by the rotation policy at `2 × LEDGER_MAX_BYTES`,
    /// once, at the first event of a run.
    ///
    /// Unparseable lines are SKIPPED rather than fatal: the last line of a file
    /// whose writer was killed mid-write is legitimately half a line, and
    /// refusing to sweep because of it would be the failure mode doing double
    /// duty.
    fn close_orphans(&self) {
        let mut open: BTreeMap<(String, u64, String), ()> = BTreeMap::new();
        for file in [
            previous_generation_path(&self.path, LEDGER_GENERATIONS),
            self.path.clone(),
        ] {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            for line in text.lines() {
                let Ok(record) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                // A record without all three is not a subscription event. A
                // `repeat` is the deliberate case: it keeps `conn` and `shape`
                // nested inside `of`, so it lands here and is skipped.
                let (Some(boot), Some(conn), Some(shape)) = (
                    record.get("boot").and_then(Value::as_str),
                    record.get("conn").and_then(Value::as_u64),
                    record.get("shape").and_then(Value::as_str),
                ) else {
                    continue;
                };
                let entry = (boot.to_string(), conn, shape.to_string());
                match record.get("event").and_then(Value::as_str) {
                    Some(EVENT_CONNECT) => {
                        open.insert(entry, ());
                    }
                    Some(EVENT_DISCONNECT) => {
                        open.remove(&entry);
                    }
                    _ => {}
                }
            }
        }
        for (boot, conn, shape) in open.into_keys() {
            // A connect from THIS generation cannot be stale — this is the
            // generation's first write, so nothing of ours is open yet. Guarded
            // anyway: closing our own live entry would be the ledger lying about
            // the PRESENT, which is worse than a stale line about the past.
            if boot == self.generation.boot {
                continue;
            }
            self.write_record(serde_json::json!({
                "at": now_rfc3339(),
                "chassis": self.generation.chassis,
                "boot": boot,
                "event": EVENT_DISCONNECT,
                "conn": conn,
                "shape": shape,
                "reason": CloseReason::ChassisRestart.as_str(),
                "closed_by": self.generation.boot,
            }));
        }
    }
}

impl Drop for Journal {
    /// **THE LAST FLUSH.** A run in progress is summarised on the way out, so a
    /// chassis that exits cleanly leaves no count behind. One that does not exit
    /// cleanly loses the count and nothing else — see [`LEDGER_RUN_MAX`].
    fn drop(&mut self) {
        self.settle_run();
    }
}

/// `x.jsonl` → `x.jsonl.1`. Appended to the whole name rather than replacing an
/// extension, so the live file and its generation sort together and `.jsonl`
/// stays visible in both.
fn previous_generation_path(path: &Path, generation: usize) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{generation}"));
    PathBuf::from(name)
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. The ledger itself
// ─────────────────────────────────────────────────────────────────────────────

/// A close held for one event of lookahead — mechanism 2.
#[derive(Debug, Clone)]
struct Deferred {
    conn: ConnId,
    shape: String,
    identity: Option<ConsumerIdentity>,
    reason: CloseReason,
    /// **WHEN THE SUBSCRIPTION THIS CLOSES ACTUALLY OPENED**, carried so a
    /// subscribe that turns out to CONTINUE it can put its age back.
    ///
    /// Without it the recovery cycle silently resets `since` to now — and
    /// `since` is where *"three days old"* is read from, on the consumer most
    /// likely to be cycling.
    since: String,
}

/// **THE LEDGER.** One per producer; see the module doc for the two halves.
///
/// The registry half works unconditionally and needs no filesystem, which is
/// what lets `shapes.list` answer honestly in a process that never attached a
/// file — and lets the whole thing be tested without one.
#[derive(Default)]
pub struct ConsumerLedger {
    registry: Mutex<Registry>,
    journal: Mutex<Option<Journal>>,
    /// **MECHANISM 2: ONE EVENT OF LOOKAHEAD.** A close is held here instead of
    /// being written, so an `unsubscribe` followed immediately by a `subscribe`
    /// on the same connection and shape is recognised as the single recovery
    /// cycle it is. Flushed by the next event of any kind, or by the producer
    /// exiting; **never by a timer** — there is no period anywhere in this
    /// module.
    ///
    /// **Losing one deferred close to a `SIGKILL` is self-healing**, which is
    /// why deferring is safe: the generation sweep at the next chassis start
    /// closes that entry as [`CloseReason::ChassisRestart`]. And it cannot make
    /// `shapes.list` wrong for one instant, because the REGISTRY entry is
    /// removed immediately and the registry is what answers.
    deferred: Mutex<Option<Deferred>>,
}

impl ConsumerLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// **WIRE THE FILE HALF.** Called once, by the producer's entry point: a
    /// chassis that is going to serve consumers records them, and one built by a
    /// CLI verb or a test serves nobody and writes nothing.
    ///
    /// Nothing touches the filesystem here — see [`Journal::started`].
    pub fn attach(&self, path: PathBuf, chassis: &str, boot: &str, version: &str) {
        self.attach_with_max_bytes(path, chassis, boot, version, LEDGER_MAX_BYTES)
    }

    /// [`Self::attach`] with the rotation threshold named explicitly, so the
    /// rotation MECHANISM can be exercised at a small bound without writing
    /// [`LEDGER_MAX_BYTES`] of records.
    ///
    /// It is a convenience and not an excuse: the POLICY is tested at its real
    /// constant, through [`Self::attach`], by crossing 4 MiB for real.
    pub fn attach_with_max_bytes(
        &self,
        path: PathBuf,
        chassis: &str,
        boot: &str,
        version: &str,
        max_bytes: u64,
    ) {
        *self.journal.lock().unwrap() = Some(Journal {
            path,
            generation: Generation {
                chassis: chassis.to_string(),
                boot: boot.to_string(),
                pid: std::process::id(),
                version: version.to_string(),
            },
            started: false,
            run: None,
            max_bytes,
        });
    }

    /// The attached path, for diagnostics and for the tests. `None` when no file
    /// half is wired.
    pub fn path(&self) -> Option<PathBuf> {
        self.journal.lock().unwrap().as_ref().map(|j| j.path.clone())
    }

    /// **A CONSUMER SUBSCRIBED.** [`Self::connected_link`] with no link state.
    pub fn connected(
        &self,
        shape: &str,
        key: Option<&str>,
        conn: ConnId,
        identity: Option<ConsumerIdentity>,
    ) {
        self.connected_link(shape, key, conn, identity, None)
    }

    /// **A CONSUMER SUBSCRIBED, with rule 2.6's LINK STATE recorded** — *"the
    /// ledger records it"*, in 2.6's own words, for a link served on a version
    /// this producer no longer calls current.
    ///
    /// The registry is updated first and unconditionally. What reaches the FILE
    /// depends on what this subscribe turned out to be, and there are five
    /// outcomes; they are named and argued on [`Admission`], decided by
    /// [`Registry::admit`], and turned into a record exactly once below.
    ///
    /// **`superseded` is the only version fact this module carries, and it does
    /// not compare it — it is handed the verdict.** The decision belongs to
    /// [`crate::wire::ShapeContract::link`], taken once at the link. The module
    /// doc's ruling that this file never compares a version was always about
    /// [`ConsumerIdentity::shape_version`] — free text a consumer says about
    /// itself — and never about rule 2.6's integer.
    ///
    /// **It rides the connect/resubscribe record rather than a record of its
    /// own, and that is rule 2.1's compression rule rather than tidiness.** A
    /// `superseded` line emitted beside each `connect` would ALTERNATE with it,
    /// so mechanism 3 — which compares a record to the last one written — would
    /// never match, and the reconnect loop that is the common failure would put
    /// two lines per cycle on disk forever. As a field it is part of the run
    /// key, so a stale consumer reconnecting 10,000 times is one line plus
    /// `repeat` records.
    ///
    /// **ABSENT when the link is current or stated no version.** Not `null`: a
    /// `"superseded": null` on every line is the field that says *I did not
    /// look*, which is the collapse rule 2.7 names for `changed`.
    pub fn connected_link(
        &self,
        shape: &str,
        key: Option<&str>,
        conn: ConnId,
        identity: Option<ConsumerIdentity>,
        superseded: Option<crate::wire::Superseded>,
    ) {
        // Built once, so the records below cannot spell it two ways.
        let link = superseded.map(|s| serde_json::json!({"wanted": s.wanted, "current": s.current}));
        let held_close = self.take_deferred();
        let resumes = held_close
            .as_ref()
            .is_some_and(|d| d.conn == conn && d.shape == shape && d.identity == identity);
        // A deferred close this subscribe does NOT continue is a real close, and
        // it is written BEFORE anything else so the file's order is the order the
        // events happened in.
        if !resumes {
            if let Some(d) = held_close.as_ref() {
                self.write_close(d);
            }
        }
        let admitted = self.registry.lock().unwrap().admit(
            conn,
            shape,
            key,
            &identity,
            resumes,
            held_close.as_ref().map(|d| d.since.as_str()),
        );
        // Rule 2.6's link state folded into whichever record is written, in ONE
        // place, and absent when there is nothing to say.
        let with_link = |mut record: Value| -> Value {
            if let (Some(state), Some(fields)) = (link.clone(), record.as_object_mut()) {
                fields.insert("superseded".to_string(), state);
            }
            record
        };
        let subscribed = |event: &str, after: Option<&str>| {
            let mut record = serde_json::json!({
                "event": event,
                "conn": conn,
                "shape": shape,
                "key": key,
                "keys": admitted.keys,
                "consumer": identity,
            });
            if let (Some(after), Some(fields)) = (after, record.as_object_mut()) {
                fields.insert("after".to_string(), Value::String(after.to_string()));
            }
            with_link(record)
        };
        match admitted.admission {
            // Case 1 — the deferred close is completed by this subscribe. The
            // held `disconnect` is dropped rather than written, because the
            // subscription did not end; `after` names the verb that would have
            // closed it, which is how a reader tells a recovery from an
            // unsubscribe-and-think-better-of-it.
            Admission::Resumed => self.record(subscribed(
                EVENT_RESUBSCRIBE,
                held_close.as_ref().map(|d| d.reason.as_str()),
            )),
            // Case 2a — rule 2.5's recovery act, and the symptom an operator
            // reads the column to find.
            Admission::Repeated => self.record(subscribed(EVENT_RESUBSCRIBE, Some("subscribe"))),
            // Case 2b — a consumer doing its job, in its own column.
            Admission::Widened => self.record(subscribed(EVENT_WIDEN, None)),
            // Case 3 — a different consumer on this socket gets its own close
            // FIRST. Merging it into case 2a would hide a build change, which is
            // the whole defect this module exists for. The close carries no link
            // state: it is about the subscription that ENDED.
            Admission::Replaced => {
                self.record(serde_json::json!({
                    "event": EVENT_DISCONNECT,
                    "conn": conn,
                    "shape": shape,
                    "reason": CloseReason::Resubscribed.as_str(),
                }));
                self.record(subscribed(EVENT_CONNECT, None));
            }
            Admission::Opened => self.record(subscribed(EVENT_CONNECT, None)),
        }
    }

    /// **ONE SUBSCRIPTION CLOSED.**
    ///
    /// The REGISTRY entry goes immediately — `shapes.list` must never report a
    /// subscription that has ended, not even for one event. The journal LINE is
    /// deferred by one event; see [`Self::deferred`].
    ///
    /// Silent when there is no entry: an unsubscribe for something never
    /// subscribed is a no-op on the producer too, and a `disconnect` with no
    /// matching `connect` would break the pairing invariant that
    /// [`Journal::close_orphans`] and every reader depend on.
    pub fn disconnected(&self, shape: &str, conn: ConnId, reason: CloseReason) {
        let Some(closed) = self.registry.lock().unwrap().remove(conn, shape) else {
            return;
        };
        // ONLY ONE close is ever held. A second flushes the first, so two closes
        // in a row cannot collapse into one.
        if let Some(d) = self.take_deferred() {
            self.write_close(&d);
        }
        *self.deferred.lock().unwrap() = Some(Deferred {
            conn,
            shape: shape.to_string(),
            identity: closed.identity,
            reason,
            since: closed.since,
        });
    }

    /// **ONE KEY DROPPED FROM A SUBSCRIPTION THAT IS STILL LIVE** — the exact
    /// inverse of [`EVENT_WIDEN`].
    ///
    /// **The defect this closes**, from substrate's record: the producer called
    /// [`Self::disconnected`] on EVERY unsubscribe, and `disconnected` takes no
    /// key — it removes the whole `(conn, shape)` entry. So `shapes.unsubscribe
    /// {shape, key: "b"}` from a connection holding `a` and `b` closed the entry
    /// outright and `shapes.list` answered `subscribers: 1, consumers: 0` — one
    /// question, two numbers, disagreeing — while key `a`'s frames kept being
    /// delivered.
    ///
    /// Returns `true` when the narrowing EMPTIED the subscription, in which case
    /// it has also CLOSED it: dropping the last key is a close, and the caller
    /// must not have to know which of the two happened to keep the journal
    /// paired.
    ///
    /// **Why the record is its own event and not a `disconnect`.** For
    /// [`EVENT_WIDEN`]'s reason read backwards: a subscription that dropped one
    /// of its keys has not ended, and a reader pairing connects with
    /// disconnects must not see it as one. [`Journal::close_orphans`] matches
    /// only `connect` and `disconnect`, so a `narrow` is invisible to the
    /// pairing by the same construction that already hides a `widen` from it.
    pub fn narrowed(&self, shape: &str, key: &str, conn: ConnId, reason: CloseReason) -> bool {
        let Some((keys_after, identity)) = self.registry.lock().unwrap().narrow(conn, shape, key)
        else {
            // Nothing subscribed: a no-op on the producer too, and a `narrow`
            // with no `connect` would be a line about a subscription that never
            // was.
            return false;
        };
        if keys_after.is_empty() {
            // The last key is gone, so the SUBSCRIPTION is gone. One event for
            // the moment it actually ended, through the one door that keeps the
            // deferred-close pairing intact.
            self.disconnected(shape, conn, reason);
            return true;
        }
        // A close held from an earlier `disconnected` is flushed FIRST, so the
        // file's order is the order the events happened in — the same reason
        // `connected_link` flushes one it does not continue.
        if let Some(d) = self.take_deferred() {
            self.write_close(&d);
        }
        self.record(serde_json::json!({
            "event": EVENT_NARROW,
            "conn": conn,
            "shape": shape,
            "key": key,
            "keys": key_list(&keys_after),
            "consumer": identity,
        }));
        false
    }

    /// **THE CONNECTION ENDED** — every shape it held closes at once.
    ///
    /// Called from the producer's connection guard's `Drop` and from nowhere
    /// else, so the transport's own lifetime is what closes an entry. See the
    /// module doc: there is no reaper and no TTL, deliberately.
    pub fn dropped(&self, conn: ConnId) {
        let shapes = self.registry.lock().unwrap().shapes_on(conn);
        for shape in shapes {
            self.disconnected(&shape, conn, CloseReason::ConnectionClosed);
        }
    }

    /// **FLUSH EVERYTHING HELD** — the deferred close and the run in progress.
    ///
    /// For a clean shutdown, and for any reader that needs the file to be
    /// current: the tests above all, which must be able to assert on what is
    /// actually on disk rather than on what is one event away from being there.
    pub fn flush(&self) {
        if let Some(d) = self.take_deferred() {
            self.write_close(&d);
        }
        if let Some(j) = self.journal.lock().unwrap().as_mut() {
            j.settle_run();
        }
    }

    /// **WHO IS SUBSCRIBED TO `shape` RIGHT NOW**, oldest connection first. Read
    /// from the registry; see the module doc on why never from the file.
    pub fn consumers(&self, shape: &str) -> Vec<ConsumerEntry> {
        self.registry.lock().unwrap().rows_for(shape)
    }

    fn take_deferred(&self) -> Option<Deferred> {
        self.deferred.lock().unwrap().take()
    }

    fn write_close(&self, d: &Deferred) {
        self.record(serde_json::json!({
            "event": EVENT_DISCONNECT,
            "conn": d.conn,
            "shape": d.shape,
            "reason": d.reason.as_str(),
        }));
    }

    fn record(&self, event: Value) {
        if let Some(j) = self.journal.lock().unwrap().as_mut() {
            j.push(event);
        }
    }
}

impl Drop for ConsumerLedger {
    /// A producer that exits cleanly leaves nothing held: the deferred close and
    /// the run in progress are both written on the way out.
    fn drop(&mut self) {
        self.flush();
    }
}

#[cfg(test)]
mod tests;
