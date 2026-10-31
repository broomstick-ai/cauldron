//! # Commands and promises — rule 2.4
//!
//! The rule numbers in this module are `docs/shapes.md`'s, and that document
//! lives in **substrate-interface-layer**, not in this tree; see the crate root
//! for the same pointer. Where a sentence below cites a substrate file or a
//! substrate producer it says so, because none of them has been migrated here
//! yet and a bare path would read as a path in cauldron.
//!
//! **A command returns an acknowledgement, never a fact.** The reply to
//! `accounts.disable` is `{promise_id}`; the new row arrives where every other
//! row arrives, through the shape. A reply that carried the row would be a
//! SECOND SOURCE for it — the exact thing substrate's S3 release deleted from
//! the read path, quietly kept on the write path — and while a second way to
//! learn a fact exists, every disagreement between the two is ambiguous.
//!
//! Three things, and only the first is a type problem:
//!
//! * **Which type resolves this command** is checked by the compiler. A handler
//!   is `fn(&self, C) -> Promise<C::Resolves>`, and [`Promise`] carries its
//!   type, so a handler that promised the wrong one does not build.
//! * **WHICH SUBJECT it is about** is not a type problem, because every account
//!   has the same type. `accounts.disable aa` and `accounts.disable bb` resolve
//!   on one `Fact::AccountDisabled`, so a book matching on type alone closes
//!   both on whichever fact emits first, and the ids ride that account's frame
//!   — which the other consumer, holding its own key, never receives. That is
//!   why [`PromiseBook::open`] takes a subject and [`PromiseBook::resolve`]
//!   returns its ids ROUTED under one: see [`crate::wire::Resolving`].
//! * **That the emit actually HAPPENS** is liveness, which no type can prove.
//!   That is what [`PromiseBook::open_ids`] is for: run every command through a
//!   producer, drain deterministically, assert the book is empty. A command
//!   whose effect is a no-op — disabling an already-disabled account — must
//!   still emit its resolving fact as a re-assertion, or the test cannot tell
//!   "resolved by re-assertion" from "never emitted". Levels absorb the
//!   re-assertion for free.
//!
//! # Rule 2.7 — one act, one event per moment it completes
//!
//! **The operator's ruling, 2026-09-26**, and the half of it that is the promise
//! machinery's:
//!
//! | stage | means | carried by | cost | default |
//! |---|---|---|---|---|
//! | `accepted` | understood, validated, the store write is done | the reply — [`Accepted`] | free | always |
//! | `visible` | the resolving fact folded and the frame carrying it emitted to the subject's subscribers | the frame's `promises` list | one field on a frame already going out | always, when a fold is tracking |
//! | `durable` | committed such that a crash cannot lose it | **a barrier a caller asks for** — [`BARRIER_METHOD`], answered by [`barrier`] | the expensive one | **never unless asked** |
//!
//! **`durable` IS STILL NOT A FIELD HERE, AND THERE IS A BARRIER.** The stage is
//! asked for by calling [`BARRIER_METHOD`] and it is answered by [`barrier`]; it
//! never appears on [`Accepted`], because a reply that carried it would make
//! every command pay for a guarantee the caller did not ask for — which is the
//! accident rule 2.7 was written against, not its implementation.
//!
//! A producer declares the stage by returning `Some` from its `durable()`
//! accessor; one that cannot promise durability returns `None` and the barrier
//! answers [`crate::envelope::codes::UNSUPPORTED`]. That is the REFUSAL, spelled
//! where a caller can read it, and it is the one thing this rule will not trade:
//! a silent downgrade leaves the consumer believing a guarantee it did not get.
//! (The `Producer` trait that carries that accessor lives in this crate's
//! `server` module, which is a LATER DISPATCH in this tree — `barrier` takes the
//! `Option<&dyn Durable>` directly and does not depend on it.)
//!
//! **`accepted` without `visible` is a real outcome and the reply says so.** A
//! producer's not-tracking early return closes a promise with no emit at all —
//! see [`PromiseBook::abandon_all`]. Before 2026-09-26 the reply on that path was
//! byte-identical to the one on the path where a frame WILL follow, so a writer
//! could not tell "it took" from "nobody will ever tell you", and the assumption
//! that made it safe — *"a consumer that is not subscribed cannot be waiting"* —
//! is false for the next consumer in line: substrate's AUI issues commands from a
//! process that is not yet subscribed to the shape they move. [`Accepted::visible`]
//! is that distinction, decided by the emit path itself rather than re-derived by
//! the handler.

use std::any::TypeId;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::wire::Resolving;

/// The acknowledgement a command replies with. Flat on the wire — the types
/// constrain the producer, the transport stays simple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PromiseId(pub u64);

impl std::fmt::Display for PromiseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// **WILL A FRAME EVER CARRY THIS PROMISE ID?** — rule 2.7's `visible` stage,
/// as the emit path answers it.
///
/// It is returned BY the emit and never re-derived by the handler. A handler that
/// asked `any_tracking()` for itself would be reading the same predicate a second
/// time, at a second instant, and the two reads can disagree — which is the
/// disease this rule exists to treat, one layer down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visible {
    /// A fold is tracking. The resolving fact went out, and the frame carrying
    /// it will reach the subject's subscribers.
    Expected,
    /// **Nothing is tracking, so no frame will ever carry this id.** The store
    /// write happened — the command TOOK — and there is no chassis left to say
    /// so. A writer told this never waits on a frame that is not coming.
    Never,
}

impl Visible {
    pub fn is_expected(self) -> bool {
        matches!(self, Visible::Expected)
    }
}

/// **THE REPLY TO A COMMAND — rule 2.7's `accepted`, and what it says about the
/// stages after it.**
///
/// Flat on the wire, as 2.4 requires: `{"promise_id": 7, "visible": true}`. Three
/// distinct things, none of which is the fact itself:
///
/// * `promise_id` — the handle. `accepted` is free and always.
/// * `visible` — whether a frame carrying that id will ever exist. See
///   [`Visible`]. **This is the field that closes the one case where a consumer
///   holding a promise id could never learn anything**: not from the fold, not
///   from a timeout, not from a re-subscribe.
/// * `changed` — did this call MOVE anything, or was it a re-assertion of
///   something already true. A fact about the CALL, which is why it rides the
///   reply and not the frame (the same reasoning that kept `observations` and
///   `failures` on `gauge.probe`'s reply: nothing else reports it and no shape
///   carries it). `None`, and absent from the wire, when the producer cannot
///   answer it — never `false` as a stand-in for "did not look", because that is
///   the collapse again.
///
/// **`durable` IS DELIBERATELY NOT A FIELD HERE, AND THAT DID NOT CHANGE WHEN
/// THE BARRIER LANDED.** The stage is opt-in: it is asked for by calling
/// [`BARRIER_METHOD`] and answered by [`barrier`], on its own reply. Putting it
/// here would mean every command reply had to wait for an fsync to be able to
/// fill it in — a stage imposed rather than asked for, which is the half of rule
/// 2.7 that is not about naming the stages at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    pub promise_id: PromiseId,
    /// `true` = a frame carrying `promise_id` will reach the subject's
    /// subscribers. `false` = accepted, applied, and nothing will ever carry it.
    pub visible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed: Option<bool>,
}

impl Accepted {
    /// The reply, with `changed` unanswered.
    pub fn new(promise_id: PromiseId, visible: Visible) -> Self {
        Self {
            promise_id,
            visible: visible.is_expected(),
            changed: None,
        }
    }

    /// **DID THIS CALL MOVE ANYTHING.** Pass what the producer actually read;
    /// omit the call entirely when it did not read.
    pub fn changed(mut self, changed: bool) -> Self {
        self.changed = Some(changed);
        self
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("an Accepted is three scalars")
    }
}

