//! **Serving a warnings shape is part of what it means to be a cauldron
//! service.** This module is the contract, and it holds BOTH halves of the
//! enforcement.
//!
//! * **The mechanism is a compile-time check on the producer build.**
//!   [`ServesWarnings`], [`Attestation`] and [`attest_warnings!`] — a producer
//!   that does not serve a warnings shape **does not link**. Start reading at
//!   the `THE PRODUCER'S COMPILE-TIME OBLIGATION` banner; the argument for
//!   putting it there rather than in the chassis or in `cauldron build` is
//!   written out beside it. Proved by `scripts/prove-compile-gate`.
//! * **The backstop is the runtime refusal.** [`check`] and [`Compliance`],
//!   driven by `cli/`. It is what catches the two things a build cannot see: a
//!   binary that compiled fine and then lies, and a process started by
//!   something other than `cauldron start`.
//!
//! # The decision this encodes
//!
//! The operator wanted centralized warning screening, and the shape it could
//! have taken was routing every service's traffic through the supervisor so
//! that the supervisor saw everything. That design was rejected. The
//! supervisor is **an ordinary consumer**: it opens a socket, asks one
//! question, reads one answer, and hangs up. It never sees a data frame, and
//! it still has the whole picture.
//!
//! The honest cost of that choice is that it only aggregates what producers
//! **declare**. The operator's answer was to close the gap by contract: *"yes
//! lets make them export a warnings shape."* So a service that does not serve
//! warnings is not a well-formed cauldron service.
//!
//! His second answer was *where* that is refused: *"refuse. It should be a
//! compile-time check on the producer build specifically."* A runtime refusal
//! is enforcement a service can be SHIPPED without — you only find out at
//! `cauldron start`. So a non-compliant producer does not build, and the
//! runtime refusal is what remains for the cases a build cannot reach.
//!
//! # Why the supervisor can CHECK rather than TRUST
//!
//! A boolean in a declaration is a claim the declaration makes about a binary
//! it does not contain. It can be wrong in both directions — set on a service
//! that serves nothing, absent on a service that serves it fine — and nothing
//! finds out. So the declaration does not carry a boolean. It carries, at
//! most, a **shape name**: *the question to ask.* Compliance is then a
//! property of the **running process**, established the same way a seam is —
//! by a handshake with the socket — and a declaration cannot lie its way past
//! it, because the answer has to come back.
//!
//! Three things make the answer checkable rather than merely present:
//!
//! 1. it must be the shape that was asked for (`shape` echoes the request);
//! 2. it must name the producer, and the producer must be the service whose
//!    seam we opened — otherwise one service's quiet could be credited to a
//!    noisy one;
//! 3. it must carry `counts_since`, so a count that restarted is visible as a
//!    restart rather than read as a drop to zero. This is the field the
//!    reference implementation calls out specifically: the counters outlive
//!    the shape's own epoch, so the epoch is not the counting window.
//!
//! # THE CLOSED VOCABULARY — how "not in the data path" is made falsifiable
//!
//! [`SupervisorAsk`] is every word the supervisor is allowed to put on a
//! service's socket. There are two, `HELLO` and `WARNINGS`, and the list is
//! checked against its own length by a compile-time assertion below. Adding a
//! third — a `Relay`, a `Forward`, a `Frames` — **does not compile** until
//! somebody edits the assertion, which is the review this guard exists to
//! force. Every socket write in this workspace's supervisor path goes through
//! [`SupervisorAsk::word`]: [`crate::seams::probe`] writes
//! `SupervisorAsk::Hello`, and [`ask`] writes `SupervisorAsk::Warnings`.
//!
//! The PRODUCER side now reads the same list. [`dispatch`] answers those two
//! words with a `match` over the enum rather than over strings, so a third
//! variant is a second place that stops compiling in the same edit — and
//! [`shadows_a_supervisor_word`] is the `const` assertion that stops a service
//! from claiming either word for itself.
//!
//! That is the static half. The runtime half lives at the producer:
//! `testkit/seam-probe` records the verb sequence of every connection it
//! serves, and `scripts/prove-warnings` asserts that no connection which began
//! with a supervisor word ever asked for a data frame — while a separate
//! consumer connection *does* get data frames, so the assertion is about
//! restraint and not about absence.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::seams::{Seam, SeamState, Verdict};

/// The shape every cauldron service serves unless its declaration names
/// another one.
///
/// **A correction, 2026-10-07.** This constant used to be documented as
/// matching "the reference implementation's `Shape::NAME`
/// (`lib/threads/src/shapes/warnings.rs`)". **That file does not exist and
/// never did.** The live `threads` daemon serves seven shapes and none of them
/// is `warnings`; its shape modules are `accounts`, `body`, `heartbeat`,
/// `log`, `narration`, `recent`, `summary` and `threads`, and asking the
/// running daemon for a `warnings` shape answers `unknown shape: warnings`.
///
/// The claim is corrected rather than deleted because it was load-bearing in
/// the wrong direction: a reader deciding how to adopt `threads` would have
/// concluded from it that the adoption could be verified by behaviour, and
/// built the verb on an assumption the daemon does not support. The name
/// `warnings` is still the right default — it is the name a `threads` shape
/// WOULD take — but nothing is serving it yet.
pub const SHAPE: &str = "warnings";

/// The wire version this build of the contract speaks. A service answering a
/// higher number is accepted and flagged, never refused: refusing it would
/// make the supervisor the thing that has to be upgraded first.
pub const SHAPE_VERSION: u32 = 1;

/// How long the supervisor waits for a warnings answer. The same budget as
/// [`crate::seams::PROBE_TIMEOUT`], and for the same reason: this is one
/// request/response on a loopback socket, and a service that cannot answer it
/// in a second and a half is not answering.
pub const ASK_TIMEOUT: Duration = Duration::from_millis(1500);