/// **WHO EMITS THE FACT THAT CLOSES A PROMISE.** Implemented by a shape, once per
/// fact type its fold emits with a resolve.
///
/// This is the FastAPI lens applied to a command: in FastAPI there is no way for a
/// request to return nothing — a 500 is still a response — so the route's
/// declaration is where the guarantee lives. Here, a command's `Resolves` type is
/// the fact whose emit closes its promise, and [`Command::ResolvedBy`] is the
/// shape that emits it. A command whose resolving type nothing on this producer
/// emits used to be a RUNTIME DISCOVERY: the reply said `visible: false` and the
/// handler remembered to abandon its own promise. It is now a DECLARATION, in the
/// command's own definition, and the alternative is spelled [`NeverVisible`]
/// rather than left as an omission.
///
/// **WHAT THIS DOES NOT PROVE, named so the gap is not mistaken for coverage.**
/// It is an unconstrained marker: implementing it says the shape CLAIMS to emit
/// the fact. It cannot prove the fold reaches the emit, on that path, with
/// tracking on, about the right subject. That is irreducibly a test — substrate's
/// chassis wrote its liveness property three times before it could fail, twice
/// passing with the emit deleted — and the repair there was to make the resolve an
/// ARGUMENT of the emit and to assert the id arrived on a frame. A type cannot
/// see whether a call site is reached. This marker makes the PATH's existence
/// static; nothing can make the traversal static.
#[diagnostic::on_unimplemented(
    message = "no shape on this producer resolves this command's fact, so its promise can never become visible",
    label = "nothing here emits it",
    note = "A command's `Resolves` type is the fact whose emit closes its promise, and \
            `Command::ResolvedBy` is the shape whose fold emits that fact. Nothing \
            implements `Resolves` for it, so every promise this command opens would reach \
            `accepted` and never `visible` — and the reply would be a lie unless the \
            handler remembered to abandon it.",
    note = "WHAT IS MISSING: either `impl shapewire::command::Resolves<TheFact> for TheShape` \
            on the shape whose fold emits it, or — if nothing on this producer carries that \
            fact — `type Resolves = shapewire::command::NeverVisible;` and `type ResolvedBy \
            = shapewire::command::NeverVisible;` on this command, which declares that the \
            reply always says `visible: false` and makes it a fact a reader finds in the \
            definition instead of a branch in a handler."
)]
pub trait Resolves<E> {}

/// **A COMMAND NO FACT RESOLVES — THE EXPLICIT ALTERNATIVE, AND IT IS A
/// DECLARATION.**
///
/// Uninhabited on purpose: a `Promise<NeverVisible>` waits for a value that
/// cannot be constructed, which is the type-level statement of *nothing will ever
/// carry this id*. The command still gets a `promise_id` — a reply with no id at
/// all would be indistinguishable from a method that does not participate in the
/// contract — and the reply still says [`Visible::Never`], which is a real answer
/// a writer can act on.
///
/// Two commands on substrate's live `threads` producer are this case, and both
/// were discovered by building rather than stated: `budgets/{key}/set`, because
/// no shape on that producer carries a budget at all; and `gauge/probe`, whose
/// promise is SUBJECTLESS and which therefore needs a shape-level fact to close
/// it, which does not exist. Note the second one carefully: it is a SUBJECT
/// mismatch, not a type mismatch — `GaugeObservedFact` is emitted, per account,
/// per axis — so this marker catches it only because the command declares the
/// honest answer, not because the type system found it. The subject dimension is
/// not encoded here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeverVisible {}

/// The one witness [`NeverVisible`] needs, so that *nothing resolves this* is a
/// thing a command can SAY rather than a bound it fails.
impl Resolves<NeverVisible> for NeverVisible {}

/// A typed command. `Resolves` names the event whose arrival means "it took".
pub trait Command {
    const NAME: &'static str;
    /// The producer event that resolves this command. `'static` so the book can
    /// match it by [`TypeId`] at emit.
    type Resolves: 'static;
    /// **THE SHAPE WHOSE FOLD EMITS IT** — or [`NeverVisible`] when nothing on
    /// this producer does.
    ///
    /// REQUIRED, with no default, because no default is true: defaulting it to
    /// anything would publish a claim nobody made, which is the hole this
    /// declaration exists to close. A command that declares neither does not
    /// compile, and rustc names the thing that is missing:
    ///
    /// ```text
    /// error[E0046]: not all trait items implemented, missing: `ResolvedBy`
    /// ```
    type ResolvedBy: Resolves<Self::Resolves>;
}

/// **An open promise, carrying the type that will close it.**
///
/// There is no public constructor. The only way to obtain one is
/// [`PromiseBook::open`], which infers `E` from the command — so a handler
/// cannot hand back a promise for a type it does not intend to emit.
#[derive(Debug)]
pub struct Promise<E> {
    id: PromiseId,
    _m: PhantomData<fn() -> E>,
}

impl<E> Promise<E> {
    /// The id to put on the wire. Consumes nothing: the promise stays open in
    /// the book until an emit of `E` resolves it.
    pub fn id(&self) -> PromiseId {
        self.id
    }

    /// **THE REPLY.** `visible` is the EMIT PATH's own answer, handed back
    /// through it — never a second reading of `any_tracking()` by the handler.
    pub fn accepted(&self, visible: Visible) -> Accepted {
        Accepted::new(self.id, visible)
    }
}

/// One open promise: what closes it, what command opened it, and **what it is
/// about**.
#[derive(Debug, Clone)]
struct Open {
    /// The `TypeId` of the event whose emit closes this.
    resolves: TypeId,
    /// The command's name, for diagnostics.
    command: &'static str,
    /// **The subject.** `Some(key)` = this promise is about one key of the
    /// shape, and only a fact about THAT key closes it. `None` = the command was
    /// about the shape and not about any one key (`gauge.probe`: "poll the pool
    /// now").
    subject: Option<String>,
}

/// **The open promises, and the only thing that closes one.**
///
/// One per producer, beside the hub. `resolve::<E>` is called from the emit
/// path with the type that was actually emitted AND the subject it was about,
/// so the match is the emit itself rather than a promise the producer makes
/// about it.
///
/// # A promise is about a SUBJECT, not only about a type
///
/// Matching on type alone was the defect this book shipped with. Two consumers
/// disable two different accounts; both promises resolve on `AccountDisabled`;
/// whichever account's fact emits first closes BOTH, and the ids ride that
/// account's frame — which the other consumer, subscribed to its own key, never
/// receives. Each writer closed the other's promise and neither learned its own
/// command took. The subject is what makes "the fact that proves MY command"
/// expressible.
#[derive(Debug, Default)]
pub struct PromiseBook {
    next: AtomicU64,
    /// id -> the open promise
    open: Mutex<HashMap<u64, Open>>,
}

impl PromiseBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a promise for a command. The returned [`Promise`]'s type is the
    /// command's `Resolves`, not a choice the caller makes.
    ///
    /// `subject` is what the command is ABOUT — the account name, the key. Pass
    /// `None` only when the command genuinely names no key, which is a
    /// shape-level command; a `None` passed out of convenience is a promise that
    /// any fact of its type can close, which is the defect this argument exists
    /// to delete.
    pub fn open<C: Command>(&self, subject: Option<String>) -> Promise<C::Resolves> {
        let id = PromiseId(self.next.fetch_add(1, Ordering::Relaxed) + 1);
        self.open.lock().unwrap().insert(
            id.0,
            Open {
                resolves: TypeId::of::<C::Resolves>(),
                command: C::NAME,
                subject,
            },
        );
        Promise {
            id,
            _m: PhantomData,
        }
    }

    /// **Close every promise waiting on an emit of `E` ABOUT THIS SUBJECT,
    /// newest last.**
    ///
    /// Returns the ids ROUTED under the subject they were about, which the
    /// caller hands to the emit so they ride the frame for that subject's key —
    /// see [`Resolving`]. A coalesced `row` may resolve several promises for one
    /// subject (two consumers disabling the same account, a re-assertion), which
    /// is why the wire field is `promises: [..]` and not a scalar.
    ///
    /// **A SUBJECT ANSWERS ITS OWN SUBJECT, IN BOTH DIRECTIONS.** Exactly:
    ///
    /// * `Some(key)` closes promises about `key` **and nothing else**.
    /// * `None` closes ONLY subjectless promises, and routes their ids keyless.
    ///
    /// # The over-broad direction, deleted 2026-09-26
    ///
    /// `(Some(_), None)` used to be `true`: a keyed fact closed every subjectless
    /// promise of its type. The subject fix made KEYED promises exact and left the
    /// subjectless case exactly as wrong as it had always been, in the other
    /// direction. What that cost, concretely: `gauge.probe` says "poll the pool
    /// now" and its sweep emits one `GaugeObserved` per account per axis, so the
    /// promise for "my sweep completed" closed on the FIRST observation of the
    /// first account — and not necessarily one of its own, because the background
    /// poller emits through the same path and races it. *"My sweep completed"* and
    /// *"one observation of one account arrived"* are two events, and one of them
    /// was answering for the other.
    ///
    /// **The consequence, stated rather than hidden:** a shape-level command now
    /// needs a shape-level fact to close it. A producer with no such fact has a
    /// command that reaches `accepted` and never `visible`, and it must say so on
    /// the reply ([`Accepted::visible`]) and close its own promise on the way out
    /// ([`PromiseBook::abandon`]) rather than leave it open forever.
    pub fn resolve<E: 'static>(&self, subject: Option<&str>) -> Resolving {
        let hit = self.take_matching::<E>(subject);
        match subject {
            Some(key) => Resolving::keyed(key, hit),
            None => Resolving::shape_level(hit),
        }
    }

    /// **RULE 2.7's `accepted` WITHOUT `visible`: THE PRODUCER FOUND NOTHING THAT
    /// COULD CARRY THESE IDS.**
    ///
    /// The not-tracking early return every resolving producer has. Its reasoning
    /// is right for the BOOK — nothing is folding, so no frame will ever carry an
    /// id — and it used to call [`Self::resolve`] and DROP the routing, which is a
    /// promise closing in silence: the reply the handler then built was
    /// byte-identical to the one it builds when a frame is coming. A consumer
    /// could not tell *"it took"* from *"nobody will ever tell you"*, and the
    /// assumption that made that safe — *"a consumer that is not subscribed cannot
    /// be waiting"* — is false for the writer that subscribes AFTER issuing its
    /// command, which is what substrate's AUI migration does.
    ///
    /// So it is its own act with its own name, it returns what it closed, and the
    /// handler turns that into [`Visible::Never`] on the reply. It must never be
    /// used where an emit happened: the ids would be closed and never carried.
    pub fn abandon_all<E: 'static>(&self, subject: Option<&str>) -> Vec<PromiseId> {
        self.take_matching::<E>(subject)
            .into_iter()
            .map(PromiseId)
            .collect()
    }

    /// **CLOSE ONE PROMISE BY ID.** Two callers, and what differs is the reply,
    /// not what the book does:
    ///
    /// * a handler that **FAILED after opening** one — `gauge.probe`'s `Err` arm.
    ///   The reply is a refusal, the id never reaches anybody, and *a promise
    ///   never outlives the reply that refused it*. Before this existed that arm
    ///   left an id open forever, and [`Self::open_ids`] — the liveness test's own
    ///   oracle — reported a leak no test triggered.
    /// * a handler whose producer has **no fact that could ever carry it**, which
    ///   is `accepted` without `visible` for a shape-level command on a producer
    ///   with no shape-level fact. The reply says `visible: false`.
    ///
    /// By id rather than by (type, subject) so it touches one promise and never a
    /// concurrent caller's.
    pub fn abandon(&self, id: PromiseId) -> bool {
        self.open.lock().unwrap().remove(&id.0).is_some()
    }

    fn take_matching<E: 'static>(&self, subject: Option<&str>) -> Vec<u64> {
        let want = TypeId::of::<E>();
        let mut open = self.open.lock().unwrap();
        let mut hit: Vec<u64> = open
            .iter()
            .filter(|(_, o)| o.resolves == want)
            .filter(|(_, o)| match (subject, o.subject.as_deref()) {
                // A fact about this key answers a command about this key.
                (Some(s), Some(mine)) => s == mine,
                // A KEYED fact does not answer a command about the SHAPE — see
                // this method's caller for what that cost.
                (Some(_), None) => false,
                // A shape-level fact answers only shape-level commands.
                (None, None) => true,
                (None, Some(_)) => false,
            })
            .map(|(id, _)| *id)
            .collect();
        hit.sort_unstable();
        for id in &hit {
            open.remove(id);
        }
        hit
    }

    /// Every promise still open, as `(id, command name, subject)`. **The
    /// property test's assertion**: drain a producer deterministically and this
    /// must be empty. The subject is on it because "one promise is open" and
    /// "one promise about `bb` is open" are different findings.
    pub fn open_ids(&self) -> Vec<(PromiseId, &'static str, Option<String>)> {
        let open = self.open.lock().unwrap();
        let mut v: Vec<_> = open
            .iter()
            .map(|(id, o)| (PromiseId(*id), o.command, o.subject.clone()))
            .collect();
        v.sort_unstable_by_key(|(id, _, _)| *id);
        v
    }

    pub fn is_empty(&self) -> bool {
        self.open.lock().unwrap().is_empty()
    }

    /// **THE HIGHEST PROMISE ID THIS BOOK HAS EVER MINTED.**
    ///
    /// Ids are `1..=issued()`, from one counter, never reused — so this is the
    /// only question the book can still answer about a promise it has already
    /// resolved and forgotten: *did this producer ever issue that id at all?*
    /// [`barrier`] asks it, and that is the whole reason it exists: an id above
    /// this watermark was minted by SOMETHING ELSE — another producer, another
    /// boot, a consumer's own arithmetic — and answering `durable` for it would
    /// be the chassis agreeing with a caller's mistake.
    ///
    /// `PromiseId(0)` is not an id. The counter is `fetch_add + 1`, so the first
    /// id is 1 and a fresh book answers `PromiseId(0)` here, meaning *nothing
    /// has been issued*.
    pub fn issued(&self) -> PromiseId {
        PromiseId(self.next.load(Ordering::Relaxed))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Rule 2.7's THIRD stage — `durable`, and the barrier that is the only way to
// ask for it
// ─────────────────────────────────────────────────────────────────────────────

/// **THE ONE METHOD THAT ANSWERS AFTER COMMIT.** A chassis method, not a
/// producer's, for the reason `shapes/list` is one: the RULE is the chassis's
/// and only the fsync is the producer's.
///
/// `durable` is the family and `barrier` the act, which is the same shape every
/// other method on this wire has. There is deliberately no per-command flag
/// (`{…, durable: true}`) beside it: a flag is cheaper for exactly one write and
/// is the wrong primitive for the thing the operator's ruling is about — *"fsync
/// doesn't need to happen on all writes"* — because N flagged writes are N
/// fsyncs, while N writes and one barrier are one. The barrier is the amortising
/// form, so it is the only form.
pub const BARRIER_METHOD: &str = "durable/barrier";

/// **RULE 2.8's CEILING ON `promises`** — the default a producer gets, and the
/// number a refusal names.
///
/// It bounds the ids ONE call may name, which is the only knob the barrier has.
/// A barrier costs one commit however many ids it names, so this is not a cost
/// bound: it is a bound on what the chassis will verify and echo in one reply,
/// and it exists because `promises` is a caller-supplied list and an unbounded
/// one is a memory fault wearing a parameter's clothes (the same reason
/// [`crate::envelope::codes::TOO_LARGE`] exists for the upload door, which the
/// `resource` module serves and which is a later dispatch in this tree).
///
/// 1024 because a caller that has 1024 unacknowledged writes outstanding has a
/// batching problem the barrier cannot fix, and because the natural use —
/// *issue N writes, then one barrier* — is a handful of ids, not a thousand.
pub const MAX_PROMISES_PER_BARRIER: usize = 1024;

/// **WHAT A PRODUCER DECLARES WHEN IT CAN PROMISE DURABILITY — AND WHAT IT
/// DECLARES BY NOT IMPLEMENTING THIS AT ALL.**
///
/// A producer's `durable()` accessor returns `Option<&dyn Durable>`. `None` is
/// the honest refusal and it is the DEFAULT: a producer whose store cannot
/// promise survival — an in-memory database, a store running `synchronous=OFF`,
/// a producer that has not thought about it — says nothing and the barrier
/// answers [`crate::envelope::codes::UNSUPPORTED`]. **It never downgrades to
/// `accepted`.** That is the disease this rule exists to treat: a consumer that
/// asked for durability, got a 200, and believes a guarantee nobody gave it.
///
/// Three methods, and the split between them is the point — the chassis owns the
/// RULE and the producer owns only the fsync:
///
/// * [`Self::max_promises`] is rule 2.8's ceiling, declared by the producer and
///   enforced by [`barrier`].
/// * [`Self::issued`] is [`PromiseBook::issued`], so the chassis can tell an id
///   this producer minted from an id it did not, without holding the book.
/// * [`Self::commit`] is the only thing the producer actually does.
pub trait Durable: Send + Sync {
    /// Rule 2.8: the most ids one barrier call may name. Refused naming this
    /// number, **never clamped** — see [`barrier`] for why clamping is worse
    /// here than anywhere else the rule applies.
    fn max_promises(&self) -> usize {
        MAX_PROMISES_PER_BARRIER
    }

    /// The highest promise id this producer has ever minted —
    /// [`PromiseBook::issued`].
    fn issued(&self) -> PromiseId;

    /// **COMMIT SUCH THAT A CRASH CANNOT LOSE ANYTHING ALREADY `accepted`.**
    ///
    /// It must not return until that is true. A producer that cannot make it
    /// true for this call REFUSES — it never returns `Ok` and hopes, and it
    /// never answers late.
    ///
    /// It takes no promise ids, and that is deliberate. `accepted` means *the
    /// store write is done* (see [`Accepted`]), and the ordering that makes that
    /// true is **reply-after-write, not mint-after-write**: substrate's `threads`
    /// handlers `open` the promise FIRST and write SECOND
    /// (`lib/threads/src/server.rs`'s `ThreadsUpdate`, `ThreadsEvent`,
    /// `ThreadsSeen`, `PoolShareSet`, `AccountsDisable`, `BudgetsSet` all read
    /// `let promise = …open::<_>(…)` above their store call), and the reply is
    /// built out of the store call's own result. So a caller can only be HOLDING
    /// an id whose write has returned, which is exactly the precondition a
    /// store-wide commit needs. (substrate-interface-layer's
    /// `docs/chassis-writes.md` §7.7 says *"the handler ran `store.write` before
    /// `open`"*; that is not what the code does, and the property it was reaching
    /// for survives the correction.)
    ///
    /// A store-wide commit therefore covers every id a caller can name, and a
    /// per-id commit would be N fsyncs where the rule asks for one. The ids are
    /// the CALLER's statement of what it is asking about, which is what
    /// [`barrier`] checks and echoes.
    fn commit(&self) -> Result<(), crate::envelope::Refusal>;
}

/// **THE BARRIER CONTRACT, AS IT RIDES `shapes.list`** — rule 2.8's last clause,
/// *"so a consumer can read the contract before it asks."*
///
/// It is a sibling of `shapes` on that payload and not a field inside any one
/// shape, because durability is a property of the producer's STORE and no shape
/// has its own. Absent means this producer promises no durability and the
/// barrier refuses; a consumer that reads the absence never has to probe for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableContract {
    /// [`Durable::max_promises`].
    pub max_promises: usize,
}