// ═══════════════════════════════ THE PRODUCER'S COMPILE-TIME OBLIGATION ══════
//
// **THE ENFORCEMENT IS A BUILD FAILURE, NOT A REFUSAL AT START.**
//
// Everything above this line is the SUPERVISOR's half: it asks, it checks, it
// refuses. Everything below is the PRODUCER's half, and it is checked by
// `rustc` rather than by `cauldron`. The operator's words were *"it should be a
// compile-time check on the producer build specifically"*, and the argument is
// that a runtime refusal is enforcement a service can be SHIPPED without — you
// only find out at `cauldron start`. A build failure cannot be deferred.
//
// # Where the obligation attaches, and why HERE
//
// Not on the chassis (`shapewire`'s `Shape` trait), because `shapewire` is
// meant to be extractable and open-sourceable and an opinion about warnings
// baked into it would travel with it to places that have no supervisor. Not in
// a macro a service opts into, because opt-in is the thing being overruled.
// And not in `cauldron build`, which shells out to cargo: a check cauldron runs
// *after* cargo returns is the runtime check moved earlier, not a compile-time
// check at all.
//
// It attaches to [`crate::seams::ServiceEnv::register`] — **the one call that
// writes a seam, and therefore the one call that makes a process visible to
// cauldron at all.** `register` takes an [`Attestation`], whose field is
// private to this module, so the ONLY way to obtain one is
// [`attest_warnings!`], which does not type-check unless the producer
// implements [`ServesWarnings`]. A producer with no warnings shape therefore
// **does not link**: it cannot write a seam, so there is no address for a
// consumer to read and nothing for `status` to find.
//
// That is narrower and more precise than a crate boundary could be. `cli`
// links this crate too and is unaffected, because the supervisor never
// registers a seam. The obligation lands on the act of BEING a producer.
//
// # The one escape, and why it is not one
//
// A binary could refuse the obligation by not linking this crate at all and
// writing `seams.json` itself. It would then be writing a file whose format,
// locking and header validation are all [`crate::seams`]'s
// ([`crate::seams::Table::load`] refuses a table that disagrees with the
// directory it was loaded from), it would have had to reimplement all of that on
// purpose, and the first thing `cauldron status` would do is ask it for its
// warnings shape and name it a violation. That is the BACKSTOP's case, not a gap
// in the gate: the gate's claim is that a producer built the normal way cannot
// omit the shape, not that a hand-rolled impostor is impossible. Nothing in a
// compiler can make that claim about a file on disk.
//
// # THE ESCAPE THAT IS REAL: A HOSTED BINARY THAT DOES NOT LINK THIS CRATE
//
// **The section above was written about a hypothetical impostor. This one is
// about real processes, and it is the harder case, because they are
// innocent.**
//
// `threads` and `tts` do not link this crate. Not out of evasion — they are a
// different repository, they were built before any of this existed, and they
// have their own release pipelines. Cauldron now HOSTS them: it installs their
// images as versions, forks them, watches them, restarts them, rolls them back
// and stops them. What it cannot do is be handed a seam by them, so
// [`crate::seams::record_hosted`] writes one, and that function takes no
// [`Attestation`] because there is no producer in hand to demand one from and
// no build to fail. **A compile-time obligation cannot reach a binary that
// does not link the crate the obligation lives in.** That is not a weakness in
// the gate; it is the gate's domain being what it is.
//
// The three ways out, and why only one of them is honest:
//
// 1. **Refuse to host anything that cannot attest.** The gate stays pristine
//    and cauldron hosts nothing — not the ledger service, not the voice
//    service, nothing the operator actually runs. The contract would have
//    protected a number by making the supervisor empty. REJECTED.
// 2. **Let the supervisor mint an attestation on the child's behalf.** This
//    type-checks and it is a lie: the supervisor would be swearing to a
//    property it cannot know, and an attestation that the supervisor can hand
//    itself is an attestation that means nothing anywhere. REJECTED, and it is
//    why [`Attestation`]'s field stays private and `record_hosted` takes no
//    attestation parameter at all rather than an `Option` — there is no
//    argument position in which this mistake can be made.
// 3. **Host it, and make the AGGREGATE carry the hole by name.** The
//    obligation moves from compile time to the aggregate's own accounting, in
//    the direction that admits more rather than less. CHOSEN.
//
// What (3) costs, stated plainly: `cauldron status` reports a warnings total
// that does not include `threads` or `tts`. What it buys is that the total
// SAYS SO, every time, by name, with the reason —
// [`Compliance::Unregistered`], [`Aggregate::unregistered`],
// [`Aggregate::is_complete`] and [`Aggregate::incompleteness`].
//
// # WHAT CHANGED WHEN `adopt` WAS DELETED, since this is the field that
// carried it
//
// The gap used to be declared. `[adopt]`'s `warnings_askable = false` was a
// boolean an operator wrote by hand into a TOML file about a binary the file
// does not contain, and `check` had a branch that would ASK a process whose
// declaration said `true` and credit a good answer. Two things were wrong with
// that and both are now gone:
//
// * **It could be wrong in both directions with nothing finding out.** A
//   `true` on a process that does not parse `WARNINGS` meant writing an
//   unparseable word onto a production socket — on this machine, onto the
//   socket the operator's voice comes out of. A `false` on a process that
//   would have answered meant a permanently understated total.
// * **It did not close by itself.** It stayed `false` until a human edited it.
//
// The gap is now DERIVED from [`crate::seams::Seam::registered`], which is
// written by whichever function wrote the row and so cannot be wrong about
// which function wrote the row. There is no knob. And it is self-closing: the
// day `threads` links this crate and calls `register`, its row is
// `registered = true`, the ask is made like any other producer's, and the gap
// is gone with no edit anywhere.
//
// **The honest footnote, because it is measured and it cuts against the neat
// story.** `threads` DOES serve a warnings shape today — `lib/threads/src/
// shapes/warnings.rs` registers a shape named `warnings`, measured 2026-10-08
// — but over shapewire's websocket chassis, not over cauldron's one-word line
// protocol. So the counts exist and cauldron still cannot read them. That is
// an argument for `threads` registering, which is the thing that closes this,
// and it is not an argument for a flag that would have the supervisor write
// `WARNINGS\n` at a websocket handshake and call the silence compliance.
//
// # What compile time can prove, and what it cannot
//
// It proves the obligation is DECLARED and WIRED: that an impl exists, that it
// has a body (there is no default), that the shape name is usable, that the
// `WARNINGS` word is answered by cauldron's own code ([`dispatch`]) rather than
// by the service, and that the service has not shadowed that word with a verb
// of its own.
//
// It cannot prove the ANSWER is honest — that the counts are real, that
// `producer` names this service, that `counts_since` is this process's start.
// Those are properties of a running process and they stay where they were: the
// supervisor's runtime check ([`check`]), which is now the BACKSTOP rather than
// the mechanism. It is also what catches a process started by something other
// than `cauldron start`.

/// A byte-for-byte `==` on `&str` that works in `const` context, so the
/// assertions below are evaluated by the compiler rather than by a test.
pub const fn const_str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// **WHAT A CAULDRON SERVICE OWES, AND THE SENTENCE IT GETS IF IT DOES NOT.**
///
/// The `#[diagnostic::on_unimplemented]` block is the whole point of writing
/// this as a trait: a compile error that merely says *a bound is not satisfied*
/// is a riddle, and the runtime refusals in this module all name what is
/// missing, so the compile-time one has to as well.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a cauldron service: it serves no warnings shape",
    label = "this producer declares no warnings shape",
    note = "SERVING A WARNINGS SHAPE IS PART OF WHAT IT MEANS TO BE A CAULDRON SERVICE. The \
            supervisor aggregates warnings by subscribing to every producer like an ordinary \
            consumer -- it is never in the data path -- so a producer that serves none is a hole \
            in the aggregate that nothing else can fill.",
    note = "WHAT IS MISSING: `impl env::warnings::ServesWarnings for {Self}`, carrying TWO \
            items. (1) `const SHAPE: &'static str` -- the shape name this service answers \
            `WARNINGS` with; the default is `env::warnings::SHAPE`, i.e. \"warnings\", and it \
            must match the `warnings = \"<name>\"` line in `services/<name>.toml`. (2) `fn \
            warnings(&self, asked: &str) -> anyhow::Result<env::warnings::Export>` -- the \
            counts: `warn` and `error` COUNTED APART, `producer` naming this service, and \
            `counts_since` carrying THIS PROCESS's start (never the shape's own epoch, or a \
            restart reads as a drop to zero).",
    note = "THERE IS NO DEFAULT BODY AND NO OPT-OUT. `ServiceEnv::register` -- the one call \
            that writes a seam, and so the one call that makes a process visible to cauldron at \
            all -- takes an `env::warnings::Attestation`, and the only way to mint one is \
            `env::warnings::attest_warnings!({Self})`, which does not type-check without this \
            impl. A producer with no warnings shape does not link, cannot write a seam, and has \
            no address for anything to read."
)]
pub trait ServesWarnings {
    /// **THE SHAPE NAME THIS SERVICE ANSWERS `WARNINGS` WITH**, declared at
    /// compile time. Asserted usable by [`attest_warnings!`], with the same
    /// character rule the declaration parser applies, so the two cannot drift.
    const SHAPE: &'static str;

    /// **THE COUNTS.** `asked` is the shape name the caller put on the wire;
    /// a service serving one shape answers `Err` for any other, which is what
    /// the supervisor reads as [`Compliance::NotServed`].
    ///
    /// **NO DEFAULT BODY, deliberately.** A default would be a way to inherit
    /// silence, and every refusal in this module exists because silence is the
    /// failure mode.
    fn warnings(&self, asked: &str) -> Result<Export>;
}

/// One connection, as a verb handler sees it: somewhere to write, and the
/// running count of DATA FRAMES this connection has been handed.
///
/// `frames` is here rather than in the service because the claim it feeds is
/// the supervisor's: a connection that opened with a word from
/// [`SupervisorAsk`] is asserted never to have carried one.
pub struct Conn<'a> {
    pub out: &'a mut dyn Write,
    pub frames: u64,
}

impl Conn<'_> {
    /// One line out, newline appended.
    pub fn line(&mut self, s: &str) -> std::io::Result<()> {
        self.out.write_all(s.as_bytes())?;
        self.out.write_all(b"\n")
    }
}

/// One verb a SERVICE answers for itself — its own half of the socket.
///
/// A function pointer rather than a match arm, so the table IS the dispatcher
/// ([`dispatch`] reads nothing else) and cannot drift from what the service
/// actually answers.
pub struct Verb<S: 'static> {
    pub word: &'static str,
    pub run: fn(&S, &mut Conn<'_>, &str) -> Result<()>,
}

impl<S: 'static> Verb<S> {
    pub const fn new(word: &'static str, run: fn(&S, &mut Conn<'_>, &str) -> Result<()>) -> Self {
        Self { word, run }
    }
}

/// **A SERVICE'S SOCKET.** The supertrait bound is the lock: writing
/// `impl Service for X` without [`ServesWarnings`] is the compile error whose
/// text is quoted above.
pub trait Service: ServesWarnings + Sized + 'static {
    /// Every verb this service answers ITSELF. May not contain a word the
    /// supervisor owns — asserted by [`attest_warnings!`].
    const VERBS: &'static [Verb<Self>];

    /// This service's environment, for the identity handshake.
    fn service_env(&self) -> &crate::seams::ServiceEnv;

    /// The address it actually bound, for the same.
    fn addr(&self) -> SocketAddr;
}

/// Did [`dispatch`] recognise the verb?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answered {
    /// Handled — by cauldron for a supervisor word, by the service otherwise.
    Yes,
    /// Not a word cauldron owns and not in [`Service::VERBS`]. The service's
    /// own business, and usually a warning at its own call site.
    NotMine,
}