impl DurableContract {
    pub fn of(d: &dyn Durable) -> Self {
        Self {
            max_promises: d.max_promises(),
        }
    }
}

/// **ONE BARRIER CALL: PARSED, CHECKED, COMMITTED, ANSWERED.**
///
/// The whole of the rule is here rather than in the server's match, so it is
/// testable without a socket and so there is one place where the order of the
/// checks is decided. The order is load-bearing: **every refusal that can be
/// answered for free is answered before [`Durable::commit`] is called**, because
/// a caller that asked wrongly must not be charged an fsync to find out.
///
/// # What it answers
///
/// `{"stage": "durable", "promises": [3, 4, 5]}` — the milestone, and exactly
/// the ids the caller named, as a sorted set.
///
/// **It carries no store internals, and that is the operator's ruling applied:**
/// *"ack should just inform the general milestones, not protocol-specific
/// details. User should not care."* So no `synchronous` level, no WAL frame
/// counts, no fsync timing, no "did this call actually have anything to do" —
/// all of which the producer knows and none of which a caller can act on. The
/// stage is the answer.
///
/// # Why it echoes the ids instead of a watermark
///
/// The obvious reply is `{"durable_through": 5}`, and substrate-interface-layer's
/// `docs/rules-audit.md` §4 drafted exactly that. It is not answerable by the
/// chassis, and on substrate's `threads` producer it would be **false** rather
/// than merely unverifiable. A watermark claims that ids `1..=5` are durable,
/// which needs id order to equal write order — and those handlers mint the id
/// BEFORE they write (see [`Durable::commit`]). So at the instant a barrier runs,
/// an id below the watermark can belong to a concurrent handler that has not
/// reached its store call yet. The echo claims nothing the caller did not already
/// state — and a caller can only hold an id whose own write has returned — which
/// is why the honest answer is per id.
///
/// It is also what keeps a barrier over several promises honest about SUBJECTS:
/// a bag of ids answered as one number is the same collapse
/// [`PromiseBook::resolve`] had when a keyed fact closed every subjectless
/// promise of its type. Per id, or not at all.
///
/// # Why the ceiling refuses rather than clamps
///
/// Everywhere else in rule 2.8 a clamp is a correction the consumer cannot see.
/// Here it is worse: a clamped barrier would answer `stage: durable` for a
/// PREFIX of the ids it was asked about, and the caller — holding a reply that
/// says `durable` — would have no way to tell which of its writes the fsync
/// actually covered. A refusal naming the ceiling costs a round trip. A clamp
/// costs the guarantee.
pub fn barrier(
    producer: Option<&dyn Durable>,
    params: &serde_json::Value,
) -> Result<serde_json::Value, crate::envelope::Refusal> {
    use crate::envelope::{codes, Refusal};

    // ── 1. IS THERE A BARRIER AT ALL ────────────────────────────────────────
    //
    // `unsupported` and NOT `unknown_method`: the method exists on every
    // chassis precisely so this answer is reachable. The same reasoning the
    // resource door is mounted under — a producer with no resources must be
    // distinguishable from a producer that has LOST one — applies with more
    // force here, because the thing being distinguished is a DURABILITY
    // guarantee and the alternative to saying so is saying nothing.
    let Some(producer) = producer else {
        return Err(Refusal::new(
            codes::UNSUPPORTED,
            "this producer promises no durability, so there is nothing to \
             commit and no barrier to answer. It is refusing rather than \
             downgrading: a reply of `accepted` here would leave you believing \
             a guarantee you did not get",
        ));
    };

    // ── 2. WHAT ARE YOU ASKING ABOUT ────────────────────────────────────────
    //
    // **REQUIRED, with no "everything" form**, and that is a deliberate
    // narrowing of substrate-interface-layer's `docs/rules-audit.md` §4
    // `barrier {promise_ids?}`. An omitted list would have to mean "every write
    // you have accepted", which the chassis can only answer as a watermark over
    // ids — see this function's doc for why it cannot. A caller that wants
    // everything names everything it holds.
    let Some(asked) = params.get("promises") else {
        return Err(Refusal::malformed(
            "`promises` is required: name the promise ids you are asking about, \
             as an array. There is no form that means `everything` — the ids are \
             what makes the answer about YOUR writes rather than about whatever \
             else this producer happened to be doing",
        ));
    };
    let Some(asked) = asked.as_array() else {
        return Err(Refusal::malformed(
            "`promises` must be an array of promise ids (non-negative integers)",
        ));
    };
    if asked.is_empty() {
        return Err(Refusal::malformed(
            "`promises` is empty. A barrier over nothing is not a cheap barrier, \
             it is a question with no subject — and answering `stage: durable` to \
             it would be a guarantee about no writes at all",
        ));
    }

    // ── 3. RULE 2.8 — REFUSED NAMING THE CEILING, NEVER CLAMPED ─────────────
    let max = producer.max_promises();
    if asked.len() > max {
        return Err(Refusal::malformed(format!(
            "`promises` names {} ids; this producer's barrier takes at most {max} \
             in one call. Ask for {max} or fewer and call it again. It is not \
             clamped: a barrier that committed a prefix and still answered \
             `durable` would leave you unable to tell which of your writes it \
             covered",
            asked.len()
        )));
    }

    let mut ids: Vec<PromiseId> = Vec::with_capacity(asked.len());
    for v in asked {
        let Some(n) = v.as_u64() else {
            return Err(Refusal::malformed(format!(
                "`promises` must be an array of promise ids (non-negative \
                 integers); got {v}"
            )));
        };
        ids.push(PromiseId(n));
    }

    // ── 4. DID THIS PRODUCER EVER ISSUE THESE ───────────────────────────────
    //
    // `unknown` — *"no such thing was ever known to this producer"* — and it is
    // checked BEFORE the commit so a caller that is confusing two producers, two
    // boots, or its own arithmetic is told so for free. A promise that has
    // already RESOLVED is not unknown: the book forgets a resolved promise, so
    // the only answerable question is whether the id was ever minted here, and
    // that is the right question anyway. `accepted` already means the store
    // write is done, so an id this producer issued names a write a store-wide
    // commit covers, resolved or not.
    let issued = producer.issued();
    for id in &ids {
        if id.0 == 0 || id.0 > issued.0 {
            return Err(Refusal::new(
                codes::UNKNOWN,
                format!(
                    "promise {id} was never issued by this producer (it has issued \
                     {issued}). Nothing was committed: a barrier that answered \
                     `durable` for an id it does not recognise would be agreeing \
                     with whatever mistake produced that id"
                ),
            ));
        }
    }

    // ── 5. THE ONLY EXPENSIVE LINE, AND THE LAST ONE ────────────────────────
    producer.commit()?;

    ids.sort_unstable();
    ids.dedup();
    Ok(serde_json::json!({ "stage": "durable", "promises": ids }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct AccountDisabled;
    #[derive(Debug, PartialEq)]
    struct GaugeSampled;

    /// The stand-in for a SHAPE: the thing that claims to emit the facts above.
    /// One type claiming both is the common case — substrate's live `accounts`
    /// shape emits `AccountDisabled` and `GaugeObserved` both.
    struct AnAccountsShape;
    impl Resolves<AccountDisabled> for AnAccountsShape {}
    impl Resolves<GaugeSampled> for AnAccountsShape {}

    struct Disable;
    impl Command for Disable {
        const NAME: &'static str = "accounts/{account}/disable";
        type Resolves = AccountDisabled;
        type ResolvedBy = AnAccountsShape;
    }
    struct Probe;
    impl Command for Probe {
        const NAME: &'static str = "gauge/probe";
        type Resolves = GaugeSampled;
        type ResolvedBy = AnAccountsShape;
    }

    /// **AND THE DECLARED ALTERNATIVE.** A command whose fact nothing on this
    /// producer emits says so, and [`NeverVisible`] is uninhabited — so the promise
    /// it opens waits for a value that cannot be constructed. `budgets/{key}/set`
    /// is this case on substrate's live producer.
    struct NothingCarriesThis;
    impl Command for NothingCarriesThis {
        const NAME: &'static str = "budgets/{key}/set";
        type Resolves = NeverVisible;
        type ResolvedBy = NeverVisible;
    }

    fn about(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn an_emit_of_the_wrong_type_resolves_nothing() {
        let book = PromiseBook::new();
        let p = book.open::<Disable>(about("aa"));
        // The gauge emitted. The disable promise is NOT closed by it.
        assert!(book.resolve::<GaugeSampled>(Some("aa")).is_empty());
        assert_eq!(
            book.open_ids(),
            vec![(p.id(), "accounts/{account}/disable", about("aa"))]
        );
        // The right type, about the right account, closes it.
        assert_eq!(
            book.resolve::<AccountDisabled>(Some("aa")).ids(),
            vec![p.id().0]
        );
        assert!(book.is_empty());
    }

    /// **The same subject, several promises — THAT is coalescing.**
    ///
    /// # This test previously blessed the defect
    ///
    /// In substrate it was written as
    /// `a_coalesced_emit_resolves_every_promise_waiting_on_it` and it opened TWO
    /// promises with no subject at all, then asserted that one emit closed both.
    /// Nothing in it distinguished "two commands about the same account" from
    /// "two commands about two different accounts", so it asserted the type-only
    /// match as correct and made the real defect — one account's fact closing
    /// another account's promise, with the ids riding a frame the second consumer
    /// never receives — look like a feature. Two different things were conflated
    /// under one word.
    ///
    /// Coalescing is the legitimate half, and it is what this asserts now: two
    /// consumers disable the SAME account (or one disables it twice), one
    /// `AccountDisabled` about that account arrives, and it is the answer to both
    /// — a re-assertion resolves a promise exactly as a change does. The
    /// illegitimate half is
    /// `a_fact_about_one_subject_leaves_another_subjects_promise_open`.
    #[test]
    fn one_fact_resolves_every_promise_about_that_same_subject() {
        let book = PromiseBook::new();
        let a = book.open::<Disable>(about("aa"));
        let b = book.open::<Disable>(about("aa"));
        let ids = book.resolve::<AccountDisabled>(Some("aa"));
        assert_eq!(ids.ids(), vec![a.id().0, b.id().0], "both, in open order");
        assert_eq!(
            ids.for_key("aa"),
            vec![a.id().0, b.id().0],
            "and routed to the frame for the subject they are about"
        );
        assert!(book.is_empty());
    }

    /// **THE DEFECT, ASSERTED AWAY.** A fact about one account is not an answer
    /// to a command about another, however much the two share a type.
    ///
    /// The composed failure this prevents: `bb`'s promise closed by `aa`'s fact,
    /// with `bb`'s id put on `aa`'s row frame — which the hub's `drive` delivers
    /// only to connections holding `aa` (the `hub` module is a later dispatch in
    /// this tree; the routing it consumes is [`Resolving`], which is here).
    /// The consumer watching `bb` waits forever, and the book has already
    /// forgotten it was owed anything. With two writers on two keys, each closed
    /// the other's promise.
    #[test]
    fn a_fact_about_one_subject_leaves_another_subjects_promise_open() {
        let book = PromiseBook::new();
        let aa = book.open::<Disable>(about("aa"));
        let bb = book.open::<Disable>(about("bb"));

        let ids = book.resolve::<AccountDisabled>(Some("aa"));
        assert_eq!(ids.ids(), vec![aa.id().0], "only `aa`'s");
        assert_eq!(
            book.open_ids(),
            vec![(bb.id(), "accounts/{account}/disable", about("bb"))],
            "`bb` is still owed its fact, and the book still says so"
        );

        // …and `bb`'s own fact closes `bb`'s promise, on `bb`'s key.
        let ids = book.resolve::<AccountDisabled>(Some("bb"));
        assert_eq!(ids.for_key("bb"), vec![bb.id().0]);
        assert!(ids.for_key("aa").is_empty());
        assert!(book.is_empty());
    }

    /// **A SUBJECT IS A SUBJECT IN BOTH DIRECTIONS — `None` IS NOT A WILDCARD
    /// EITHER WAY.**
    ///
    /// # What this test used to assert, and why it was wrong
    ///
    /// In substrate it was `a_subjectless_promise_is_about_the_shape_and_a_keyless
    /// _fact_is_not_a_wildcard`, and it asserted that a keyed emit closes a
    /// SUBJECTLESS promise — *"any observation is its answer; there is no subject
    /// to confuse"*. There is. `gauge.probe`'s promise means *"the sweep I asked
    /// for completed"*, and the sweep emits one observation per account per axis,
    /// so the FIRST of them closed it — and the background poller emits down the
    /// same path and races the probe, so the observation that closed it need not
    /// have been part of the sweep at all. Two events, one answer: *"my sweep
    /// completed"* and *"one observation of one account arrived"*.
    ///
    /// So the asymmetry is gone and the rule is symmetric: a fact answers a
    /// command about the same subject, and `None` is a subject.
    #[test]
    fn a_keyed_fact_does_not_answer_a_shape_level_command() {
        let book = PromiseBook::new();
        let probe = book.open::<Probe>(None);
        let keyed = book.open::<Probe>(about("aa"));

        // A KEYED emit answers the keyed command and leaves the shape-level one
        // open — this is the direction that changed.
        let ids = book.resolve::<GaugeSampled>(Some("aa"));
        assert_eq!(ids.for_key("aa"), vec![keyed.id().0]);
        assert_eq!(
            book.open_ids(),
            vec![(probe.id(), "gauge/probe", None)],
            "one account's observation is not `the sweep completed`"
        );

        // And a keyless emit answers the shape-level command, routed keyless.
        let ids = book.resolve::<GaugeSampled>(None);
        assert_eq!(ids.ids(), vec![probe.id().0]);
        assert!(ids.for_key("aa").is_empty(), "keyless ids have no key");
        assert!(book.is_empty());

        // A keyless fact still does not answer a keyed command — unchanged.
        let keyed = book.open::<Probe>(about("aa"));
        assert!(book.resolve::<GaugeSampled>(None).is_empty());
        assert_eq!(
            book.open_ids(),
            vec![(keyed.id(), "gauge/probe", about("aa"))]
        );
    }

    /// **RULE 2.7 ON THE REPLY: `accepted` IS NOT `visible`.**
    ///
    /// The not-tracking path closes a promise with no emit, and before 2.7 the
    /// reply it produced was byte-identical to the one on the path where a frame
    /// WILL follow. These two replies must differ in one field and the same field.
    #[test]
    fn the_reply_says_whether_a_frame_will_ever_carry_the_id() {
        let book = PromiseBook::new();

        let will = book.open::<Disable>(about("aa"));
        let coming = will.accepted(Visible::Expected);
        assert_eq!(
            coming.json(),
            serde_json::json!({ "promise_id": will.id().0, "visible": true })
        );

        let wont = book.open::<Disable>(about("bb"));
        let abandoned = book.abandon_all::<AccountDisabled>(Some("bb"));
        assert_eq!(abandoned, vec![wont.id()], "closed, and it says which");
        let never = wont.accepted(Visible::Never);
        assert_eq!(
            never.json(),
            serde_json::json!({ "promise_id": wont.id().0, "visible": false })
        );
        assert_ne!(
            coming.json()["visible"], never.json()["visible"],
            "the two outcomes must not be byte-identical — that is the whole bug"
        );

        // `abandon_all` closes it in the book too: an abandoned promise is not a
        // leak, and the liveness oracle must not report it as one.
        assert_eq!(
            book.open_ids(),
            vec![(will.id(), "accounts/{account}/disable", about("aa"))]
        );
    }

    /// **`changed` IS ABSENT WHEN THE PRODUCER DID NOT LOOK**, never `false`.
    ///
    /// A `false` that means "I did not read the prior state" is the collapse
    /// again: a consumer branching on it concludes its command was a no-op.
    #[test]
    fn changed_is_absent_rather_than_false_when_unanswered() {
        let book = PromiseBook::new();
        let p = book.open::<Disable>(about("aa"));
        let bare = p.accepted(Visible::Expected).json();
        assert!(bare.get("changed").is_none(), "{bare}");
        let answered = p.accepted(Visible::Expected).changed(false).json();
        assert_eq!(answered["changed"], serde_json::json!(false));
        let moved = p.accepted(Visible::Expected).changed(true).json();
        assert_eq!(moved["changed"], serde_json::json!(true));
    }

    /// **A HANDLER THAT FAILS AFTER OPENING CLOSES ITS OWN PROMISE.**
    ///
    /// `gauge.probe` opened one and then answered an error on the `Err` arm
    /// without resolving it: the book held it forever and `open_ids()` reported a
    /// leak no test triggered. A promise never outlives the reply that refused it.
    #[test]
    fn a_refused_command_closes_the_promise_it_had_already_opened() {
        let book = PromiseBook::new();
        let mine = book.open::<Probe>(None);
        let someone_elses = book.open::<Probe>(None);
        assert!(book.abandon(mine.id()));
        assert_eq!(
            book.open_ids(),
            vec![(someone_elses.id(), "gauge/probe", None)],
            "by id, so a concurrent caller's promise is untouched"
        );
        assert!(
            !book.abandon(mine.id()),
            "idempotent, and says it was already gone"
        );
    }

    #[test]
    fn ids_are_never_reused_and_start_at_one() {
        let book = PromiseBook::new();
        assert_eq!(book.open::<Disable>(about("aa")).id(), PromiseId(1));
        let _ = book.resolve::<AccountDisabled>(Some("aa"));
        assert_eq!(book.open::<Probe>(about("aa")).id(), PromiseId(2));
    }

    /// The liveness half, in miniature: a handler that forgets to emit leaves
    /// the book non-empty, which is exactly what the suite-wide test asserts.
    #[test]
    fn a_handler_that_never_emits_is_visible_in_the_book() {
        let book = PromiseBook::new();
        let _ = book.open::<Disable>(about("aa"));
        assert!(!book.is_empty());
        assert_eq!(book.open_ids()[0].1, "accounts/{account}/disable");
        assert_eq!(book.open_ids()[0].2, about("aa"), "and about WHAT");
    }

    // ═════════════════════════════════════════════════════════════════════════
    // Rule 2.7's third stage — the barrier
    // ═════════════════════════════════════════════════════════════════════════

    use crate::envelope::codes;
    use std::sync::atomic::AtomicUsize;

    /// A producer that can promise durability, and **counts whether it was
    /// asked to**. The count is the assertion in half these tests: a refusal
    /// that still paid for an fsync is not the refusal the rule describes.
    struct Fake {
        issued: u64,
        max: usize,
        commits: AtomicUsize,
        refuse: Option<&'static str>,
    }

    impl Fake {
        fn issuing(n: u64) -> Self {
            Self {
                issued: n,
                max: MAX_PROMISES_PER_BARRIER,
                commits: AtomicUsize::new(0),
                refuse: None,
            }
        }
        fn with_max(mut self, m: usize) -> Self {
            self.max = m;
            self
        }
        fn refusing(mut self, why: &'static str) -> Self {
            self.refuse = Some(why);
            self
        }
        fn commits(&self) -> usize {
            self.commits.load(Ordering::Relaxed)
        }
    }

    impl Durable for Fake {
        fn max_promises(&self) -> usize {
            self.max
        }
        fn issued(&self) -> PromiseId {
            PromiseId(self.issued)
        }
        fn commit(&self) -> Result<(), crate::envelope::Refusal> {
            self.commits.fetch_add(1, Ordering::Relaxed);
            match self.refuse {
                None => Ok(()),
                Some(why) => Err(crate::envelope::Refusal::new(codes::BUSY, why)),
            }
        }
    }

    fn ask(ids: &[u64]) -> serde_json::Value {
        serde_json::json!({ "promises": ids })
    }

    /// **A PRODUCER THAT CANNOT PROMISE DURABILITY REFUSES. IT DOES NOT ANSWER
    /// A WEAKER STAGE UNDER THIS STAGE'S NAME.**
    ///
    /// `unsupported` and not `unknown_method`: the method is on every chassis so
    /// that this answer is REACHABLE, exactly as the resource route is mounted on
    /// a producer with no resources. A consumer that got `unknown_method` would
    /// conclude it was talking to an old daemon; one that got `ok` would conclude
    /// it had a guarantee.
    ///
    /// **Forced red** by `barrier(None, …)` falling through to `Ok` with
    /// `{"stage":"durable","promises":[1]}` — which is the silent downgrade
    /// itself, and the one outcome this rule will not trade.
    #[test]
    fn a_producer_with_no_durability_refuses_rather_than_downgrading() {
        let r = barrier(None, &ask(&[1])).unwrap_err();
        assert_eq!(r.code, codes::UNSUPPORTED);
        assert!(r.message.contains("promises no durability"), "{}", r.message);
        assert!(
            r.message.contains("downgrading"),
            "the refusal says what it is refusing to do, because the consumer \
             cannot see the alternative: {}",
            r.message
        );
    }

    /// **RULE 2.8: REFUSED NAMING THE CEILING, NEVER CLAMPED — AND THE FSYNC IS
    /// NOT PAID FOR AN ASK THAT WAS GOING TO BE REFUSED.**
    ///
    /// The clamp is worse here than anywhere else the rule applies: it would
    /// answer `stage: durable` over a PREFIX, and the caller — holding a reply
    /// that says durable — could not tell which of its writes the commit
    /// covered.
    ///
    /// **Forced red** two ways, separately. (a) `ids.truncate(max)` in place of
    /// the refusal: the ask of 4 ids comes back `Ok` with
    /// `{"stage":"durable","promises":[1,2]}` and the assertion on `is_err`
    /// fails. (b) the ceiling check moved BELOW `producer.commit()`: the code and
    /// message still match, and `commits == 0` fails with `1`.
    #[test]
    fn the_ceiling_is_refused_naming_itself_and_the_commit_is_never_paid() {
        let f = Fake::issuing(10).with_max(2);
        let r = barrier(Some(&f), &ask(&[1, 2, 3, 4])).unwrap_err();
        assert_eq!(r.code, codes::MALFORMED);
        assert!(
            r.message.contains("at most 2"),
            "the ceiling is NAMED, so a consumer never has to probe for it: {}",
            r.message
        );
        assert_eq!(
            f.commits(),
            0,
            "a refusal that can be answered for free must not cost an fsync"
        );
        // …and the ask AT the ceiling is served, so the refusal is a ceiling and
        // not an off-by-one.
        let ok = barrier(Some(&f), &ask(&[1, 2])).unwrap();
        assert_eq!(ok["promises"], serde_json::json!([1, 2]));
        assert_eq!(f.commits(), 1);
    }

    /// **THE LIMIT THAT ACTUALLY ARBITRATES, AT ITS OWN NUMBER: 1024 IS SERVED
    /// AND 1025 IS REFUSED.** NEW IN THIS TREE — substrate has no test that
    /// serves an ask of `MAX_PROMISES_PER_BARRIER` ids.
    ///
    /// `the_declared_contract_is_the_producers_own_ceiling` asserts one id served
    /// and 1025 refused, and `the_ceiling_is_refused_naming_itself…` asserts the
    /// boundary at a FABRICATED ceiling of 2. Neither of them asks for exactly
    /// 1024, so the pair is satisfied by `asked.len() >= max` at the real
    /// constant as long as the fabricated one is also wrong — and it is the real
    /// constant that every producer which overrides nothing actually runs on. A
    /// limit is proven at N >= 2 on BOTH sides of its own edge or it is not
    /// proven.
    ///
    /// **Forced red** two ways. (a) `asked.len() >= max`: the 1024 ask comes back
    /// `Err(malformed)` — *`promises` names 1024 ids; this producer's barrier
    /// takes at most 1024 in one call* — and the first `unwrap()` panics. (b)
    /// `asked.len() > max + 1`: the 1024 ask still passes and the 1025 ask comes
    /// back `Ok` with 1025 promises, so the second assertion fails. The red runs
    /// are in the report; a constant implementation of this check fails one arm
    /// or the other by construction, which is why both are here.
    #[test]
    fn the_default_ceiling_serves_exactly_max_promises_and_refuses_one_more() {
        let at: Vec<u64> = (1..=MAX_PROMISES_PER_BARRIER as u64).collect();
        let over: Vec<u64> = (1..=MAX_PROMISES_PER_BARRIER as u64 + 1).collect();
        assert_eq!(at.len(), 1024, "the number this crate publishes");
        assert_eq!(over.len(), 1025);

        // AT the boundary, through the trait DEFAULT — the ceiling every
        // producer that overrides nothing gets.
        let served = barrier(Some(&Bare), &ask(&at))
            .expect("an ask of exactly MAX_PROMISES_PER_BARRIER ids is served");
        assert_eq!(served["stage"], serde_json::json!("durable"));
        assert_eq!(
            served["promises"].as_array().unwrap().len(),
            MAX_PROMISES_PER_BARRIER,
            "and all 1024 are echoed, because the reply is per id"
        );

        // ONE PAST IT, and the refusal names the number.
        let f = Fake::issuing(2000);
        let r = barrier(Some(&f), &ask(&over)).unwrap_err();
        assert_eq!(r.code, codes::MALFORMED);
        assert!(
            r.message.contains("names 1025 ids") && r.message.contains("at most 1024"),
            "the refusal names both sides of the edge: {}",
            r.message
        );
        assert_eq!(
            f.commits(),
            0,
            "and one past the ceiling costs no fsync either"
        );
    }

    /// **AN ID THIS PRODUCER NEVER MINTED IS `unknown`, AND NOTHING IS
    /// COMMITTED.**
    ///
    /// The one claim the barrier makes beyond the caller's own is *"these are
    /// ids I issued"*. An id above the watermark came from somewhere else — a
    /// second producer, a previous boot, the consumer's own arithmetic — and
    /// answering `durable` for it would be the chassis agreeing with whatever
    /// produced it.
    ///
    /// **Forced red** by deleting the `issued` loop: the ask comes back
    /// `Ok({"stage":"durable","promises":[1,99]})`, which is a durability
    /// guarantee about an id that does not exist.
    #[test]
    fn an_id_this_producer_never_issued_is_unknown_and_nothing_is_committed() {
        let f = Fake::issuing(3);
        let r = barrier(Some(&f), &ask(&[1, 99])).unwrap_err();
        assert_eq!(r.code, codes::UNKNOWN);
        assert!(r.message.contains("promise 99"), "{}", r.message);
        assert!(r.message.contains("issued 3"), "{}", r.message);
        assert_eq!(f.commits(), 0, "checked before the fsync, not after");

        // Zero is not an id either: the book's first id is 1, so a `0` is a
        // caller that counted from the wrong end.
        assert_eq!(
            barrier(Some(&f), &ask(&[0])).unwrap_err().code,
            codes::UNKNOWN
        );

        // A promise that has already RESOLVED is not unknown. The book forgets a
        // resolved promise, so the only answerable question is whether the id was
        // ever minted — and that is the right question: `accepted` already means
        // the store write is done.
        assert!(barrier(Some(&f), &ask(&[1, 2, 3])).is_ok());
    }

    /// **`promises` IS REQUIRED, AND THERE IS NO FORM THAT MEANS
    /// `everything`.**
    ///
    /// A deliberate narrowing of substrate-interface-layer's
    /// `docs/rules-audit.md` §4 `barrier {promise_ids?}`. An omitted list could
    /// only be answered as a watermark, and a watermark claims ids `1..=N` are
    /// durable — true only if mint order equals write order, which is a producer
    /// convention the chassis can neither enforce nor observe.
    ///
    /// **Forced red** by treating an absent `promises` as `1..=issued()`: the
    /// bare `{}` comes back `Ok` and the first assertion fails. Each of the four
    /// malformed asks was also checked individually against a parser that
    /// `unwrap_or_default()`s, where all four become the empty ask.
    #[test]
    fn promises_is_required_and_there_is_no_everything_form() {
        let f = Fake::issuing(5);
        for (bad, what) in [
            (serde_json::json!({}), "absent"),
            (serde_json::json!({ "promises": 3 }), "not an array"),
            (serde_json::json!({ "promises": [] }), "empty"),
            (serde_json::json!({ "promises": ["3"] }), "a string id"),
            (serde_json::json!({ "promises": [-1] }), "a negative id"),
        ] {
            let r = barrier(Some(&f), &bad).unwrap_err();
            assert_eq!(r.code, codes::MALFORMED, "{what}: {r:?}");
        }
        assert_eq!(f.commits(), 0, "none of those cost an fsync");
    }

    /// **THE REPLY IS THE MILESTONE AND THE IDS. IT CARRIES NOTHING ABOUT THE
    /// STORE.**
    ///
    /// The operator's ruling: *"ack should just inform the general milestones,
    /// not protocol-specific details. User should not care."* The producer knows
    /// its `synchronous` level, how many WAL frames the checkpoint folded, and
    /// whether this call had anything to do at all — and a consumer can act on
    /// none of it.
    ///
    /// It is a sorted SET, which is what makes a barrier over several promises
    /// honest about subjects: per id, never a watermark.
    ///
    /// **Forced red** by adding `"synchronous": 2` to the reply — the key-set
    /// assertion fails with `["promises","stage","synchronous"]`.
    #[test]
    fn the_reply_is_the_stage_and_the_ids_and_nothing_about_the_store() {
        let f = Fake::issuing(9);
        let r = barrier(Some(&f), &ask(&[7, 3, 3, 5])).unwrap();
        assert_eq!(r["stage"], serde_json::json!("durable"));
        assert_eq!(
            r["promises"],
            serde_json::json!([3, 5, 7]),
            "sorted, deduplicated, and exactly the ids the caller named"
        );
        let keys: Vec<&str> = r.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["promises", "stage"],
            "two keys. No `synchronous`, no frame counts, no timing: {r}"
        );
        // **AND IT IS NOT A WATERMARK.** This producer has issued nine ids; the
        // caller named three; the reply names those three and says nothing about
        // ids 1, 2, 4, 6, 8 or 9. A `durable_through: 7` would have claimed all
        // of 1..=7, and on substrate's `threads` producer that claim is false and
        // not merely unverifiable — handlers mint an id BEFORE they write, so an
        // unnamed id below the watermark can belong to a handler that has not
        // reached its store call. See [`barrier`]'s doc.
        //
        // **Forced red** by replacing the echo with `(1..=highest_named)`, which
        // is what a `durable_through` reply amounts to: *left: [1,2,3,4,5,6,7],
        // right: [3,5,7]*.
        for unnamed in [1u64, 2, 4, 6, 8, 9] {
            assert!(
                !r["promises"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!(unnamed)),
                "{unnamed} was not asked about and must not be answered: {r}"
            );
        }
    }

    /// **A COMMIT THAT DID NOT HAPPEN IS NOT ANSWERED `durable`.**
    ///
    /// The producer's refusal reaches the caller unchanged, code and all. A
    /// barrier that swallowed it would be the downgrade one layer in, where it
    /// is harder to see: the capability said yes and the call said ok.
    ///
    /// **Forced red** by `let _ = producer.commit();` — the ask comes back
    /// `Ok({"stage":"durable",…})` after a commit that failed.
    #[test]
    fn a_commit_that_failed_is_reported_and_never_answered_durable() {
        let f = Fake::issuing(4).refusing("the checkpoint could not get its lock");
        let r = barrier(Some(&f), &ask(&[4])).unwrap_err();
        assert_eq!(r.code, codes::BUSY, "the remedy is to call it again");
        assert!(r.message.contains("could not get its lock"), "{}", r.message);
        assert_eq!(f.commits(), 1, "it was tried");
    }

    /// **THE WATERMARK IS THE BOOK'S, AND IT OUTLIVES THE PROMISES.**
    ///
    /// [`PromiseBook::issued`] is the only question the book can still answer
    /// about a promise it has resolved and forgotten, and the barrier's
    /// mintedness check is built on exactly that. A watermark derived from the
    /// OPEN map instead would refuse every id the moment its frame went out,
    /// which is the moment a caller is most likely to ask.
    ///
    /// **Forced red** by `issued()` returning `self.open.lock().unwrap().len()`:
    /// the first assertion passes, and the one after the resolve fails with `0`.
    #[test]
    fn issued_is_the_watermark_and_a_resolved_promise_does_not_lower_it() {
        let book = PromiseBook::new();
        assert_eq!(book.issued(), PromiseId(0), "nothing issued yet");
        let a = book.open::<Disable>(about("aa"));
        let b = book.open::<Disable>(about("bb"));
        assert_eq!(book.issued(), b.id());
        let _ = book.resolve::<AccountDisabled>(Some("aa"));
        let _ = book.resolve::<AccountDisabled>(Some("bb"));
        assert!(book.is_empty(), "both resolved and forgotten");
        assert_eq!(
            book.issued(),
            b.id(),
            "and the watermark does not move back — a barrier over an id whose \
             frame has already gone out is the ordinary case"
        );
        assert_eq!(a.id(), PromiseId(1));
    }

    /// A producer that overrides NOTHING it does not have to. It exists because
    /// substrate's first cut of `the_declared_contract_is_the_producers_own_ceiling`
    /// asserted the default through [`Fake`], which sets its `max` field to
    /// `MAX_PROMISES_PER_BARRIER` in its constructor — so the assertion read a
    /// field initialised to the right number and never the trait default at all.
    /// It stayed GREEN with `max_promises`'s default body changed to `0`.
    struct Bare;
    impl Durable for Bare {
        fn issued(&self) -> PromiseId {
            PromiseId(2000)
        }
        fn commit(&self) -> Result<(), crate::envelope::Refusal> {
            Ok(())
        }
    }

    /// **THE CONTRACT A CONSUMER READS BEFORE IT ASKS** — rule 2.8's last
    /// clause. One number, from the producer, so nothing has to be probed for.
    ///
    /// **Forced red** by `max_promises`'s DEFAULT body returning `0`: the
    /// `Bare` assertions fail with `0` against `1024`, and the barrier over one
    /// id comes back `malformed` — *`promises` names 1 ids; this producer's
    /// barrier takes at most 0 in one call*. The same mutation left substrate's
    /// first cut of this test green; see [`Bare`].
    #[test]
    fn the_declared_contract_is_the_producers_own_ceiling() {
        let f = Fake::issuing(1).with_max(7);
        assert_eq!(DurableContract::of(&f), DurableContract { max_promises: 7 });
        assert_eq!(
            serde_json::to_value(DurableContract::of(&f)).unwrap(),
            serde_json::json!({ "max_promises": 7 })
        );

        // **THE DEFAULT IS THE CHASSIS'S CONSTANT, NOT ZERO** — a producer that
        // overrides nothing gets a usable barrier rather than one that refuses
        // every ask. Asserted through a type that overrides nothing, which is
        // the only way to assert it.
        assert_eq!(Bare.max_promises(), MAX_PROMISES_PER_BARRIER);
        assert_eq!(
            DurableContract::of(&Bare),
            DurableContract {
                max_promises: MAX_PROMISES_PER_BARRIER
            }
        );
        assert!(
            barrier(Some(&Bare), &ask(&[1])).is_ok(),
            "and the default serves an ask rather than refusing it"
        );
        let over: Vec<u64> = (1..=MAX_PROMISES_PER_BARRIER as u64 + 1).collect();
        assert_eq!(
            barrier(Some(&Bare), &ask(&over)).unwrap_err().code,
            codes::MALFORMED,
            "one past the default is still refused"
        );
    }

    /// **A DECLARED `NeverVisible` COMMAND BEHAVES THE WAY THE DECLARATION SAYS.**
    ///
    /// The point of the declaration is that `visible: false` stops being something
    /// a handler remembers and becomes something a reader finds in the command's
    /// definition. This asserts the two halves that are observable at runtime: the
    /// promise opens and gets an id (a reply with no id would be
    /// indistinguishable from a method outside the contract), and the reply says
    /// never.
    ///
    /// The third half — that nothing can emit the fact — is carried by the TYPE:
    /// [`NeverVisible`] is uninhabited, so there is no value to pass to
    /// `resolve`. That is asserted by this test's existence rather than by a line
    /// in it, because the line cannot be written. The report for this step forces
    /// that ban red by writing the line that cannot be written and pasting
    /// rustc's refusal, because a type-level ban nobody has seen reject anything
    /// is a sentence that can never fail.
    #[test]
    fn a_command_that_resolves_to_nothing_says_so_on_the_reply() {
        let book = PromiseBook::new();
        let p = book.open::<NothingCarriesThis>(about("k"));
        assert_eq!(p.id(), PromiseId(1), "it still gets an id");
        let reply = p.accepted(Visible::Never);
        assert!(
            !reply.visible,
            "and the reply says no frame will ever carry it"
        );
        book.abandon(p.id());
        assert!(
            book.open_ids().is_empty(),
            "a promise nothing can resolve does not outlive the reply that answered it"
        );
    }

    /// **AND A COMMAND WHOSE FACT A SHAPE DOES CLAIM IS THE OTHER CASE** — the
    /// positive control for the one above. A bound that refused everything would
    /// not be a bound.
    #[test]
    fn a_command_a_shape_resolves_opens_an_ordinary_promise() {
        let book = PromiseBook::new();
        let p = book.open::<Disable>(about("aa"));
        assert!(p.accepted(Visible::Expected).visible);
        assert_eq!(book.open_ids().len(), 1);
        assert_eq!(
            book.resolve::<AccountDisabled>(Some("aa")),
            Resolving::keyed("aa", vec![p.id().0]),
            "and the fact its declared shape emits is what closes it"
        );
        assert!(
            book.open_ids().is_empty(),
            "which leaves the book empty, as a kept promise does"
        );
    }

    /// **THE REPLY PARSES BACK, INCLUDING THE SHAPE A CONSUMER ACTUALLY
    /// RECEIVES — THE ONE WITH NO `changed`.** NEW IN THIS TREE.
    ///
    /// [`Accepted`] and [`DurableContract`] both derive `Deserialize` and
    /// substrate asserts neither direction. They are WIRE types: a consumer is
    /// the thing that parses them, and `changed`'s `#[serde(default)]` is the
    /// only reason the common reply — which omits the field entirely, as
    /// `changed_is_absent_rather_than_false_when_unanswered` proves — is
    /// parseable at all. A `Deserialize` nobody has run is a claim.
    ///
    /// **TWO ATTRIBUTES ON THESE TYPES CANNOT BE SHOWN TO DO ANYTHING, AND
    /// THIS COMMENT SAYS SO RATHER THAN CLAIMING THEM.** Measured, not
    /// reasoned: removing `#[serde(default)]` from `changed`, and removing
    /// `#[serde(transparent)]` from [`PromiseId`], each leave this test GREEN
    /// (`rc=0`, `1 passed`). serde's derive already treats a missing
    /// `Option<T>` field as `None`, and serde_json already represents a
    /// one-field tuple struct as its inner value, so for JSON both attributes
    /// are redundant. They are kept — `transparent` is load-bearing for a
    /// non-self-describing format, and both are substrate's spelling of a wire
    /// type — but a test whose stated forcing is a no-op is a sentence that
    /// can never fail, so the forcings named here are the ones that fire.
    ///
    /// **Forced red** three ways. (a) `#[serde(rename = "pid")]` on
    /// `promise_id`: *missing field `promise_id`* on the parse. (b)
    /// `skip_serializing_if` dropped from `changed`: the re-serialise emits
    /// `"changed": null` and `parsed.json() == on_the_wire` fails. (c)
    /// `#[serde(rename = "max")]` on `DurableContract::max_promises`: the
    /// contract no longer parses. All three reds are in the report.
    #[test]
    fn the_reply_and_the_contract_parse_back_off_the_wire() {
        // The shape a producer that did not look sends, verbatim.
        let on_the_wire = serde_json::json!({ "promise_id": 7, "visible": true });
        let parsed: Accepted = serde_json::from_value(on_the_wire.clone()).unwrap();
        assert_eq!(parsed.promise_id, PromiseId(7), "transparent, not nested");
        assert!(parsed.visible);
        assert_eq!(
            parsed.changed, None,
            "absent on the wire parses as unanswered, not as `false`"
        );
        assert_eq!(parsed.json(), on_the_wire, "and re-serialises unchanged");

        // And the answered form, both ways, so the field is not write-only.
        let answered = serde_json::json!({ "promise_id": 7, "visible": false, "changed": false });
        let parsed: Accepted = serde_json::from_value(answered.clone()).unwrap();
        assert_eq!(parsed.changed, Some(false), "a real `false` survives");
        assert_eq!(parsed.json(), answered);

        // The contract a consumer reads before it asks.
        let c: DurableContract =
            serde_json::from_value(serde_json::json!({ "max_promises": 1024 })).unwrap();
        assert_eq!(
            c,
            DurableContract {
                max_promises: MAX_PROMISES_PER_BARRIER
            }
        );
    }
}