/// **CAULDRON ANSWERS THE SUPERVISOR'S WORDS; THE SERVICE ANSWERS ITS OWN.**
///
/// The `match` below is over [`SupervisorAsk`] rather than over strings, so the
/// vocabulary is exhaustive by construction: a third variant makes this
/// function stop compiling in the same edit that breaks
/// [`ALL_IS_EXHAUSTIVE`].
///
/// This is why the compile-time check is about more than a decorative impl.
/// `WARNINGS` is routed by THIS function, which calls
/// [`ServesWarnings::warnings`] — so an impl that exists is an impl that is
/// reached, and a service cannot have one without serving it.
pub fn dispatch<S: Service>(
    svc: &S,
    verb: &str,
    rest: &str,
    c: &mut Conn<'_>,
) -> Result<Answered> {
    for ask in SupervisorAsk::ALL {
        if ask.word() != verb {
            continue;
        }
        match ask {
            SupervisorAsk::Hello => {
                let id = svc.service_env().identity(&svc.addr());
                c.line(&serde_json::to_string(&id)?)?;
            }
            SupervisorAsk::Warnings => match svc.warnings(rest) {
                Ok(export) => c.line(&serde_json::to_string(&export)?)?,
                // A producer's own refusal, verbatim on the wire. The
                // supervisor reads this as `NotServed` and names the shape it
                // asked for; nothing here tries to interpret it.
                Err(e) => c.line(&format!("ERR {e:#}"))?,
            },
        }
        return Ok(Answered::Yes);
    }
    for v in S::VERBS {
        if v.word == verb {
            (v.run)(svc, c, rest)?;
            return Ok(Answered::Yes);
        }
    }
    Ok(Answered::NotMine)
}

/// Is `verb` a word the supervisor owns? The `const` twin of
/// [`SupervisorAsk::is_supervisor_word`], so the shadowing assertion can run at
/// compile time.
pub const fn is_supervisor_word_const(verb: &str) -> bool {
    let mut i = 0;
    while i < SupervisorAsk::ALL.len() {
        if const_str_eq(SupervisorAsk::ALL[i].word(), verb) {
            return true;
        }
        i += 1;
    }
    false
}

/// Does a service's verb table SHADOW one of the supervisor's two words?
///
/// A service that answers `WARNINGS` itself would look compliant from the
/// outside while [`ServesWarnings::warnings`] was never called — the aggregate
/// starved by a service that has the impl. That is the one way a declared
/// obligation could be unwired, and it is a build failure.
pub const fn shadows_a_supervisor_word<S: 'static>(verbs: &[Verb<S>]) -> bool {
    let mut i = 0;
    while i < verbs.len() {
        if is_supervisor_word_const(verbs[i].word) {
            return true;
        }
        i += 1;
    }
    false
}

/// The same character rule [`crate::decl::WarningsDecl`] applies to a shape
/// name in a declaration, in `const` form — so a shape name the TOML parser
/// would reject cannot be smuggled in from the producer side either.
pub const fn shape_is_usable(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if !(c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'/') {
            return false;
        }
        i += 1;
    }
    true
}

// `#[macro_export]` puts the macro at the crate root; this re-export lets it be
// written `env::warnings::attest_warnings!`, beside the trait it is about.
pub use crate::attest_warnings;

/// Private, so [`Attestation`] cannot be constructed outside this module.
#[derive(Debug, Clone, Copy)]
struct Sealed;

/// **PROOF, CARRIED IN THE TYPE SYSTEM, THAT A PRODUCER SERVES WARNINGS.**
///
/// [`crate::seams::ServiceEnv::register`] takes one. Its field is private to
/// this module and it has no `Default`, no public constructor and no `From`, so
/// there is exactly one way to come by it: [`attest_warnings!`].
#[derive(Debug, Clone, Copy)]
pub struct Attestation {
    shape: &'static str,
    _sealed: Sealed,
}

impl Attestation {
    /// Minted by [`attest_warnings!`] only. Public because a macro expands at
    /// the call site, hidden because it is not an API.
    #[doc(hidden)]
    pub const fn __mint<S: Service>() -> Self {
        Self { shape: S::SHAPE, _sealed: Sealed }
    }

    /// The shape name the producer declared at COMPILE TIME. Distinct from the
    /// shape it answers with at runtime, which a service is allowed to make
    /// configurable and which the supervisor checks separately.
    pub const fn shape(&self) -> &'static str {
        self.shape
    }
}

/// **THE DOOR.** Mint the [`Attestation`] that
/// [`crate::seams::ServiceEnv::register`] demands, and run the two `const`
/// assertions that go with it.
///
/// Modelled on `shapewire`'s `declare_operation!`: the macro exists so the
/// assertions cannot be forgotten, because there is no other way through.
///
/// ```ignore
/// let attested = env::warnings::attest_warnings!(MyService);
/// se.register(&addr, &binary, &probe, &attested)?;
/// ```
#[macro_export]
macro_rules! attest_warnings {
    ($ty:ty) => {{
        // THE FIRST COMPILE-TIME CHECK. A shape name the declaration parser
        // would reject cannot be declared by the producer either. The message
        // is a plain literal: `assert!` reads its second argument as a FORMAT
        // string, and the span already points at the offending type.
        const _: () = assert!(
            $crate::warnings::shape_is_usable(
                <$ty as $crate::warnings::ServesWarnings>::SHAPE
            ),
            "this cauldron service's `ServesWarnings::SHAPE` is not a usable shape name. It \
             must be non-empty and [A-Za-z0-9_-/], which is the same rule \
             `services/<name>.toml` applies to `warnings = \"<name>\"`. An empty shape name is \
             not a way of declaring non-compliance: `warnings = false` in the declaration is, \
             and `cauldron start` refuses it by name. The default is `env::warnings::SHAPE`.",
        );
        // THE SECOND. A service that answers `WARNINGS` itself would never have
        // `ServesWarnings::warnings` called, so the impl would be decorative
        // and the aggregate would be starved by a service that looks compliant.
        const _: () = assert!(
            !$crate::warnings::shadows_a_supervisor_word(
                <$ty as $crate::warnings::Service>::VERBS
            ),
            "this cauldron service's `Service::VERBS` shadows one of the supervisor's two \
             words. `HELLO` and `WARNINGS` are answered by cauldron, in \
             `env::warnings::dispatch`, which is what calls `ServesWarnings::warnings` -- so a \
             service that claims either word for itself has an impl that is never reached and \
             an aggregate it silently starves. Remove that entry; put the behaviour in \
             `ServesWarnings::warnings`.",
        );
        $crate::warnings::Attestation::__mint::<$ty>()
    }};
}

// ───────────────────────────────────────────────── the closed vocabulary

/// **EVERY WORD THE SUPERVISOR MAY SAY TO A SERVICE.** There are two.
///
/// This type exists so that "the supervisor is not in the data path" is a
/// statement about the code and not about anybody's intentions. A relay needs
/// a verb; a verb needs a variant; a variant breaks [`ALL_IS_EXHAUSTIVE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorAsk {
    /// The seam identity handshake. Proves *which* instance holds an address.
    Hello,
    /// The warnings shape. The only thing the supervisor ever reads a payload
    /// from, and it is a count, not a frame.
    Warnings,
}

impl SupervisorAsk {
    /// Every variant, in the order they were added.
    pub const ALL: &'static [SupervisorAsk] = &[SupervisorAsk::Hello, SupervisorAsk::Warnings];

    pub const fn word(self) -> &'static str {
        match self {
            SupervisorAsk::Hello => "HELLO",
            SupervisorAsk::Warnings => "WARNINGS",
        }
    }

    /// Is `verb` a word the supervisor is allowed to have said? Used by the
    /// proof to partition a service's connection log into supervisor-shaped
    /// and consumer-shaped.
    pub fn is_supervisor_word(verb: &str) -> bool {
        Self::ALL.iter().any(|a| a.word() == verb)
    }
}

/// **THE COMPILE-TIME HALF OF "NOT IN THE DATA PATH".**
///
/// `SupervisorAsk::ALL` must have exactly two members. A third verb — any
/// third verb, whatever it is called — is a compile error here, which is the
/// whole point: a future edit that teaches the supervisor to ask for frames
/// cannot land quietly. If you are reading this because the build broke, the
/// question to answer out loud is *why does the supervisor need to say a third
/// word to a service*, and the answer must not be "to move data".
pub const ALL_IS_EXHAUSTIVE: [(); 2] = [(); SupervisorAsk::ALL.len()];

// ───────────────────────────────────────────────────────── what is served

/// One kind of warning. **A kind is a CALL SITE, not a message string** —
/// `(target, line)`. The reference implementation measured why: one escalating
/// `error!` in the live daemon appeared under 18 distinct message strings, so
/// keyed by text it was 18 kinds and the breakdown was all one defect, while
/// keyed by call site it was one kind with a count of 53.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kind {
    /// The module (or file) that warned.
    pub target: String,
    /// The source line. With `target`, this IS the kind.
    pub line: u32,
    /// `warn` or `error`. Carried so a reader of the breakdown can tell a
    /// defect from a condition without re-deriving it from the totals.
    pub level: String,
    pub count: u64,
    /// The FIRST message this call site wrote, as its human label.
    #[serde(default)]
    pub message: String,
}

/// **THE WHOLE FACT, as the supervisor receives it.** One JSON line.
///
/// The field names are the reference implementation's, so a service that
/// already serves that shape serves this contract by re-encoding rather than
/// by re-deriving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Export {
    /// Echoes the shape that was asked for. Checked.
    pub shape: String,
    pub shape_version: u32,
    /// **The producer naming itself.** Checked against the seam's service, so
    /// one service's silence cannot be credited to another's noise.
    pub producer: String,
    /// **When the counting started** — the producer's own start, never the
    /// shape's epoch. Not optional: without it a restart reads as a drop to
    /// zero, and a count that silently resets is the lie this whole feature
    /// exists to stop telling.
    pub counts_since: String,
    pub warn: u64,
    /// Counted apart from `warn`, never folded into it. A face with room for
    /// one number calls [`Export::total`] and can say that it summed.
    pub error: u64,
    /// Writes to the producer's durable record that FAILED. Carried here
    /// **because it cannot be logged**: a log-write failure reported into the
    /// log is the original defect, recursive.
    #[serde(default)]
    pub sink_errors: u64,
    /// Whether a durable record is attached at all. A different statement
    /// from `sink_errors == 0`.
    #[serde(default)]
    pub sink_attached: bool,
    /// The busiest call sites, descending by count.
    #[serde(default)]
    pub kinds: Vec<Kind>,
}

impl Export {
    pub fn total(&self) -> u64 {
        self.warn + self.error
    }
}

// ─────────────────────────────────────────────────────────── the verdict

/// What the supervisor found when it asked. Serialised into `status --json`,
/// so the operator's machine-readable view carries the refusal reason too.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "compliance", rename_all = "kebab-case")]
pub enum Compliance {
    /// The service serves the shape and the answer checked out.
    Served { export: Box<Export> },
    /// The DECLARATION confesses it: `warnings = false`. Refused before a
    /// process is ever spawned, because there is nothing to ask.
    RefusedByDeclaration { service: String },
    /// Nothing to ask: the seam is down or stale, so there is no live
    /// producer. Not a contract failure — a contract failure requires a
    /// running service that will not answer.
    NoProducer { state: String },
    /// Connected, asked, and the service did not serve the shape.
    NotServed { shape: String, detail: String },
    /// Answered a DIFFERENT shape than the one asked for.
    WrongShape { asked: String, answered: String },
    /// Answered in another service's name.
    WrongProducer { expected: String, answered: String },
    /// Answered something that is not a warnings export at all.
    Unparseable { detail: String },
    /// **A HOSTED process that cannot be asked for warnings — a NAMED GAP,
    /// and deliberately NOT a violation.**
    ///
    /// A [`Compliance::NotServed`] is a service that owes the contract and
    /// refuses it; `status` exits 4 and somebody has to fix a breach. A
    /// process whose row was written by the supervisor never signed the
    /// contract: it does not link this crate, so the compile-time obligation
    /// never ran against it and cannot retroactively. Filing it as a violation
    /// would demand a fix for a breach nobody committed, and would make the
    /// exit code lie about whose fault it is.
    ///
    /// So it is a gap: counted in the DENOMINATOR of [`Aggregate::coverage`]
    /// and never in the numerator, printed by name, and announced as something
    /// the total EXCLUDES. The aggregate stays honest by ADMITTING the hole
    /// rather than by hiding it or by miscategorising it.
    ///
    /// **Nothing is attempted over the socket**, and that is a change from the
    /// variant this replaced. There is no field saying what was tried, because
    /// nothing is tried: a binary that does not link this crate does not parse
    /// `WARNINGS`, and writing a word it cannot parse onto a production socket
    /// is a side effect with nothing to show for it. The old
    /// `warnings_askable = true` path that would have tried anyway is deleted
    /// along with the knob that fed it.
    Unregistered {
        service: String,
        /// The shape the declaration named, which is what the service WOULD be
        /// asked for the day it registers. Carried so the sentence can say
        /// what closing the gap looks like rather than only that it is open.
        shape: String,
    },
}

impl Compliance {
    /// The export, if there is one. `None` for every refusal.
    pub fn export(&self) -> Option<&Export> {
        match self {
            Compliance::Served { export } => Some(export),
            _ => None,
        }
    }

    /// Is this a CONTRACT FAILURE — a service that exists and will not serve
    /// warnings? Distinguished from `NoProducer`, which is only the absence of
    /// anything to ask.
    pub fn is_violation(&self) -> bool {
        !matches!(
            self,
            Compliance::Served { .. }
                | Compliance::NoProducer { .. }
                // NOT a violation. See the variant's own doc: a process
                // that does not link this crate never signed the contract, so
                // its silence is a gap to be named, not a breach to be fixed.
                // This is the single line that keeps `status`'s exit 4 meaning
                // what it says.
                | Compliance::Unregistered { .. }
        )
    }

    /// **Is this a HOSTED service the aggregate cannot cover?**
    ///
    /// Separate from [`Compliance::is_violation`] on purpose: the two are the
    /// two kinds of hole, and collapsing them would mean either accusing a
    /// process of a breach it never signed up to or excusing a real one.
    pub fn is_unregistered_gap(&self) -> bool {
        matches!(self, Compliance::Unregistered { .. })
    }

    /// **THE SENTENCE THAT NAMES WHAT IS MISSING.** `None` when nothing is.
    ///
    /// Every refusal path produces one of these and every refusal path prints
    /// it, because a refusal that does not say what to add makes the contract
    /// a riddle.
    pub fn missing(&self) -> Option<String> {
        match self {
            Compliance::Served { .. } | Compliance::NoProducer { .. } => None,
            Compliance::RefusedByDeclaration { service } => Some(format!(
                "its declaration says `warnings = false`. Serving a warnings shape is part of \
                 what it means to be a cauldron service: the supervisor aggregates warnings by \
                 subscribing to every producer, so a producer that declares none is a hole in \
                 the aggregate that nothing else can fill. Remove that line (the default is \
                 `warnings = \"{SHAPE}\"`) and make `{service}` answer the `WARNINGS` request \
                 with its counts."
            )),
            Compliance::NotServed { shape, detail } => Some(format!(
                "it does not serve the `{shape}` shape. The supervisor asked it `WARNINGS \
                 {shape}` on its own seam and got: {detail}. Answer that request with one JSON \
                 line carrying at least {{\"shape\":\"{shape}\",\"shape_version\":{SHAPE_VERSION},\
                 \"producer\":\"<service>\",\"counts_since\":\"<this process's start>\",\
                 \"warn\":<n>,\"error\":<n>}}, or declare a different shape name."
            )),
            Compliance::WrongShape { asked, answered } => Some(format!(
                "it was asked for the `{asked}` shape and answered `{answered}`. Either serve \
                 `{asked}` or declare `warnings = \"{answered}\"` — the supervisor will not \
                 silently accept a different shape than the one the declaration named."
            )),
            Compliance::WrongProducer { expected, answered } => Some(format!(
                "its warnings shape names producer `{answered}`, but the seam it was read from \
                 belongs to `{expected}`. The supervisor refuses to credit one service's \
                 warnings to another."
            )),
            Compliance::Unparseable { detail } => Some(format!(
                "it answered something that is not a warnings export: {detail}"
            )),
            Compliance::Unregistered { service, shape } => Some(format!(
                "`{service}` is HOSTED and contributes NOTHING to the total. Cauldron forks it, \
                 watches it, restarts it and stops it — but it does not link cauldron's `env` \
                 crate, so it never satisfied the compile-time warnings obligation and cannot \
                 satisfy it retroactively: `ServiceEnv::register` is the call that demands an \
                 attestation, and this process never made it. Its row was written FOR it by the \
                 supervisor that forked it.\n\
                 The supervisor did not ask, and there is no setting that would make it ask. A \
                 binary that does not link this crate does not parse `WARNINGS`, so putting \
                 that word on its socket is a side effect with nothing to show for it — and on \
                 this machine one of those sockets is where the operator's voice comes out.\n\
                 This is NOT a contract violation, because the contract was never signed; it is \
                 a hole in the aggregate, named here rather than closed. It closes when \
                 `{service}` links the crate and calls `register` with its own \
                 `attest_warnings!`, serving the `{shape}` shape its declaration already names. \
                 Nothing in cauldron has to change on that day: the row it writes will say \
                 `registered = true` and the ask will be made like any other producer's."
            )),
        }
    }
}

// ───────────────────────────────────────────────────────────── the asking

fn resolve(addr: &str) -> Result<SocketAddr> {
    addr.to_socket_addrs()
        .with_context(|| format!("resolving the seam address `{addr}`"))?
        .next()
        .with_context(|| format!("the seam address `{addr}` resolved to nothing"))
}

/// **ONE CONNECTION, ONE WORD, ONE LINE BACK.**
///
/// The whole of the supervisor's participation in a service's life. It writes
/// [`SupervisorAsk::Warnings`] and the shape name, reads one line, and closes.
/// There is no second request, no stream, and nothing that stays open — which
/// is why the supervisor can be a CLI and not a daemon.
pub fn ask(addr: &str, shape: &str) -> Result<Export> {
    let sa = resolve(addr)?;
    let stream = crate::egress::connect_tcp("warnings", &sa, ASK_TIMEOUT)
        .with_context(|| format!("connecting to {addr} to ask for its warnings shape"))?;
    let _ = stream.set_read_timeout(Some(ASK_TIMEOUT));
    let _ = stream.set_write_timeout(Some(ASK_TIMEOUT));
    let mut w = stream.try_clone()?;
    // The ONLY place `SupervisorAsk::Warnings` is written. See the module doc.
    let req = format!("{} {shape}\n", SupervisorAsk::Warnings.word());
    w.write_all(req.as_bytes()).and_then(|_| w.flush()).with_context(|| {
        format!("writing `{}` to {addr}", req.trim())
    })?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .with_context(|| format!("{addr} accepted a connection then said nothing"))?;
    let line = line.trim();
    if line.is_empty() {
        bail!("{addr} closed the connection without answering");
    }
    if !line.starts_with('{') {
        bail!("{addr} answered `{line}`");
    }
    serde_json::from_str(line).with_context(|| format!("parsing the warnings export from {addr}"))
}

/// **THE CHECK.** Given a seam, the verdict already reached about it, and the
/// shape its declaration named, decide whether the service is serving
/// warnings.
///
/// `declared` is `None` for a declaration that says `warnings = false`; that
/// case never reaches a socket, because there is no question to ask.
pub fn check(seam: &Seam, verdict: &Verdict, declared: Option<&str>) -> Compliance {
    let Some(shape) = declared else {
        return Compliance::RefusedByDeclaration { service: seam.service.clone() };
    };
    // Only a LIVE seam has a producer to ask. `stale:*` and `down` are the
    // seam contract's business, already reported by `status`, and asking a
    // stranger's socket for warnings would attribute them to this service.
    if !matches!(verdict.state, SeamState::Live | SeamState::LiveUnverified) {
        return Compliance::NoProducer { state: verdict.state.as_str().to_string() };
    }
    // ──────────────────── A HOSTED ROW, AND THE ONE DECISION THAT IS LEFT ────
    //
    // A process whose row the supervisor wrote did not call
    // `ServiceEnv::register`, so it never held an `Attestation`, so the
    // compile-time obligation never ran against it. It cannot now. The
    // question this branch answers is NOT "does it owe the contract" — it does
    // not — but "can the aggregate COVER it anyway?"
    //
    // **No.** A binary that does not link this crate does not parse the one
    // word the ask consists of, so there is nothing to ask and no answer to
    // credit. This used to be a branch with a knob in it: an adopted
    // declaration could say `warnings_askable = true` and the supervisor would
    // try. That knob is deleted, because it was a claim a TOML file made about
    // a binary it does not contain, and the version of this branch that
    // honoured it would write an unparseable frame onto a production socket on
    // the strength of a line somebody typed.
    //
    // The result is a NAMED GAP and never a violation. See
    // `Compliance::Unregistered`.
    if !seam.registered {
        return Compliance::Unregistered {
            service: seam.service.clone(),
            shape: shape.to_string(),
        };
    }
    match ask(&seam.addr, shape) {
        Err(e) => Compliance::NotServed { shape: shape.to_string(), detail: format!("{e:#}") },
        Ok(export) => {
            if export.shape != shape {
                return Compliance::WrongShape {
                    asked: shape.to_string(),
                    answered: export.shape,
                };
            }
            if export.producer != seam.service {
                return Compliance::WrongProducer {
                    expected: seam.service.clone(),
                    answered: export.producer,
                };
            }
            if export.counts_since.trim().is_empty() {
                return Compliance::Unparseable {
                    detail: "the export carries an empty `counts_since`, so a count that reset \
                             would be indistinguishable from a count that fell to zero"
                        .into(),
                };
            }
            Compliance::Served { export: Box::new(export) }
        }
    }
}

// ───────────────────────────────────────────────────────── the aggregate

/// One producer's line in the aggregate.
#[derive(Debug, Clone, Serialize)]
pub struct Producer {
    pub service: String,
    pub shape: String,
    pub warn: u64,
    pub error: u64,
    pub total: u64,
    pub counts_since: String,
    pub sink_errors: u64,
    pub sink_attached: bool,
}

/// A producer the aggregate could NOT include, and why. **Carried next to the
/// total rather than instead of it**, because a total whose coverage is unknown
/// is the log file again: a number that looks complete and is not.
#[derive(Debug, Clone, Serialize)]
pub struct Gap {
    pub service: String,
    /// `violation` (a running service that OWES the contract and will not
    /// serve warnings), `no-producer` (nothing running to ask), or `unregistered`
    /// (a running process cauldron did not spawn, which never signed the
    /// contract and cannot satisfy it retroactively).
    ///
    /// **Three kinds, not two, and the third is not a rounding of the other
    /// two.** Folding `unregistered` into `violation` would accuse a process of a
    /// breach it never committed and would make `status` exit 4. Folding it
    /// into `no-producer` would be worse: it would say there is nothing
    /// running to ask, when there is a live daemon on a known address, and the
    /// aggregate would under-report its own hole by describing it as an
    /// absence.
    pub kind: &'static str,
    pub reason: String,
}

/// **WHAT `cauldron status` REPORTS.** The sum, who it came from, and who is
/// missing from it.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Aggregate {
    pub warn: u64,
    pub error: u64,
    pub total: u64,
    /// Producers that answered.
    pub producers: Vec<Producer>,
    /// Producers that did not, with the reason.
    pub gaps: Vec<Gap>,
    /// The busiest call sites across every producer that answered, descending.
    pub top_kinds: Vec<AggregateKind>,
    /// Is any gap a CONTRACT FAILURE rather than an absent service?
    pub violations: usize,
    /// **How many services the total EXCLUDES because they were ADOPTED.**
    ///
    /// Carried separately from [`Aggregate::violations`] so the two holes
    /// cannot be confused, and carried AT ALL so the total can never be read
    /// as complete while one exists. [`Aggregate::is_complete`] is the single
    /// predicate, and it is `false` while this is non-zero.
    pub unregistered: usize,
}

/// A kind, carrying which producer it came from — because the same line number
/// in two services is two different defects.
#[derive(Debug, Clone, Serialize)]
pub struct AggregateKind {
    pub service: String,
    pub target: String,
    pub line: u32,
    pub level: String,
    pub count: u64,
    pub message: String,
}

impl Aggregate {
    /// Fold one service's verdict in. Every service is folded, including the
    /// ones that failed, so `producers.len() + gaps.len()` is the whole
    /// inventory and the aggregate can state its own coverage.
    pub fn add(&mut self, service: &str, shape: &str, c: &Compliance) {
        match c {
            Compliance::Served { export } => {
                self.warn += export.warn;
                self.error += export.error;
                self.total += export.total();
                for k in &export.kinds {
                    self.top_kinds.push(AggregateKind {
                        service: service.to_string(),
                        target: k.target.clone(),
                        line: k.line,
                        level: k.level.clone(),
                        count: k.count,
                        message: k.message.clone(),
                    });
                }
                self.producers.push(Producer {
                    service: service.to_string(),
                    shape: shape.to_string(),
                    warn: export.warn,
                    error: export.error,
                    total: export.total(),
                    counts_since: export.counts_since.clone(),
                    sink_errors: export.sink_errors,
                    sink_attached: export.sink_attached,
                });
            }
            other => {
                let violation = other.is_violation();
                let unregistered_gap = other.is_unregistered_gap();
                if violation {
                    self.violations += 1;
                }
                if unregistered_gap {
                    self.unregistered += 1;
                }
                self.gaps.push(Gap {
                    service: service.to_string(),
                    kind: if violation {
                        "violation"
                    } else if unregistered_gap {
                        "unregistered"
                    } else {
                        "no-producer"
                    },
                    reason: other
                        .missing()
                        .unwrap_or_else(|| match other {
                            Compliance::NoProducer { state } => {
                                format!("nothing is running to ask: the seam is {state}")
                            }
                            _ => "unknown".into(),
                        }),
                });
            }
        }
        // Deterministic: descending by count, ties broken by
        // `(service, target, line)` so two runs of `status` over the same
        // numbers print the same order.
        self.top_kinds.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.service.cmp(&b.service))
                .then_with(|| a.target.cmp(&b.target))
                .then_with(|| a.line.cmp(&b.line))
        });
    }

    /// How many of the services asked actually answered.
    ///
    /// **A hosted service that cannot be asked is in the DENOMINATOR and not
    /// in the numerator.** That is the whole mechanism by which adoption keeps
    /// the aggregate honest: `adopt` makes a process visible to `status`, and
    /// the price of that visibility is that the coverage fraction immediately
    /// says it is not covered. There is no spelling of adoption that quietly
    /// raises the numerator.
    pub fn coverage(&self) -> (usize, usize) {
        (self.producers.len(), self.producers.len() + self.gaps.len())
    }

    /// **Does the total above account for every declared service?**
    ///
    /// The one predicate for "is this number complete", so no caller can
    /// invent a laxer one. It is `false` for ANY gap — a violation, an absent
    /// producer, or a hosted process — because each of those is a service
    /// the total does not include, and a total that excludes something and
    /// does not say so is the 47 MB log again: a number that looks complete
    /// and is not.
    pub fn is_complete(&self) -> bool {
        self.gaps.is_empty()
    }

    /// **The sentence that says what the total leaves out.** `None` only when
    /// nothing is left out.
    ///
    /// Written here rather than in the CLI so that every face reporting this
    /// aggregate reports the same hole in the same words, and a new face
    /// cannot omit it by forgetting to look at `gaps`.
    pub fn incompleteness(&self) -> Option<String> {
        if self.is_complete() {
            return None;
        }
        let mut parts = Vec::new();
        if self.violations > 0 {
            parts.push(format!(
                "{} service(s) RUNNING AND REFUSING the contract (a violation: `status` exits 4)",
                self.violations
            ));
        }
        if self.unregistered > 0 {
            parts.push(format!(
                "{} HOSTED service(s) whose binary does not link cauldron's `env` crate, so \
                 the compile-time warnings obligation never ran against them and there is no \
                 word the supervisor could put on their sockets — not a violation, because the \
                 contract was never signed, but still a hole this total does not cover",
                self.unregistered
            ));
        }
        let absent = self.gaps.len() - self.violations - self.unregistered;
        if absent > 0 {
            parts.push(format!("{absent} service(s) with nothing running to ask"));
        }
        Some(format!(
            "the total above is INCOMPLETE: it excludes {}. A warning that reaches no consumer \
             is a warning nobody reads.",
            parts.join("; ")
        ))
    }
}
