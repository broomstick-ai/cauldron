//! # The hub — the FIFO, the shape task, and its supervisor
//!
//! **THE RUNTIME, and the only module in this crate that links tokio.** Generic
//! over `I`, the producer's input type: the chassis counts inputs, **stamps each
//! one exactly once**, and hands them to folds in **strict arrival order**. It
//! never matches on an input. The only thing it asks of `I` is [`Kind::kind`],
//! a coarse name for the burst warning.
//!
//! Those two promises are the whole reason the module exists, so they are worth
//! saying where the mechanism is rather than only in a doc:
//!
//! * **Stamped once.** [`Hub::emit`] is the one emit point and
//!   [`Hub::emit_at`]/[`Hub::emit_resolving`] are the two ways of saying *the
//!   caller already took the stamp*. All three call `fan_out`, which builds ONE
//!   [`Stamped<I>`] and clones THAT into every tracking shape's FIFO — so the
//!   N shapes of one producer see one value with one `at`, and there is no
//!   second `Utc::now()` anywhere downstream of the emit. `emit_at` exists
//!   because a producer that reads its store before it sends must stamp BEFORE
//!   the read, or a fact whose content is from t0 arrives stamped t3 and sets a
//!   level from pre-hydrate content.
//! * **Folded in arrival order.** [`Fifo`] is a `VecDeque` under a mutex and
//!   not an `mpsc`, for one reason: control items — subscribe, unsubscribe,
//!   drop — are appended to the SAME queue the inputs are in. So an export is
//!   placed among the events it must reflect instead of racing them on a side
//!   channel, and one task pops one item at a time. A second queue would need a
//!   watermark to recover the order, and neither queue carries one.
//!
//! ## What is here and what is not
//!
//! **The frames a consumer parses are NOT here — they are in [`crate::wire`].**
//! `Op`, `ShapeState`, `Epoch`, `ShapeWire`, `ShapeFrame`, `ShapeInfo` and
//! `ShapeError` are inert data with no async in them. They lived in this file
//! only because that is where the chassis extraction happened to leave them,
//! which put every type a MIRROR must read behind tokio. The one piece of frame
//! code still here is `impl ShapeInfo`, because building one takes a
//! [`Tracking`] and a shape's declared counters — both hub concepts.
//!
//! So this module is what a producer's RUNTIME needs and nothing else. It owns:
//!
//! | | |
//! |---|---|
//! | [`Shape`] | the two declarations a shape makes — `folds` and `hydrates` — and the ceilings rule 2.8 makes it publish |
//! | [`Fifo`] | arrival order, the capacity drop, and the high-water mark |
//! | [`Hub`] | registration, the connection id space, the subscribe doors, and the per-connection frame channels |
//! | `ShapeTask` | one spawned task per shape: init, fold, debounce, export, page, beat, and the armed future-`at` timers |
//! | [`ConnFrames`] | one bounded channel per connection, so a frame addressed to one consumer is charged to one buffer |
//! | [`Hydrate`], [`Hydration`], [`Handles`] | how a key enters a root, and what a producer answers at init |
//!
//! Its whole `use` set is `std::collections`, `std::sync`, `chrono::Duration`,
//! `tokio::sync`, and four modules of this crate: [`crate::fold`],
//! [`crate::wire`], [`crate::cell`], [`crate::ledger`]. **Nothing in this file
//! reaches outside the crate**, which is the property that lets the faces vendor
//! it from `src/` with a manifest they write themselves.
//!
//! ## Five things that stayed behind with the producer
//!
//! The `Input` enum, the legacy firehose, an accounts-shape counter, a
//! hard-coded `keyed = true`, and a FIFO capacity justified by one producer's
//! measured peak. Each is now a generic parameter, a trait method, or an
//! argument — except `keyed`, which turned out to be **nothing at all**.
//! `Shape::KEYED` was declared, carried onto `shapes.list`, and read by NO code:
//! `do_subscribe` iterates `Root::keys()`, and `export(key: None)` builds a
//! frame whose `row` and `cells` are both `None` by construction, so a
//! `KEYED = false` shape would have been acked and handed nothing — the
//! ack-then-silence class this file is built against. The const and the wire
//! field are gone. **Every shape is a keyed pool, and a "keyless" one is a pool
//! with one fixed key.**

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, RwLock};

use chrono::Duration as ChronoDuration;
use tokio::sync::{mpsc, watch, Notify};

use crate::fold::Root;
use crate::fold::Membership;
use crate::wire::{
    Beat, ConnId, Derived, Detail, Epoch, MembersPage, Op, Paging, Resolving, Retain,
    ShapeError, ShapeFrame,
    ShapeContract, ShapeInfo, ShapeMax, ShapeState, ShapeWire, Stamped, Timestamp,
};

/// **The one thing the chassis asks of a producer's input type.**
///
/// A coarse name for the burst warning, so an operator can tell which input
/// made a fold re-emit keys it did not change. That is the entire contract:
/// the chassis never matches on an input, only counts and forwards it.
pub trait Kind {
    fn kind(&self) -> &'static str;
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. The hydrate contract
// ─────────────────────────────────────────────────────────────────────────────

/// A request the shape makes of its producer at init.
pub trait HydrateRequest: Default + Send + 'static {
    const NAME: &'static str;
    type Response: Send + 'static;
}

/// **The producer answers a request, or it does not compile.**
///
/// `Accounts::hydrates` calls `p.hydrate(PoolRoster::default())` on a concrete
/// `ThreadManager`; a request the producer cannot answer is a compile error at
/// the shape, not a runtime audit. Types catch the wrong response type and the
/// missing handler. They do NOT catch emptiness or completeness — a
/// `Complete(vec![])` where there were four accounts is a handler that LIED,
/// and that is the CI round trip's job.
pub trait Handles<R: HydrateRequest> {
    fn hydrate(&self, req: R) -> Hydration<R::Response>;
}

/// Provenance is Debezium's: a hydrate response is a LABELLED READ of current
/// state and never pretends to be an occurrence.
#[derive(Debug, Clone)]
pub enum Hydration<E> {
    /// Exactly these rows, and that is all of them.
    Complete(Vec<E>),
    /// There are none, and here is why. **Empty is a statement; it renders as
    /// empty, not as `?`.**
    Empty { reason: &'static str },
    /// Some rows, and the rest could not be read. Rows in `got` hydrate their
    /// key `complete: true`; keys with no row are left `NotHydrated{reason}` —
    /// the rule the critique's F8 said the design left to invention.
    Partial { got: Vec<E>, reason: String },
}

impl<E> Hydration<E> {
    pub fn rows(&self) -> &[E] {
        match self {
            Hydration::Complete(v) | Hydration::Partial { got: v, .. } => v,
            Hydration::Empty { .. } => &[],
        }
    }

    pub fn into_rows(self) -> Vec<E> {
        match self {
            Hydration::Complete(v) | Hydration::Partial { got: v, .. } => v,
            Hydration::Empty { .. } => Vec::new(),
        }
    }

    pub fn is_complete(&self) -> bool {
        !matches!(self, Hydration::Partial { .. })
    }

    pub fn reason(&self) -> Option<String> {
        match self {
            Hydration::Empty { reason } => Some((*reason).to_string()),
            Hydration::Partial { reason, .. } => Some(reason.clone()),
            Hydration::Complete(_) => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. The shape itself
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tracking {
    /// Init at `register`, at daemon boot. Required by any shape holding a
    /// [`crate::fold::Counter`], which cannot be hydrated.
    Eager,
    /// No subscribers → the FIFO accepts no inputs and the task holds no state.
    /// The first subscribe item runs init and then exports; the last
    /// unsubscribe clears state. No grace period; a reconnect re-hydrates.
    Lazy,
}

impl Tracking {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tracking::Eager => "eager",
            Tracking::Lazy => "lazy",
        }
    }
}

/// **HOW A KEY ENTERS THIS SHAPE'S SCOPE, declared by field the way
/// [`Tracking`] is** — the operator's ruling, and the reason it is a const on
/// [`Shape`] and not a subscribe parameter: it is a property of the shape, so a
/// consumer cannot get it wrong and two consumers cannot disagree about it.
///
/// **A BINARY, AND IT USED TO BE A THREE-WAY.** It was
/// `KeyScope::{Fleet, Active, OnDemand}`, and `Active`'s own doc comment said
/// what was wrong with it: *"This is a CLAIM, and today the chassis behaves
/// identically to `Fleet` for it."* An axis value with no mechanism is a place
/// for a future mechanism to be hung by someone who reads the enum and not the
/// doc. The claim `Active` was making — *this set is a live subset, not the
/// universe* — is a COMPLETENESS FACT, and completeness already has two wire
/// homes that carry it per frame: `CellState::Known{complete}` /
/// `Hydration::Partial`, and `ShapeWire::complete`. A fact that changes per
/// export cannot be a const, which is the whole of the argument
/// (`docs/access-patterns.md` 7.2).
///
/// So what is left is one question with two answers: **how does a key get into
/// this root?**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hydrate {
    /// **READ THE WHOLE ROOT AT INIT.** What `accounts`, `narration` and
    /// `threads/summary` do: one statement per epoch produces every key this
    /// shape will hold, so a keyless subscribe is an inventory and a keyed
    /// subscribe naming a key that does not exist yet is standing interest in a
    /// name that may become one — a pool reload adds a login.
    ///
    /// **It says nothing about whether that set is the universe.** A `Whole`
    /// read that hit its own cap answers `Hydration::Partial`, the rows land
    /// `complete: false`, and the export says so: that is `Active`'s claim,
    /// made where it can be true on one epoch and false on the next.
    ///
    /// The DEFAULT, so a shape written before this const existed is unchanged.
    Whole,
    /// **READ ONE KEY WHEN SOMEBODY NAMES IT** — [`Shape::hydrates_key`]. The
    /// default key set may be empty or small.
    ///
    /// `docs/chassis-resources.md` promotes this from an option to a
    /// requirement: a nested shape's key space is `(parent_key, own_key)` and a
    /// VFS root cannot be hydrated whole, so "read the entire root at init" is
    /// not a thing that can be asked of it.
    ///
    /// **A keyless `detail: rows` subscribe on such a shape is REFUSED** with
    /// [`ShapeError::KeyRequired`], naming [`Shape::MAX_ROWS_PER_SUBSCRIBE`], on
    /// the request id. "Every key I know about" on a shape that knows keys only
    /// on demand is "nothing", and answering `{queued: true}` and then exporting
    /// nothing is the ack-then-silence class [`Hub::subscribe`]'s timer cap was
    /// moved out of the task to close.
    ///
    /// **A keyless `detail: keys` subscribe IS answered**, and that is the
    /// change this enum's collapse was bundled with: a pool that cannot afford
    /// to hand you its rows can still afford to tell you what is in it. See
    /// [`Shape::hydrates_members`].
    PerKey,
}

impl Hydrate {
    pub fn as_str(&self) -> &'static str {
        match self {
            Hydrate::Whole => "whole",
            Hydrate::PerKey => "per_key",
        }
    }
}

/// Two declarations, `folds` and `hydrates`. **Masking does not exist** in any
/// form.
pub trait Shape<I>: Send + 'static {
    const NAME: &'static str;
    const TRACKING: Tracking;

    /// **THE VERSION OF THIS SHAPE'S CONTRACT THIS PRODUCER CALLS CURRENT** —
    /// rule 2.6's producer declaration, and the half that did not exist until
    /// 2026-10-05. A consumer states what it was built against (rule 2.1); this is
    /// what it is stated against, and [`Hub::subscribe_as`] refuses the link where
    /// the two cannot be reconciled.
    ///
    /// **It moves only on REMOVAL or SEMANTIC CHANGE, never on an addition.** That
    /// is rule 2.6's *cheapening property* and it is what keeps this number from
    /// becoming a build stamp: additive changes need no bump, PROVIDED every
    /// consumer chassis ignores cells and fields it does not know — which is a
    /// stated obligation of a consumer and which [`crate::mirror`] holds. Two
    /// additive changes to `accounts` (`disabled`, `machine_login`) shipped before
    /// this const existed and the board absorbed both without one, which is the
    /// property observed before it was named.
    ///
    /// **`1` IS THE DEFAULT, and that is a claim about today rather than a guess.**
    /// Every other const on this trait defaults so that *"nothing changes for a
    /// shape whose author has not thought about this"*, and the worry that makes
    /// this one different — a shape silently claiming v1 while really being on its
    /// fourth revision — cannot happen at the moment this const comes into
    /// existence: no shape on any producer has ever bumped, because there was
    /// nothing to bump. The number is an opaque token for *which contract*, not a
    /// count of revisions past, so `1` is TRUE of every shape that exists the day
    /// this lands and true of every shape written afterwards on its first
    /// contract. What no default could protect against is an author who CHANGES a
    /// contract and forgets to bump, and that is rule 2.6's own soft spot —
    /// *"'semantic change' is a judgement with no detector"* — not something a
    /// defaulted or undefaulted const decides.
    ///
    /// **`0` is refused at [`Hub::register`]**, so *"I did not think about this"*
    /// has no spelling on the wire and a consumer reading
    /// [`crate::wire::ShapeContract::version`] is reading a declaration somebody
    /// made.
    const VERSION: u32 = 1;

    /// **THE OLDER VERSIONS THIS SHAPE STILL SERVES** — rule 2.6's second state,
    /// and the producer's opt-in to accommodating a consumer it could refuse.
    ///
    /// **`&[]` IS THE DEFAULT, so exact-match-only is what a shape gets for free
    /// and accommodation is something an author has to write down.** That is the
    /// standing preference in this codebase expressed as a default rather than as
    /// advice: the loud answer is the one you get by not thinking, and the kind one
    /// costs a declaration.
    ///
    /// **Serving an older version is NOT a silent accommodation**, which is the
    /// only reason it is permitted here at all. Rule 2.6 requires the link to
    /// carry a WARNING for its whole life — every export's
    /// [`crate::wire::Epoch::superseded`] names both versions, and the consumer
    /// ledger records the link as superseded — so a consumer running on an old
    /// contract is a fact on the wire and in a file, not a thing the producer
    /// quietly absorbed. A clamp hides; this announces.
    ///
    /// **STRICTLY DESCENDING, and every entry strictly below [`Self::VERSION`]**.
    /// [`Hub::register`] panics otherwise, in the producer's build function, like
    /// every other declaration check here: a duplicate, an out-of-order entry, or
    /// [`Self::VERSION`] repeated in this list would each make the published
    /// `serves` list disagree with itself, and a contract a consumer reads has to
    /// have one reading.
    ///
    /// **Rule 2.6's own advice is to keep this to ONE version back** — *"a
    /// `SERVES` list is compatibility code in the producer, which the operator's
    /// 'no pointers to the past' rule cuts against"*. That is advice and is NOT
    /// enforced here: a ceiling on the length would be a number nobody ruled, and
    /// inventing one is the thing rule 2.8 says a producer may not do to a
    /// consumer. It is written down so an author reads it before adding a third
    /// entry.
    const SERVES: &'static [u32] = &[];

    /// **HOW A KEY ENTERS SCOPE** — see [`Hydrate`]. Set by field, like
    /// [`Self::TRACKING`]; defaults to what `accounts` does, so no existing shape
    /// changes behaviour by this const coming into existence.
    const HYDRATE: Hydrate = Hydrate::Whole;

    /// **THE MOST ROWS ONE `shapes.subscribe` WILL EXPORT** — rule 2.8's
    /// ceiling for the key-selection parameter, and the number a keyless
    /// `detail: rows` refusal on a [`Hydrate::PerKey`] shape names.
    ///
    /// 500, which is `server::QUERY_MAX_LIMIT` — the number this daemon already
    /// calls one page. On a [`Hydrate::Whole`] shape a keyless subscribe that
    /// runs into it stops there and the last export says `complete: false`; the
    /// answer is short and SAYS it is short, which is rule 2.8's fourth clause.
    const MAX_ROWS_PER_SUBSCRIBE: usize = 500;

    /// **THE MOST KEYS ONE MEMBERSHIP FRAME CARRIES.**
    ///
    /// Not a consumer dial — the chassis pages, emitting as many frames as the
    /// roster needs, each naming the subscribe and the last one saying
    /// `complete: true`. It is DECLARED so a consumer can read it off
    /// `shapes.list` and size its own buffer, which is the third clause of rule
    /// 2.8 applied to a ceiling nobody asks against.
    ///
    /// It is also the repair for `ShapeInfo::keys`, an unbounded `Vec<String>`
    /// already measured at 5,417 entries: a membership SUBSCRIPTION is the
    /// enumerator, and `shapes.list` stops pretending to be one.
    const MAX_KEYS_PER_FRAME: usize = 500;

    /// **THE MOST ITEMS OF A WINDOWED CELL ONE SUBSCRIBER WILL BE SHOWN** —
    /// rule 2.8's ceiling for the `retain` parameter. See
    /// [`crate::wire::Retain`], which carries the operator's words and the
    /// answer to who pays for a large ask.
    ///
    /// **`0` IS THE DEFAULT AND IT MEANS "THIS SHAPE HOLDS NO WINDOWED CELL".**
    /// Every shape written before `retain` existed keeps that default and every
    /// `retain: n` against one is refused [`crate::wire::ShapeError::NoRetention`],
    /// so nothing changes for a shape whose author has not thought about this —
    /// which is the same property [`Self::HYDRATE`] and `detail` were given.
    ///
    /// **A shape that declares this MUST override [`crate::fold::Root::row_at`]**,
    /// or it accepts an ask it then ignores. The chassis cannot check the
    /// pairing: `Root` exposes no cell types, for the same reason the
    /// `Counter`→`Eager` check cannot be written through it. Declare the number
    /// as the LARGEST capacity among the shape's windowed cells — it is the
    /// ceiling a consumer reads off `shapes.list` before it asks.
    const MAX_RETAINED_ITEMS: usize = 0;

    /// **THE SHORTEST INTERVAL, IN MILLISECONDS, BETWEEN TWO BEAT FRAMES ON ONE
    /// CONNECTION** — rule 2.8's ceiling for the `beat` parameter, and the FIRST
    /// ceiling on this chassis that bounds WORK PER UNIT TIME rather than a
    /// quantity. [`crate::wire::Beat`] states the six things that changes; the
    /// two that change the shape of THIS declaration are:
    ///
    /// * it is a FLOOR on a period, so a consumer beyond the ceiling asks for a
    ///   SMALLER number and the refusal cannot be
    ///   [`crate::wire::ShapeError::AboveMax`] — it is
    ///   [`crate::wire::ShapeError::TooFast`];
    /// * a quantity ceiling bounds the answer forever, and this one bounds no
    ///   total at all — a beating shape's output is bounded only by the
    ///   subscription ending.
    ///
    /// **`0` IS THE DEFAULT AND IT MEANS "THIS SHAPE DOES NOT BEAT".** Every
    /// shape written before `beat` existed keeps that default and every `beat: n`
    /// against one is refused [`crate::wire::ShapeError::NoBeat`], so nothing
    /// changes for an author who has not thought about this — the same property
    /// `detail` and `retain` were given.
    ///
    /// **A shape that declares this MUST actually be beaten**, by something that
    /// emits an input at this period; the chassis cannot beat it, because the
    /// chassis cannot mint a producer's input type. That pairing is unchecked for
    /// exactly the reason [`Self::MAX_RETAINED_ITEMS`]'s pairing with
    /// [`crate::fold::Root::row_at`] is unchecked, and the default of `0` is again
    /// what keeps the unpaired case UNREACHABLE rather than silent: a shape that
    /// declares no interval refuses every `beat` ask instead of accepting one it
    /// would then never honour.
    const MIN_BEAT_INTERVAL_MS: u64 = 0;

    /// **THE MOST KEYS OF THIS SHAPE ONE CONNECTION MAY HOLD OPEN AT ONCE** —
    /// rule 2.8's ceiling for the one subscriber parameter a [`Hydrate::PerKey`]
    /// shape had no ceiling for: the KEY.
    ///
    /// Every other ceiling on this trait bounds ONE answer.
    /// [`Self::MAX_ROWS_PER_SUBSCRIBE`] bounds how many rows a subscribe exports;
    /// [`Self::MAX_RETAINED_ITEMS`] bounds how much of a window one subscriber is
    /// shown. Neither bounds HOW MANY keys a connection asks for, and on a
    /// `PerKey` shape whose per-key cell is a window that is the number that
    /// decides the root's size: a consumer walking through threads opens one key
    /// per thread and closes none, so the producer's cost is set by how fast
    /// somebody navigates.
    ///
    /// **`None` IS THE DEFAULT AND IT MEANS "THIS SHAPE DECLARES NO SUCH
    /// CEILING"** — the same property [`Self::MAX_RETAINED_ITEMS`]' `0` and
    /// [`Self::MIN_BEAT_INTERVAL_MS`]' `0` have: nothing changes for a shape whose
    /// author has not thought about this, and the shape that HAS thought about it
    /// pays one declaration. It is `Option` and not `0`, because `0` on this axis
    /// is a REAL and meaningful ceiling (*"this shape serves no keyed subscribe at
    /// all"*) and must not be the spelling for *"unbounded"*.
    ///
    /// **It is enforced as a RESERVATION in [`Hub::subscribe`], never as a read in
    /// the task** — [`crate::wire::ShapeError::TooManyKeys`], on the request id,
    /// with nothing queued. That is the armed-timer cap's own repair applied
    /// before the same defect could be written a second time: a cap the task
    /// discovers runs against an item the RPC has already answered
    /// `{queued: true}`, and the consumer then waits forever on a subscription it
    /// was told it had.
    ///
    /// The slot it consumes is per `(connection, key)` and is held by a SET, so a
    /// re-subscribe to a key already open inserts nothing and refuses nothing.
    /// Rule 2.5 makes re-subscribe the remedy for every doubt a consumer can have
    /// about its mirror, and a ceiling that refused the remedy would be a ceiling
    /// that breaks recovery.
    const MAX_OPEN_KEYS_PER_CONN: Option<usize> = None;

    /// **HOW MANY ROWS A KEYLESS SUBSCRIBE ANSWERS WITH WHEN THE CONSUMER SAYS
    /// NOTHING** — the producer's link of the paging chain, *"informed by the
    /// size of the thing being paginated"*. See [`crate::wire::Paging`], which
    /// carries the operator's words and what a page token is under rule 2.5.
    ///
    /// **[`Paging::Default`] IS THE DEFAULT and it means the CHASSIS decides**:
    /// [`crate::wire::DEFAULT_PAGE_ROWS`], 100, capped at this shape's
    /// [`Self::MAX_ROWS_PER_SUBSCRIBE`]. That is a change in what a silent
    /// keyless subscribe receives ONLY on a shape whose pool exceeds 100 rows,
    /// and on such a shape the author chooses: declare [`Paging::Whole`] to keep
    /// serving the ceiling (rule 2.6, nothing a face reads moves), or accept the
    /// page and bump [`Self::VERSION`], because a keyless answer that shrank is a
    /// semantic change. [`Paging::Rows(n)`] is a producer default below the
    /// ceiling; `n == 0` or `n` above the ceiling is refused at [`Hub::register`].
    ///
    /// A consumer may always ask for more, up to the ceiling, or for `null` —
    /// the ceiling by name. Published resolved on `shapes.list` as
    /// [`ShapeInfo::page`], so no consumer needs to know the constant.
    const PAGE: Paging = Paging::Default;

    /// **THE SHAPE THIS ONE NESTS INSIDE**, by name, or `None` for a root shape.
    ///
    /// **Nesting is addressed by a path-shaped NAME and adds no subscribe
    /// parameter.** A nested shape is a shape: it has its own name
    /// (`"threads/messages"`), its own slot, its own task, its own FIFO, its own
    /// `Tracking` and its own `Hydrate`, and a consumer reaches it with the
    /// `{shape, key}` it already uses. Nothing in `Hub::subscribe`,
    /// `Hub::unsubscribe` or the frame types changed to allow it — `shapes` is
    /// matched by `s.name == shape` and a name with a `/` in it matches like any
    /// other. **A shape nobody subscribes to costs nothing** falls straight out of
    /// `Tracking::Lazy`, unchanged.
    ///
    /// What the child's KEYS are is the child's business, and the chassis does not
    /// compose them: `docs/chassis-resources.md` 1.1 describes the key space as
    /// `(parent_key, own_key)`, and how a producer spells that pair in one string
    /// is a producer decision (and part of its `VERSION`, rule 2.6). The chassis
    /// carries one opaque key per frame and always did.
    ///
    /// **This const is the DECLARATION, and `register` enforces it:** `PARENT:
    /// Some(p)` requires `NAME == "{p}/{leaf}"` with a non-empty leaf holding no
    /// `/`, and requires `p` to be registered ALREADY — so the tree is built
    /// parents-first and cannot contain an edge to a shape that does not exist.
    /// `PARENT: None` requires a name with no `/` at all, so a path-shaped name
    /// without a declared parent is refused rather than silently treated as a root.
    /// Both are panics at `register`, which runs in the producer's build function:
    /// a mis-declared tree fails the daemon's boot, not a consumer's subscribe.
    ///
    /// For the EPOCH semantics — what a parent's re-init does to its children, what
    /// a death does, and how a consumer tells whether two mirrors are from the same
    /// era — see [`Epoch::parent_epoch`], which carries the ruling and the argument
    /// for it. It is the decision here most likely to be wrong later.
    const PARENT: Option<&'static str> = None;

    /// **WHICH SHAPE EACH `Ref` IN THIS SHAPE'S MANIFEST RESOLVES TO** — the
    /// KEY-level nesting edge, as `(ref name, shape name)` pairs, published on
    /// `shapes.list` as [`ShapeInfo::refs`].
    ///
    /// [`Self::PARENT`] nests a SHAPE inside a shape. This nests a KEY inside a
    /// key: a row carrying `children: List(Ref("node"))` holds ids, and
    /// `REFERS: &[("node", "nodes")]` says they are keys of `nodes` — so a
    /// consumer walks the edge by subscribing to `nodes/{id}` for each one,
    /// which is the operator's *"graph traversal on nodes in a flat list"*. A
    /// row that carries a set of members carries their COUNT and their IDS,
    /// never their rows (`docs/shape-design-guide.md` step 3, and
    /// `threads/recent-updates`' *"two orderings and not two copies"*); this
    /// const is what makes the ids walkable by a machine rather than by
    /// convention.
    ///
    /// **`&[]` IS THE DEFAULT, and nothing changes for a shape that declares
    /// none** — `shapes.list` carries no `refs` key for it. A `Ref` whose name
    /// is NOT in this list is an id in a space no shape serves (a resource id),
    /// and that is a legal, positive statement.
    ///
    /// **[`Hub::register`] checks the declaration against the manifest**,
    /// panicking at the producer's boot like the nesting declaration: every ref
    /// name listed must be one the manifest ([`Self::CELLS`] or
    /// [`Self::SHARED`]) actually carries, once. **Whether the shape named
    /// exists is a GRAPH question and is answered by [`Hub::dangling_refs`]**,
    /// not at register: references have cycles (`accounts` rows name threads,
    /// `threads` rows name accounts), so no registration order can see both
    /// ends, and a producer's own test asserts the list empty once every shape
    /// is in. A self-reference (`threads` rows naming other `threads`) is the
    /// ordinary case rather than a special one.
    const REFERS: &'static [(&'static str, &'static str)] = &[];

    /// **EVERY PER-KEY CELL THIS SHAPE CARRIES — THE CELL MANIFEST.** The output
    /// twin of `declare_operation!`; see [`crate::cell`] for the hole it closes.
    ///
    /// **REQUIRED, WITH NO DEFAULT, and that is the one place this trait breaks
    /// its own "nothing changes for an author who has not thought about this"
    /// rule — because no default is TRUE.** Every other const here defaults to
    /// a statement that holds of a shape whose author never read the doc:
    /// `VERSION = 1` (no shape had ever bumped), `MAX_RETAINED_ITEMS = 0` (no
    /// windowed cell), `COUNTERS = &[]` (no counters). An empty cell list is not
    /// what any shape carries, and a chassis that defaulted it would publish a
    /// manifest nobody declared — which is the hole this const exists to close,
    /// wearing a new coat. So a shape that declares nothing does not compile,
    /// and rustc names what is missing: *not all trait items implemented,
    /// missing: `CELLS`*.
    ///
    /// Written with [`crate::declare_cells!`], which is the only spelling of a
    /// [`crate::cell::Cells`] and emits the compile-time checks beside it. Read
    /// ONCE at [`Hub::register`] into the published [`ShapeInfo::cells`] — the
    /// same terms as the ceilings and the contract, so the manifest on
    /// `shapes.list` cannot drift from the declaration.
    ///
    /// **The pairing with [`crate::fold::Root::cells`] is unchecked here**, for
    /// the reason every other pairing on this trait is: `Root` exposes no cell
    /// types. It is a test's job, one per producer, and
    /// `threads/tests/cell_manifest.rs` is the instance.
    const CELLS: crate::cell::Cells;

    /// **EVERY SHAPE-LEVEL CELL** — the manifest for the frame's `shared_cells`
    /// block, on [`Self::CELLS`]'s terms. A shape with no shape-level cell
    /// declares [`crate::cell::Cells::NONE`], which is a statement and not a
    /// default.
    const SHARED: crate::cell::Cells;

    /// **Every counter this shape can name, DECLARED so it is seeded at zero.**
    ///
    /// Without this a counter appears on `shapes.list` only after it first
    /// increments, because a flattened empty map emits no key — so
    /// `unplaced_status` would read as ABSENT on a healthy daemon and as `0` on
    /// one that had seen and recovered from an unplaced status. It was always
    /// present before the chassis moved. A field that appears only once
    /// something goes wrong is worse than one that is always zero: absent reads
    /// as "this daemon has no such counter", a different claim entirely.
    ///
    /// Declared rather than inferred from [`Self::counts`], because a function
    /// cannot be enumerated.
    const COUNTERS: &'static [&'static str] = &[];
    /// More derived events than this from ONE input trips the burst warning.
    const BURST: usize = 16;
    /// Per-key `row` coalescing window. Task machinery, never the fold.
    const DEBOUNCE_MS: u64 = 100;

    type Producer: Send + 'static;
    type Root: Root;

    /// Fresh, empty held state.
    fn root(&self) -> Self::Root;

    /// Ask the producer for current state. Runs on a blocking thread — the
    /// handlers are synchronous `rusqlite` reads and one keychain sample, ~0.8s
    /// in total, which must not sit on a tokio worker.
    fn hydrates(&mut self, p: &Self::Producer, root: &mut Self::Root, as_of: Timestamp);

    /// **HYDRATE ONE KEY, ON DEMAND.** Answers `true` if the key now exists in
    /// `root`, `false` to refuse.
    ///
    /// Called only on a shape declaring [`Hydrate::PerKey`], and only for a
    /// keyed subscribe naming a key the root does not already hold. **The default
    /// REFUSES**, which is what keeps every shape written before this method
    /// existed exactly as it was: a `Whole` shape never reaches this
    /// call at all, and a `PerKey` shape whose author has not written one gets
    /// a `warn!` and an export with no row rather than a silent nothing.
    ///
    /// The obligations are `hydrates`', per key: read once, through the same
    /// builder the live fact uses, and end by discarding the outbox your `key(..)`
    /// walk dirtied — a hydrate publishes nothing, the export is the publication.
    ///
    /// Runs on the SHAPE TASK, under the same `block_in_place` treatment `hydrates`
    /// gets. That is fine for a summary row (one indexed read) and is NOT fine for
    /// a content leaf: a large blocking read here stalls every subscriber of this
    /// shape. `docs/chassis-resources.md` 6.4 is where that case is designed and it
    /// is not built here.
    fn hydrates_key(
        &mut self,
        _p: &Self::Producer,
        _root: &mut Self::Root,
        _key: &str,
        _as_of: Timestamp,
    ) -> bool {
        false
    }

    /// **EVERY KEY THAT EXISTS IN THE POOL — NOT EVERY KEY HELD.**
    ///
    /// ONE STATEMENT (`SELECT id FROM threads`), never a row scan. That is the
    /// whole affordability argument: `threads` cannot hand anybody 5,417 rows
    /// (~20 MB) and can hand anybody 5,417 ids (~200 KB), so the question *what
    /// is in this pool* becomes answerable without the question *what is in
    /// each of these rows* becoming affordable.
    ///
    /// Called only on a [`Hydrate::PerKey`] shape, and only when a
    /// `detail: keys` subscribe arrives — NOT at init. `threads`' own module doc
    /// is emphatic that INIT READS NO THREAD, and a membership read at init
    /// would make every keyed subscribe pay a full id scan for a roster nobody
    /// asked for. Once per epoch, on demand, like everything else here.
    ///
    /// The obligations are [`Self::hydrates_key`]'s: write into the root — the
    /// membership set, through `Map::member` and `Map::mark_members_read` —
    /// read once, and end by discarding the outbox your walk dirtied, because a
    /// hydrate publishes nothing and the export is the publication.
    ///
    /// **Answers `true` if the root now carries a membership set, `false` to
    /// refuse.** A producer that genuinely cannot enumerate its namespace
    /// answers `false` after calling `Map::mark_members_not_hydrated(reason)`,
    /// and the membership export carries `NotHydrated{reason}` — the
    /// three-valued state, not a flag (`docs/access-patterns.md` 6.10).
    ///
    /// **The default REFUSES**, which is what keeps every shape written before
    /// this method existed exactly as it was. A [`Hydrate::Whole`] shape never
    /// reaches this call at all: its members ARE its held keys, which
    /// `Root::members`' default already says.
    fn hydrates_members(
        &mut self,
        _p: &Self::Producer,
        _root: &mut Self::Root,
        _as_of: Timestamp,
    ) -> bool {
        false
    }

    /// Fold one input. **Reads no clock, no store and no producer.**
    fn folds(&mut self, input: &Stamped<I>, tx: &mut Tx<'_, Self::Root>);

    /// **A shape-specific counter for this input, by name, or `None`.**
    ///
    /// The chassis owns the MECHANISM and none of the names. It exists because
    /// `unplaced_status` was an accounts-shape counter sitting in the generic
    /// `ShapeInfo` (docs/shapes.md 6.2, coupling 3) — but unlike the doc's
    /// reading it is live, incremented, and read by the board, so the fix is to
    /// make the chassis generic rather than to delete the field.
    ///
    /// Counted by the TASK, not the fold: `folds` gets a `Tx` over the root and
    /// cannot see `ShapeInfo`. Counted BEFORE the pre-init return, so the number
    /// does not quietly depend on init timing.
    fn counts(&self, _input: &Stamped<I>) -> Option<&'static str> {
        None
    }
}

/// The per-input transaction. Holds the stamp so no fold has to invent one.
pub struct Tx<'a, R: Root> {
    root: &'a mut R,
    at: Timestamp,
}

impl<'a, R: Root> Tx<'a, R> {
    pub fn new(root: &'a mut R, at: Timestamp) -> Self {
        Self { root, at }
    }
    /// **The only time a fold ever sees.** It is the producer's stamp on the
    /// input, taken once at emit — not a clock read.
    pub fn at(&self) -> Timestamp {
        self.at
    }
}

impl<R: Root> std::ops::Deref for Tx<'_, R> {
    type Target = R;
    fn deref(&self) -> &R {
        self.root
    }
}

impl<R: Root> std::ops::DerefMut for Tx<'_, R> {
    fn deref_mut(&mut self) -> &mut R {
        self.root
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. The FIFO
// ─────────────────────────────────────────────────────────────────────────────

/// **THE ID THAT MAKES A SUBSCRIBE MATCHABLE TO THE EXPORT THAT SATISFIES IT** —
/// rule 2.7's `exported` stage, minted by [`Hub::subscribe`].
///
/// Its own id space, not the producer's [`crate::command::PromiseBook`]'s: a
/// command promise and a subscribe are two different acts, the chassis owns one
/// and the producer owns the other, and a consumer holding id 5 must never have to
/// guess which of its own outstanding things it has been told about. See
/// [`crate::wire::ShapeWire::subscribes`].
pub type SubscribeId = u64;

/// What a consumer asked for.
#[derive(Debug, Clone)]
pub struct SubscribeItem {
    pub conn: ConnId,
    pub key: Option<String>,
    /// **WHICH `shapes.subscribe` CALL THIS IS**, echoed onto every export the
    /// item causes. The chassis's own lag-repair re-subscribe mints one too and
    /// nothing reads it — which is correct: an export a consumer did not ask for
    /// still names the internal call that produced it, and a consumer matching its
    /// own ids simply does not recognise that one.
    pub id: SubscribeId,
    /// Absent → snapshot now. In the FUTURE → the task arms a timer and
    /// snapshots then, which is how a consumer clocks itself. **In the PAST →
    /// treated as absent** (operator ruling 4): cells hold no history, so
    /// "the snapshot reflects exactly events <= at" is unimplementable, and
    /// `as_of` reports the REAL fold point rather than lying with `at`.
    pub at: Option<Timestamp>,
    /// **HOW MUCH OF EACH KEY THIS SUBSCRIBE ASKED FOR** — see
    /// [`crate::wire::Detail`]. `Rows` is the ceiling and the default, so every
    /// subscribe written before the parameter existed is unchanged.
    pub detail: Detail,
    /// **HOW MUCH OF EACH WINDOWED CELL THIS SUBSCRIBE ASKED FOR** — see
    /// [`crate::wire::Retain`]. `Some(Max)` is the ceiling and the default for a
    /// CONSUMER'S ask.
    ///
    /// It is recorded PER CONNECTION on the task, not per `(conn, key)`: a
    /// keyless subscribe is one ask covering every key and has no per-key
    /// retain to record, so a second subscribe on this shape from the same
    /// connection replaces the first one's retain.
    ///
    /// # `None` MEANS *THIS IS NOT AN ASK* — and that is not the same as `Max`
    ///
    /// **THE DEFECT THIS FIXES (2026-10-02).** The chassis re-subscribes on its
    /// own account when a connection's frame channel overflows, and it did that
    /// through [`Hub::subscribe`], whose whole contract is *asking nothing means
    /// the ceiling* (rule 2.8). So the repair arrived at [`ShapeTask::
    /// do_subscribe`] carrying `Retain::Max` and `Beat::Max`, which that function
    /// then wrote over the connection's recorded ask — deliberately, because for a
    /// CONSUMER'S second subscribe the last ask is the current ask. The result was
    /// that a consumer which had negotiated `retain: 2` and `beat: 5000ms`, and
    /// then fell behind, was silently UPGRADED to maximum retention and maximum
    /// beat rate: more bytes per frame and more frames per second, for precisely
    /// the consumer that had just proved it could not keep up. A repair that makes
    /// the fault worse the harder it bites is a positive feedback loop, and it is
    /// invisible from the consumer's side because nothing refuses and nothing logs.
    ///
    /// Rule 2.8's *asking nothing means the ceiling* is a rule about a
    /// SUBSCRIBER'S REQUEST. The chassis's lag repair is not a request — nobody
    /// asked for it — so it has no ask to default, and the honest spelling for
    /// that is a third value rather than the ceiling. `None` is that value:
    /// `do_subscribe` leaves [`ShapeTask::retains`] and [`ShapeTask::beats`]
    /// exactly as the consumer negotiated them.
    pub retain: Option<Retain>,
    /// **HOW OFTEN THIS SUBSCRIBE ASKED TO BE BEATEN** — see
    /// [`crate::wire::Beat`]. `Some(Max)` is the ceiling and the default for a
    /// consumer's ask; `None` is the chassis's own repair, which is not an ask at
    /// all. See [`Self::retain`] for the whole argument — it is the same one, and
    /// it bites harder on this axis, because the thing being handed to a consumer
    /// that cannot keep up is a FASTER CADENCE.
    ///
    /// Recorded per CONNECTION on the task, like [`Self::retain`], and for a
    /// stronger reason: a beat is a fact about the LINK, not about a key, so a
    /// per-`(conn, key)` cadence would be answering a question nobody asked.
    pub beat: Option<Beat>,
    /// **WHETHER THIS LINK IS SERVED ON A SUPERSEDED CONTRACT** — rule 2.6's
    /// middle state, decided by [`Hub::subscribe_as`] and carried here so the
    /// task can stamp it onto every export this connection receives.
    ///
    /// `None` is BOTH *asked for the current version* and *stated no version*, and
    /// that collapse is deliberate: the only thing the delivery path needs is the
    /// warning or its absence, and two spellings for the same frame is the
    /// collapse every repair in this file is against. The distinction between
    /// those two states is a LEDGER fact and a [`crate::wire::Link`], both of
    /// which keep it.
    ///
    /// **`None` here means what it means for [`Self::retain`]: NOT AN ASK.** The
    /// chassis's own lag repair states no version, so it arrives `None` and
    /// `do_subscribe` writes nothing — the negotiated warning stands. A consumer
    /// that subscribes again STATING a version does replace it, because that is a
    /// new claim about the link. See [`ShapeTask::superseded`].
    pub superseded: Option<crate::wire::Superseded>,
    /// **HOW MANY ROWS A KEYLESS ASK ANSWERS WITH**, already resolved through
    /// the chain and already checked against the ceiling at the door — see
    /// [`crate::wire::Paging`]. Meaningless on a keyed subscribe.
    pub page: usize,
    /// **THE KEY TO START FROM**, inclusive, in key order; `None` is the
    /// first key. The consumer's own token: the last page's `next`, verbatim.
    pub from: Option<String>,
}

#[derive(Debug)]
pub enum Item<I> {
    Input(Stamped<I>),
    Subscribe(SubscribeItem),
    Unsubscribe { conn: ConnId, key: Option<String> },
    Dropped(ConnId),
}

/// A `VecDeque` under a mutex rather than an `mpsc`, for one reason: producer
/// events keep STRICT ARRIVAL ORDER and control items are appended to the same
/// queue, so an export is placed among the events it must reflect rather than
/// racing them on a side channel. (Rev 3 also placed past-stamped control items
/// mid-queue; ruling 4 deleted that, so `push_control` is now a plain
/// `push_back` — see [`SubscribeItem::at`].)
pub struct Fifo<I> {
    q: StdMutex<VecDeque<Item<I>>>,
    notify: Notify,
    cap: usize,
    /// Inputs dropped for capacity since the task last swapped it to zero.
    /// Swap-to-zero (not a saturating read) is what stops a re-init loop under
    /// sustained overload.
    lost: AtomicU64,
    high_water: AtomicUsize,
    tracking: AtomicBool,
}

impl<I> Fifo<I> {
    pub fn new(cap: usize) -> Self {
        Self {
            q: StdMutex::new(VecDeque::new()),
            notify: Notify::new(),
            cap,
            lost: AtomicU64::new(0),
            high_water: AtomicUsize::new(0),
            tracking: AtomicBool::new(false),
        }
    }

    pub fn tracking(&self) -> bool {
        self.tracking.load(Ordering::Acquire)
    }

    pub fn set_tracking(&self, on: bool) {
        self.tracking.store(on, Ordering::Release);
    }

    /// **The producer never blocks on a shape.** A full queue drops the event
    /// for THIS shape only and counts it — the same choice the broadcast makes.
    pub fn push_input(&self, item: Stamped<I>) {
        if !self.tracking() {
            return;
        }
        let mut q = self.q.lock().unwrap();
        if q.len() >= self.cap {
            self.lost.fetch_add(1, Ordering::Relaxed);
            return;
        }
        q.push_back(Item::Input(item));
        self.high_water.fetch_max(q.len(), Ordering::Relaxed);
        drop(q);
        self.notify.notify_one();
    }

    /// Control items are NEVER dropped for capacity — a subscribe that fell on
    /// the floor is a consumer that waits forever.
    pub fn push_control(&self, item: Item<I>) {
        let mut q = self.q.lock().unwrap();
        q.push_back(item);
        self.high_water.fetch_max(q.len(), Ordering::Relaxed);
        drop(q);
        self.notify.notify_one();
    }

    pub async fn recv(&self) -> Item<I> {
        loop {
            // Register interest BEFORE the check: `Notify` stores a permit, so
            // a push between the check and the await is not lost.
            let n = self.notify.notified();
            if let Some(i) = self.q.lock().unwrap().pop_front() {
                return i;
            }
            n.await;
        }
    }

    pub fn try_pop(&self) -> Option<Item<I>> {
        self.q.lock().unwrap().pop_front()
    }

    pub fn len(&self) -> usize {
        self.q.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn take_lost(&self) -> u64 {
        self.lost.swap(0, Ordering::Relaxed)
    }

    pub fn high_water(&self) -> usize {
        self.high_water.load(Ordering::Relaxed)
    }

    pub fn clear(&self) {
        self.q.lock().unwrap().clear();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. The introspection frame's constructors, and the runtime's constants
// ─────────────────────────────────────────────────────────────────────────────
//
// The frame TYPES moved to [`crate::wire`]; a consumer parses every one of them
// and starts no runtime. What is left here is the producer side of one of them:
// [`ShapeInfo`] is built from a [`Tracking`] and a shape's declared counters,
// both of which are hub concepts.

impl ShapeInfo {
    /// **WHAT A SHAPE DECLARES, AS `shapes.list` WILL PUBLISH IT, BEFORE IT HAS
    /// RUN** — every `const` on [`Shape`] read once, into the frame.
    ///
    /// This is the ONE place the declaration is read. [`Hub::register`] seeds
    /// the task's published info from it, so the ceilings a refusal names, the
    /// contract a link is checked against and the cell manifest a consumer
    /// routes its reads against are all the same read of the same consts; and
    /// a producer's offline inventory tool (`threads`' `shapes-manifest` bin)
    /// reads it too, so the inventory a consumer vendors is this exact value
    /// with no daemon in the room.
    ///
    /// The runtime fields — `keys`, `subscribers`, `epoch`, `fifo_high_water`,
    /// `bursts`, `tracked`, `consumers`, `parent` — are at their fresh values:
    /// this is the declaration, and the hub fills the rest in `list`.
    ///
    /// **Rule 2.6's declaration checks run HERE**, so a mis-declared `VERSION`
    /// or `SERVES` panics wherever the declaration is read: at `register`,
    /// which is the producer's boot, and in the inventory tool, which is a
    /// reviewer's diff. Both are before any consumer links.
    pub fn declared_by<I, S: Shape<I>>() -> Self {
        assert!(
            S::VERSION > 0,
            "shape `{}` declares VERSION 0. Zero is reserved so that `I did not \
             think about this` has no spelling on the wire — a consumer reading \
             `contract.version` must be reading a declaration somebody made, not \
             the absence of one.",
            S::NAME
        );
        {
            let mut floor = S::VERSION;
            for v in S::SERVES {
                assert!(
                    *v < floor,
                    "shape `{}` declares SERVES {:?} against VERSION {}: every \
                     entry must be strictly below VERSION and the list strictly \
                     descending. `{}` breaks that, so the published `serves` list \
                     would either repeat the current version or go backwards, and \
                     a contract a consumer reads has to have one reading.",
                    S::NAME,
                    S::SERVES,
                    S::VERSION,
                    v
                );
                floor = *v;
            }
        }
        // **THE CEILINGS, READ OFF THE SHAPE ONCE** — rule 2.8. They are consts
        // and not runtime config, so this is the only place they are ever read
        // and the value on `shapes.list` cannot drift from the value a refusal
        // names.
        let max = ShapeMax {
            rows_per_subscribe: S::MAX_ROWS_PER_SUBSCRIBE,
            keys_per_frame: S::MAX_KEYS_PER_FRAME,
            future_at_secs: MAX_FUTURE_AT.num_seconds(),
            armed_timers_per_conn: MAX_ARMED_TIMERS_PER_CONN,
            retained_items: S::MAX_RETAINED_ITEMS,
            beat_interval_ms: S::MIN_BEAT_INTERVAL_MS,
            open_keys_per_conn: S::MAX_OPEN_KEYS_PER_CONN,
        };
        // **CURRENT FIRST, THEN THE OLDER ONES.** This is the list the refusal
        // quotes and the list `shapes.list` publishes — ONE list, built once, so
        // `ShapeError::VersionRefused` cannot name a set the contract does not.
        let contract = ShapeContract {
            version: S::VERSION,
            serves: std::iter::once(S::VERSION)
                .chain(S::SERVES.iter().copied())
                .collect(),
        };
        let mut info = Self::fresh(S::NAME, S::TRACKING, max, contract);
        for c in S::COUNTERS {
            info.counters.insert((*c).to_string(), 0);
        }
        // **THE CELL MANIFEST, READ OFF THE SHAPE ONCE**, on the ceilings' terms.
        // Its well-formedness was asserted at compile time by `declare_cells!`;
        // nothing here can be malformed, so nothing here is re-checked.
        info.cells = S::CELLS.publish();
        info.shared_cells = S::SHARED.publish();
        for (r, target) in S::REFERS {
            info.refs.insert((*r).to_string(), (*target).to_string());
        }
        info.page = Some(crate::wire::PageContract {
            default: match S::PAGE {
                Paging::Whole => None,
                declared => Some(Paging::resolve(
                    Paging::Default,
                    declared,
                    S::MAX_ROWS_PER_SUBSCRIBE,
                )),
            },
            max: S::MAX_ROWS_PER_SUBSCRIBE,
        });
        info
    }

    fn fresh(name: &str, tracking: Tracking, max: ShapeMax, contract: ShapeContract) -> Self {
        Self {
            name: name.to_string(),
            // The hub fills this in `list`, from the slot's declaration, for the
            // same reason it fills `consumers` there: it is the hub's state and not
            // the task's, and nothing may read one from outside the other.
            parent: None,
            refs: BTreeMap::new(),
            page: None,
            keys: Vec::new(),
            max,
            // **RULE 2.6's CONTRACT, SEEDED HERE LIKE `max` IS.** It is a
            // declaration and never moves, so it is passed in once rather than
            // filled by `Hub::list` the way `parent` and `consumers` are: those
            // are the hub's live state, this is the shape's own statement about
            // itself and the task publishes it unchanged on every `publish`.
            contract,
            tracking: tracking.as_str().to_string(),
            tracked: false,
            subscribers: 0,
            epoch: Epoch::default(),
            fifo_high_water: 0,
            bursts: 0,
            counters: BTreeMap::new(),
            consumers: Vec::new(),
            // Filled by `declared_by`, which is the only constructor a shape
            // reaches this through.
            cells: Vec::new(),
            shared_cells: Vec::new(),
        }
    }
}

/// The bound on a future `at` (ruling 4). A consumer clocking itself asks for
/// seconds or minutes ahead; a day is already absurd, and anything larger
/// overflows tokio's `Instant` conversion — which would panic the task and trip
/// the death path for a reason that is really a bad request.
pub const MAX_FUTURE_AT: ChronoDuration = ChronoDuration::hours(24);
/// Per-connection cap on armed timers, so a consumer looping
/// `shapes.subscribe {at: +1h}` cannot grow the side-list without limit.
pub const MAX_ARMED_TIMERS_PER_CONN: usize = 4;
/// The floor between re-inits, so sustained overflow cannot livelock the task.
pub const REINIT_FLOOR_SECS: i64 = 5;

/// **HOW EARLY A BEAT MAY ARRIVE AND STILL COUNT AS ON TIME**, in milliseconds.
///
/// A rate gate comparing two clock readings against a period drops one beat in N
/// for free when the producer's own emit period and the consumer's asked period
/// are the SAME number: the emit jitters a millisecond late, the gate measures a
/// millisecond short, and every such beat is withheld until the next one — which
/// halves the cadence a consumer was promised. A quantity projection has no
/// equivalent, because `min(n, held)` has no clock in it.
///
/// So the comparison is `elapsed + slack >= want`, and the slack is a tenth of
/// the fastest period any shape here declares. It is a bound on the gate's
/// PRECISION and not a bound on the contract: the consumer's guarantee is "no
/// faster than the producer's period", and a frame 100 ms early against a
/// 5,000 ms ask still honours it.
pub const BEAT_SLACK_MS: u64 = 100;
/// Default FIFO capacity. The ledger's real peak is ~50 events/s (measured:
/// 3,034 in one minute on 2026-09-13), so 1,024 is ~20s of peak — comfortably
/// more than a 0.8s init needs.
pub const DEFAULT_FIFO_CAPACITY: usize = 1024;
/// **ONE CONNECTION'S FRAME BUFFER.**
///
/// Deliberately the same 1,024 the single shared `broadcast` had, so that no
/// connection is worse off than it was; the change is WHOSE buffer a frame is
/// charged to, not how much buffer there is. See [`Hub::connect`].
pub const CONN_FRAME_CAPACITY: usize = 1024;

/// **HOW LONG A RESOLVED PROMISE ID IS STILL OFFERED ON AN EXPORT** — the window
/// that makes rule 2.5's *"re-subscribe must be cheap and COMPLETE"* true for
/// promises as well as for cells. See [`ShapeTask::carried`].
///
/// 120 s, and the number is bounded from both ends by what it is for. The low end:
/// it must cover a socket dying, a client noticing, reconnecting and
/// re-subscribing, which is the board's own reconnect loop and is seconds. The high
/// end: an id nobody claims is dead weight, and a consumer that has been gone two
/// minutes has re-subscribed and re-read everything anyway, so re-offering an
/// answer to a command it issued before that is answering a question nobody is
/// still asking.
pub const PROMISE_RETENTION: ChronoDuration = ChronoDuration::seconds(120);

/// **THE HARD CAP PER SUBJECT**, so the window is not the only bound. A producer
/// that resolves promises faster than a consumer reconnects must not grow a list
/// per key; the oldest ids go first, which is the right end because the newest
/// command is the one a reconnecting consumer is most likely still waiting on.
pub const PROMISE_RETENTION_PER_KEY: usize = 64;

// ─────────────────────────────────────────────────────────────────────────────
// 8b. Per-connection frame delivery
// ─────────────────────────────────────────────────────────────────────────────

/// **A CONNECTION'S OWN FRAME CHANNEL, AND THAT IS THE POINT.**
///
/// The precedent is the chassis's own, one level up: shape frames left the
/// producer's legacy firehose for a channel of their own because one
/// `broadcast::channel` counts every send against every receiver, so a
/// shape-heavy consumer would otherwise push a legacy connection into `Lagged`.
/// Exactly the same argument one level down says a shape frame addressed to ONE
/// connection must not be charged to every other connection's buffer.
///
/// **What it was.** One `broadcast::Sender<Arc<ShapeFrame>>` at capacity 1,024,
/// shared by every connection, with `ShapeFrame::to` examined in the producer's
/// delivery loop AFTER the send. So an export directed at connection 7 consumed
/// one slot in connection 3's buffer, which connection 3 had to drain to avoid
/// lagging — and a lag's repair is a re-subscribe of every key it holds, which is
/// N more sends into everyone's buffer again. Re-init did it unprompted, once per
/// key, to every subscriber of every shape on the producer.
///
/// **What it is.** One bounded `mpsc` per connection, created at its first
/// subscribe and dropped with it. A frame reaches a connection's buffer only if
/// that connection is a possible recipient, so a directed export costs exactly
/// one slot in exactly one buffer.
///
/// **WHY THE BROADCAST IS GONE RATHER THAN KEPT FOR `to: None` FRAMES**, which is
/// where the design (`docs/chassis-resources.md` 7.2) left the line. Because the
/// chassis promises a TOTAL ORDER per connection, and two channels cannot keep
/// one. `ShapeTask::export`'s own contract is "ONE task emits both the derived
/// events and the export, and ONE receiver per connection delivers them in order,
/// so the consumer rule is one line — hold the export, apply everything after
/// it." Split exports onto a second channel and the delivery loop has two
/// buffered frames with no way to recover which was sent first: an export built
/// at t1 delivered after a transition emitted at t2 either drops the transition
/// (the gate has not opened yet) or duplicates it (it is already inside the
/// export's retained items), and a `row` at t1 applied after an export at t2
/// walks the mirror backwards. Recovering the order needs a watermark neither
/// channel carries. So every frame for a connection goes down that connection's
/// one channel, and the order the task sent them in is the order it reads them.
///
/// The cost, stated rather than sold as free: a keyless frame now costs one send
/// per subscribed connection instead of one send total. That is the arithmetic
/// isolation buys, it is linear in a number that is 1–3 today, and the sends it
/// replaces were charged to those same connections anyway.
pub struct ConnFrames {
    conn: ConnId,
    rx: mpsc::Receiver<Arc<ShapeFrame>>,
    lost: Arc<AtomicU64>,
}

impl ConnFrames {
    pub fn conn(&self) -> ConnId {
        self.conn
    }

    /// The next frame for this connection, in the order the producer sent it.
    /// `None` once the hub has dropped this connection's sink.
    pub async fn recv(&mut self) -> Option<Arc<ShapeFrame>> {
        self.rx.recv().await
    }

    /// The next frame if one is already buffered. For draining what has arrived
    /// so far without waiting — a delivery loop wants [`Self::recv`].
    pub fn try_recv(&mut self) -> Result<Arc<ShapeFrame>, mpsc::error::TryRecvError> {
        self.rx.try_recv()
    }

    /// **HOW MANY FRAMES WERE DROPPED FOR CAPACITY, AND RESET.**
    ///
    /// The `mpsc` twin of `broadcast`'s `Lagged`, and it exists for the same
    /// reason: a consumer can only recover from a gap it is told about. A full
    /// channel is full precisely because there are frames to read, so a caller
    /// that checks this after each `recv` learns about the gap promptly — and the
    /// repair is the one that already existed, a re-subscribe of every key this
    /// connection holds, now scoped to the one connection and costing nobody else.
    pub fn take_lost(&self) -> u64 {
        self.lost.swap(0, Ordering::Relaxed)
    }
}

/// The producer's half of one connection's channel.
struct ConnSink {
    tx: mpsc::Sender<Arc<ShapeFrame>>,
    lost: Arc<AtomicU64>,
}

// ─────────────────────────────────────────────────────────────────────────────
// 9. The hub
// ─────────────────────────────────────────────────────────────────────────────

struct ShapeSlot<I> {
    name: &'static str,
    fifo: Arc<Fifo<I>>,
    info: watch::Receiver<ShapeInfo>,
    /// Ruling 1: set by the supervisor when the task's `JoinHandle` resolves.
    dead: Arc<StdMutex<Option<String>>>,
    lazy: bool,
    /// **DECLARED, so a refusal can be decided where the request still IS a
    /// request.** The keyless-subscribe rule for [`Hydrate::PerKey`] needs
    /// nothing but the const, so it is answered on the request id rather than
    /// discovered by the task after `{queued: true}` went out.
    hydrate: Hydrate,
    /// **THIS SHAPE'S CEILINGS, ON THE SLOT** — rule 2.8. Held here for the
    /// same reason `hydrate` is: a refusal that names a ceiling has to be
    /// decidable where the request is still a request, and `Hub` is not generic
    /// over `S`.
    max: ShapeMax,
    /// **THIS SHAPE'S VERSION CONTRACT, ON THE SLOT** — rule 2.6, held here for
    /// exactly the reason [`Self::max`] is: the link refusal has to be decidable
    /// where the request is still a request, `Hub` is not generic over `S`, and
    /// the number the refusal quotes must be the number `shapes.list` publishes
    /// rather than a second read of the const.
    contract: ShapeContract,
    /// This shape's declared parent — see [`Shape::PARENT`].
    parent: Option<&'static str>,
    /// This shape's declared `Shape::PAGE`, the producer's link of the paging
    /// chain, resolved against a consumer's ask at the door.
    page: Paging,
    /// **THIS SHAPE'S `reinit` COUNT, VISIBLE FROM THE HUB.**
    ///
    /// Shared with the task, which is the only writer. It exists so a CHILD's task
    /// can read its parent's era without either task knowing the other exists:
    /// the child holds a clone of this `Arc` and stamps it onto its own `Epoch` at
    /// init. One relaxed atomic load, no message between tasks, and no way for one
    /// task to make another do work — which is the whole of the independence
    /// ruling in [`crate::wire::Epoch::parent_epoch`] expressed as a mechanism.
    reinit_gen: Arc<AtomicU64>,
    /// **ARMED FUTURE-`at` TIMERS PER CONNECTION, VISIBLE FROM THE HUB.**
    ///
    /// Shared with the task, and it exists so [`MAX_ARMED_TIMERS_PER_CONN`] can
    /// be enforced where the request is still a REQUEST — see
    /// [`Hub::subscribe`]. The hub RESERVES a slot here before enqueuing; the
    /// task releases when the timer fires, and clears the connection's whole
    /// entry on unsubscribe or drop.
    armed_timers: Arc<StdMutex<HashMap<ConnId, usize>>>,
    /// **KEYS HELD OPEN PER CONNECTION, VISIBLE FROM THE HUB** — the reservation
    /// behind [`Shape::MAX_OPEN_KEYS_PER_CONN`].
    ///
    /// A SET of keys and not a count, for two reasons that are one reason: a
    /// re-subscribe to a key already held must not consume a second slot (rule
    /// 2.5's remedy cannot be the thing a ceiling refuses), and a release must be
    /// able to name which key went away. A count could do neither.
    ///
    /// Shared with the task exactly as [`Self::armed_timers`] is: the hub takes a
    /// slot under the lock before enqueuing, the task gives it back on a keyed
    /// unsubscribe and clears the connection's whole entry when it drops.
    open_keys: Arc<StdMutex<HashMap<ConnId, BTreeSet<String>>>>,
}

/// **The one emit point.** Every producer emit goes through here, so the stamp
/// is applied in exactly one place and every shape sees every input in arrival
/// order.
///
/// Generic over `I`, the producer's input type. The chassis knows only that it
/// can be cloned, sent, and asked for a coarse [`Kind::kind`] name for the burst
/// warning — `threads` defines its own `Input` in `event.rs` and keeps its
/// legacy firehose alongside this hub rather than inside it.
pub struct Hub<I> {
    /// **ONE BOUNDED CHANNEL PER CONNECTION, and that is the point.** The
    /// chassis kept its own `broadcast` so a shape-heavy consumer could not lag
    /// a legacy one on the producer's firehose; the same argument one level down
    /// says a frame addressed to one connection must not be charged to every
    /// other connection's buffer. See [`ConnFrames`], which carries the whole
    /// reasoning including why the broadcast is gone rather than kept for
    /// undirected frames.
    conns: RwLock<HashMap<ConnId, ConnSink>>,
    shapes: RwLock<Vec<ShapeSlot<I>>>,
    /// **THE CONSUMER LEDGER** — who is subscribed, and the connect/disconnect
    /// history under `~/.cauldron/`. A CHASSIS concern: it knows nothing about
    /// any particular shape and would behave identically for one not yet
    /// written. See [`crate::ledger`].
    ///
    /// Its registry half works unconditionally; its FILE half is wired once, by
    /// the daemon entry point, so a `ThreadManager` built by a CLI verb or a
    /// test serves no consumers and writes no ledger.
    pub consumers: crate::ledger::ConsumerLedger,
    /// **ONE CONNECTION-ID SPACE PER HUB, AND THE HUB IS WHERE IT HAS TO LIVE.**
    ///
    /// A `ConnId` is minted per socket and is the address a shape export is
    /// delivered to (`ShapeFrame.to`), the key `dropped` closes subscriptions
    /// under, and the identity the consumer ledger records. It was minted by a
    /// `static AtomicU64` inside the SERVER — which is correct for exactly as
    /// long as there is only one server.
    ///
    /// There are now two: the chassis server stands up beside `threads`' own
    /// during the migration, over the SAME hub. Two statics each starting at 1
    /// would have handed both servers' first connection the id 1, and a frame
    /// addressed `to: Some(1)` would have reached whichever of them asked — a
    /// consumer receiving another consumer's export, which is the one thing the
    /// gating in `drive` exists to make impossible. The id space belongs to the
    /// thing that interprets the ids.
    next_conn: std::sync::atomic::AtomicU64,
    /// **THE SUBSCRIBE ID SPACE** — see [`SubscribeId`]. One per hub, for the same
    /// reason `next_conn` is: two servers serve one hub during the API's move, and
    /// a counter per server would hand two callers the same id over one id space.
    next_subscribe: std::sync::atomic::AtomicU64,
    /// **THE ONE CLOCK [`Hub::emit`] READS** — `chrono::Utc::now` unless
    /// [`Hub::with_clock`] says otherwise. A field and not a hard-coded call so
    /// that "`emit` reads the clock ONCE per fan-out" is a COUNT a test can take
    /// (R3.S2.10, HOLDS R8 / Q-22-5): the old guard compared two real clock
    /// reads and was only as strong as the microseconds between them.
    clock: fn() -> Timestamp,
}

impl<I: Kind + Clone + Send + Sync + 'static> Default for Hub<I> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: Kind + Clone + Send + Sync + 'static> Hub<I> {
    pub fn new() -> Self {
        Self {
            conns: RwLock::new(HashMap::new()),
            shapes: RwLock::new(Vec::new()),
            consumers: crate::ledger::ConsumerLedger::new(),
            next_conn: std::sync::atomic::AtomicU64::new(1),
            next_subscribe: std::sync::atomic::AtomicU64::new(1),
            clock: chrono::Utc::now,
        }
    }

    /// **Replace the clock [`Hub::emit`] stamps with.** Only `emit` reads it:
    /// `emit_at` and `emit_resolving` carry the caller's stamp, and the hub's
    /// other clock reads (`as_of` on exports, idle times) are not input stamps.
    /// For a test that must COUNT the reads or see a stamp no real clock could
    /// produce; a producer has no reason to call it.
    pub fn with_clock(mut self, clock: fn() -> Timestamp) -> Self {
        self.clock = clock;
        self
    }

    /// Mint the next connection id. One space per hub — see [`Hub::next_conn`]'s
    /// field for why the server may not own this counter.
    pub fn next_conn(&self) -> ConnId {
        self.next_conn
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// **OPEN THIS CONNECTION'S FRAME CHANNEL.**
    ///
    /// Call it BEFORE the first `shapes.subscribe` item is enqueued, or the
    /// export that subscribe causes is emitted into a channel nobody is holding —
    /// the same ordering the shared broadcast needed and for the same reason.
    ///
    /// Calling it twice for one `ConnId` replaces the sink, which abandons
    /// whatever the previous receiver had not drained. That is a caller error and
    /// not a supported reconnect: connection ids are minted per socket and never
    /// reused, so a second call for one id means two delivery loops believe they
    /// are the same connection.
    pub fn connect(&self, conn: ConnId) -> ConnFrames {
        let (tx, rx) = mpsc::channel(CONN_FRAME_CAPACITY);
        let lost = Arc::new(AtomicU64::new(0));
        self.conns.write().unwrap().insert(
            conn,
            ConnSink {
                tx,
                lost: lost.clone(),
            },
        );
        ConnFrames { conn, rx, lost }
    }

    /// **ONE FRAME TO ONE CONNECTION.** A directed export costs one slot in one
    /// buffer, which is the whole of the capacity-isolation argument.
    ///
    /// `try_send`, never `send().await`: the shape task must not wait on a
    /// consumer's drain. Rule 2.3 — "a slow consumer can only ever hurt itself" —
    /// is a statement about the PRODUCER'S shared machinery, and the task is
    /// shared by every subscriber of the shape. A full channel therefore drops the
    /// frame and counts it, and [`ConnFrames::take_lost`] is how the connection
    /// finds out and repairs itself.
    fn deliver(&self, conn: ConnId, frame: &Arc<ShapeFrame>) {
        let conns = self.conns.read().unwrap();
        let Some(sink) = conns.get(&conn) else {
            // No channel: this connection never subscribed, or has already gone.
            // Both are ordinary.
            return;
        };
        if sink.tx.try_send(Arc::clone(frame)).is_err() {
            sink.lost.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Every connection that has a channel, whatever it is subscribed to.
    ///
    /// The ONE caller is the supervisor's death export, and it is the one frame
    /// the producer emits from OUTSIDE the shape task — the task's `subs` set is
    /// gone by then, so there is nothing left to filter by. The delivery loop's
    /// own gate ("does this connection hold any key of this shape") is what makes
    /// that safe, and it is the same gate the shared broadcast relied on for the
    /// same frame.
    fn deliver_to_all(&self, frame: &Arc<ShapeFrame>) {
        let conns = self.conns.read().unwrap();
        for (_, sink) in conns.iter() {
            if sink.tx.try_send(Arc::clone(frame)).is_err() {
                sink.lost.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Stamp now, fan out.
    pub fn emit(&self, value: I) {
        self.emit_at((self.clock)(), value);
    }

    /// **Stamp with an `at` the CALLER took before it read the store.**
    ///
    /// Ruling 3, from the critique's F2: `emit_status` does five store reads via
    /// `thread_status` and only THEN sends. If the hub stamped at send time, a
    /// `Status` whose content was read at t0 could arrive stamped t3 — after a
    /// hydrate's `as_of` — and set a LEVEL from pre-hydrate content. Taking the
    /// stamp before the read makes the stamp a lower bound on the content's
    /// age, which is what the stamp rule assumes.
    ///
    /// **The legacy firehose is NOT here.** It was `threads`' transport, not the
    /// chassis's (docs/shapes.md 6.2, coupling 2). The producer sends it itself,
    /// after this call and off the SAME stamp, so there is still exactly one
    /// stamp per emit and the order is unchanged.
    pub fn emit_at(&self, at: Timestamp, value: I) {
        self.fan_out(Stamped::new(at, value));
    }

    /// **Emit a value that RESOLVES commands** (`docs/shapes.md` 2.4).
    ///
    /// The ids ride on the frames this input causes, so a consumer matches
    /// "my command took" to the fact that proves it — while the fact itself
    /// still arrives by the one path every other fact arrives by. The reply to
    /// the command carried an acknowledgement and no value, which is the whole
    /// point: a reply carrying the row would be a second source for it.
    ///
    /// **Which frame** is the whole of [`Resolving`]: `drive` delivers a keyed
    /// frame only to connections holding that key, so an id routed to the wrong
    /// key is an id that is never delivered at all. The producer knows the
    /// subject it is emitting about, and [`crate::command::PromiseBook::resolve`]
    /// hands back the ids already routed under it.
    pub fn emit_resolving(&self, at: Timestamp, value: I, promises: Resolving) {
        self.fan_out(Stamped::resolving(at, value, promises));
    }

    fn fan_out(&self, item: Stamped<I>) {
        let slots = self.shapes.read().unwrap();
        for s in slots.iter() {
            if s.fifo.tracking() {
                s.fifo.push_input(item.clone());
            }
        }
    }

    /// Is this shape currently tracking? The gate P6's credential sampler reads
    /// so a machine nobody is watching pays nothing.
    pub fn tracked(&self, shape: &str) -> bool {
        let slots = self.shapes.read().unwrap();
        slots
            .iter()
            .find(|s| s.name == shape)
            .is_some_and(|s| s.fifo.tracking())
    }

    /// **THE PRODUCER-SIDE GATE.** Every `emit_fact` producer asks this before
    /// it does any work, because a fact nobody is folding is a store read
    /// nobody wanted. It is the general form of `tracked("accounts")` so that
    /// registering a second shape cannot silently starve it.
    pub fn any_tracking(&self) -> bool {
        self.shapes.read().unwrap().iter().any(|s| s.fifo.tracking())
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.shapes.read().unwrap().iter().map(|s| s.name).collect()
    }

    /// **SUBSCRIBE ON A CONSUMER'S BEHALF** — the door `shapes.subscribe`
    /// comes through, and the ONLY one that touches the consumer ledger.
    ///
    /// It is separate from [`Self::subscribe`] because the chassis re-subscribes
    /// on its own account too: when the shape channel lags, `drive` re-asks for
    /// an export per held key. That is the chassis repairing itself, not a
    /// consumer arriving, and recording it would fill the ledger with events no
    /// consumer caused and — worse — would overwrite a known identity with the
    /// `None` the internal path has to pass. Two names, so the distinction is
    /// made at the call site by someone who knows which it is.
    ///
    /// The ledger is written only on SUCCESS. A subscribe refused for a dead
    /// shape or an absurd `at` created no subscription, and a ledger that
    /// recorded one would be asserting something untrue about the present.
    ///
    /// **AND IT IS THE ONLY DOOR WHERE RULE 2.6's LINK IS DECIDED, 2026-10-05.**
    /// `version` is what the consumer states it was built against (rule 2.1). It
    /// is here and not on [`Self::subscribe_ask`] for the same reason `identity`
    /// is: the chassis's own lag repair is NOT a consumer arriving, so it states
    /// no version and must not be able to — a re-subscribe that could be refused
    /// for a version mismatch would make rule 2.5's recovery path fail for a
    /// reason recovery cannot fix.
    ///
    /// **`None` is a consumer that stated nothing**, which is served and recorded
    /// as such — see [`crate::wire::ShapeContract::link`] for why that is not a
    /// refusal, and `docs/shapes.md` 2.6 for the hole it leaves, which is named
    /// rather than closed.
    ///
    /// **It is NOT `consumer.shape_version`.** That field is free text a consumer
    /// says about itself — *"even if it is a date"* — and [`crate::ledger`]'s
    /// module doc rules that the chassis never compares it, never branches on it
    /// and never refuses because of it. That ruling stands untouched: this is a
    /// different parameter, an integer, and rule 2.1 lists the two side by side
    /// precisely because one is evidence for a human and the other is a handshake.
    pub fn subscribe_as(
        &self,
        shape: &str,
        key: Option<&str>,
        conn: ConnId,
        at: Option<Timestamp>,
        identity: Option<crate::ledger::ConsumerIdentity>,
        detail: Detail,
        retain: Retain,
        beat: Beat,
        version: Option<u32>,
        page: Paging,
        from: Option<&str>,
    ) -> std::result::Result<SubscribeId, ShapeError> {
        let (id, link) = self.enqueue_subscribe(
            shape,
            key,
            conn,
            at,
            detail,
            Some(retain),
            Some(beat),
            version,
            page,
            from,
        )?;
        // **THE LEDGER RECORDS THE SUPERSEDED LINK** — rule 2.6's second state
        // says so in as many words. It rides the connect/resubscribe record rather
        // than a record of its own, and that is rule 2.1's compression rule: a
        // reconnect loop is the common failure, so an alternating
        // `superseded`/`connect` pair would defeat the run-length compression that
        // keeps the file readable and put one line per cycle on disk forever.
        let superseded = match link {
            crate::wire::Link::Superseded(s) => Some(s),
            _ => None,
        };
        self.consumers
            .connected_link(shape, key, conn, identity, superseded);
        Ok(id)
    }

    /// Enqueue an export request. Answers on the request id with
    /// `{queued: true, subscribe_id}` — EXCEPT when the shape is dead or the `at`
    /// is absurd, which are errors the consumer must see rather than wait on.
    ///
    /// **THE RETURNED [`SubscribeId`] IS RULE 2.7's `accepted` FOR A SUBSCRIBE**,
    /// and the export that satisfies this call names it in
    /// [`crate::wire::ShapeWire::subscribes`]. `{queued: true}` alone was
    /// *"an acknowledgement in spirit and anonymous in practice"* — the consumer
    /// could not match the export to the ask, so the board waited on its own
    /// `exports` COUNTER moving, with a deadline: the timeout-and-retry policy in
    /// every client that rule 2.5 exists to forbid, built because the ack had no
    /// name.
    ///
    /// **Records NOTHING in the consumer ledger.** A consumer-initiated
    /// subscribe goes through [`Self::subscribe_as`]; this is the mechanical
    /// form, used by the chassis's own lag-repair path.
    pub fn subscribe(
        &self,
        shape: &str,
        key: Option<&str>,
        conn: ConnId,
        at: Option<Timestamp>,
    ) -> std::result::Result<SubscribeId, ShapeError> {
        // **ASKING NOTHING MEANS THE CEILING** — rule 2.8's second clause, which
        // is what lets a knob be added without a version bump: every caller
        // written before `detail` existed keeps getting exactly what it got.
        self.subscribe_ask(shape, key, conn, at, Detail::Rows, Retain::Max, Beat::Max)
    }

    /// **A PAGED KEYLESS SUBSCRIBE**, the mechanical form: `page` and `from`
    /// as a consumer would send them, every other knob at its ceiling. See
    /// [`crate::wire::Paging`].
    pub fn subscribe_page(
        &self,
        shape: &str,
        conn: ConnId,
        page: Paging,
        from: Option<&str>,
    ) -> std::result::Result<SubscribeId, ShapeError> {
        self.enqueue_subscribe(
            shape,
            None,
            conn,
            None,
            Detail::Rows,
            Some(Retain::Max),
            Some(Beat::Max),
            None,
            page,
            from,
        )
        .map(|(id, _)| id)
    }

    /// [`Self::subscribe`], with the `detail` parameter stated. See
    /// [`crate::wire::Detail`]. `retain` takes its ceiling, for the same reason
    /// [`Self::subscribe`] takes `detail`'s: a caller written before a knob
    /// existed keeps getting exactly what it got.
    pub fn subscribe_detail(
        &self,
        shape: &str,
        key: Option<&str>,
        conn: ConnId,
        at: Option<Timestamp>,
        detail: Detail,
    ) -> std::result::Result<SubscribeId, ShapeError> {
        self.subscribe_ask(shape, key, conn, at, detail, Retain::Max, Beat::Max)
    }

    /// [`Self::subscribe`] with EVERY subscriber parameter stated — the full
    /// form, which the two above are the "asking nothing means the ceiling"
    /// shorthands for.
    pub fn subscribe_ask(
        &self,
        shape: &str,
        key: Option<&str>,
        conn: ConnId,
        at: Option<Timestamp>,
        detail: Detail,
        retain: Retain,
        beat: Beat,
    ) -> std::result::Result<SubscribeId, ShapeError> {
        // **STATES NO VERSION, DELIBERATELY.** This is the mechanical form and the
        // chassis is not a consumer: rule 2.6's link is decided at
        // [`Self::subscribe_as`], the one door a consumer comes through. See that
        // method for why the recovery path must not be refusable on this axis.
        // And asks for no page, so the chain's defaults decide: a mechanical
        // caller is a consumer that said nothing.
        self.enqueue_subscribe(
            shape,
            key,
            conn,
            at,
            detail,
            Some(retain),
            Some(beat),
            None,
            Paging::Default,
            None,
        )
        .map(|(id, _)| id)
    }

    /// **THE CHASSIS'S OWN LAG REPAIR, WHICH IS NOT A SUBSCRIBE REQUEST.**
    ///
    /// `drive` calls this when a connection's bounded frame channel has dropped
    /// frames: the repair for a lag is a re-export of everything that connection
    /// holds (rule 2.5 — recovery is re-subscribe, and there is no re-emit verb).
    ///
    /// **IT CARRIES THE CONNECTION'S NEGOTIATED [`Retain`] AND [`Beat`]
    /// THROUGH**, which [`Self::subscribe`] cannot do and must not: see
    /// [`SubscribeItem::retain`] for the defect. The short form is that
    /// `Hub::subscribe` means *asking nothing means the ceiling*, so routing the
    /// repair through it handed a consumer that had just fallen behind the
    /// largest frames and the fastest cadence the contract allows.
    ///
    /// Everything else is identical to a keyed subscribe with no `at`: the dead
    /// check still runs (a repair on a dead shape is still refused), no consumer
    /// ledger entry is written (nobody connected), and the [`SubscribeId`] it
    /// mints is named by the exports it causes — which a consumer matching its own
    /// ids simply does not recognise, exactly as before.
    ///
    /// `detail` IS passed, because the two repairs a connection can need are
    /// different asks: `Detail::Rows` for each key it holds, and `Detail::Keys`
    /// for the membership roster it holds. The roster half is the one that was
    /// missing — `drive` walked `shape_subs` only, and a `detail: keys`
    /// subscriber holds no key, so a membership mirror got NO repair at all.
    pub fn resubscribe_lagged(
        &self,
        shape: &str,
        key: Option<&str>,
        conn: ConnId,
        detail: Detail,
    ) -> std::result::Result<SubscribeId, ShapeError> {
        // Not an ask, so it names no page: the repair re-subscribes per HELD
        // key (`drive` walks `shape_subs`), where a page selects nothing, and
        // `Whole` is the honest spelling for a repair that must not shrink
        // what a mirror held.
        self.enqueue_subscribe(
            shape,
            key,
            conn,
            None,
            detail,
            None,
            None,
            None,
            Paging::Whole,
            None,
        )
        .map(|(id, _)| id)
    }

    /// [`Self::subscribe_ask`] and [`Self::resubscribe_lagged`] in one body.
    /// `retain`/`beat` of `None` is *not an ask* — see [`SubscribeItem::retain`].
    ///
    /// **Answers the [`crate::wire::Link`] alongside the id**, so the ONE caller
    /// that owns a consumer — [`Self::subscribe_as`] — can record rule 2.6's
    /// second state in the ledger without reading the contract a second time at a
    /// second instant. Returning the decision rather than re-deriving it is the
    /// same discipline `ShapeError::TooManyTimers` was given: one read, in one
    /// place, so the refusal and the record cannot disagree.
    #[allow(clippy::too_many_arguments)]
    fn enqueue_subscribe(
        &self,
        shape: &str,
        key: Option<&str>,
        conn: ConnId,
        at: Option<Timestamp>,
        detail: Detail,
        retain: Option<Retain>,
        beat: Option<Beat>,
        version: Option<u32>,
        page: Paging,
        from: Option<&str>,
    ) -> std::result::Result<(SubscribeId, crate::wire::Link), ShapeError> {
        let slots = self.shapes.read().unwrap();
        let slot = slots
            .iter()
            .find(|s| s.name == shape)
            .ok_or_else(|| ShapeError::Unknown(shape.to_string()))?;
        if let Some(reason) = slot.dead.lock().unwrap().clone() {
            return Err(ShapeError::Dead {
                shape: shape.to_string(),
                reason,
            });
        }
        // ── THE LINK, AGAINST THE PRODUCER'S DECLARED VERSION — RULE 2.6 ─────
        //
        // **AT LINK TIME, WHICH IS HERE AND NOWHERE ELSE.** The rule's title is
        // *"refused at LINK time, loudly"*, and this is the only moment a link is
        // established. Putting it in the fold path would make a version mismatch
        // something a consumer discovers through malformed frames — which is the
        // failure the rule was written against — and putting it on the frame path
        // would re-ask a settled question once per frame forever.
        //
        // Decidable from the slot's published contract alone, exactly like every
        // other refusal in this function, so it is decided where the answer can
        // still reach the caller rather than after `{queued: true}` went out. That
        // ordering is the whole lesson of `TooManyTimers`: an ack that means
        // nothing is worse than an error, because the consumer stops looking.
        //
        // **IT IS FIRST AMONG THE PARAMETER CHECKS, AND THAT ORDER IS A CLAIM.**
        // A consumer on a contract this producer does not serve may be ASKING
        // WRONGLY FOR REASONS THAT ARE DOWNSTREAM OF THAT — a `retain` against a
        // cell that no longer exists, a `detail` spelled the old way — so refusing
        // it on the parameter first would hand it the wrong remedy and send it to
        // fix a request that is not what is broken. The dead check stays above it
        // because a dead shape has no contract to serve anything on.
        //
        // **NEVER CLAMPED, AND THERE IS NOTHING HERE TO CLAMP TO**: the served
        // versions are a SET, not an interval, so "the nearest one we both know"
        // is not a value the producer could pick even if rule 2.8 let it.
        let link = slot.contract.link(version);
        if let crate::wire::Link::Refused { wanted, serves } = link {
            return Err(ShapeError::VersionRefused {
                shape: shape.to_string(),
                wanted,
                // Off the SLOT's published contract, never off the const. The
                // refusal and `shapes.list` therefore cannot disagree about what
                // this producer serves — the property `TooManyTimers` was given on
                // 2026-10-02 after it spent its life naming a ceiling it had
                // copied.
                serves,
            });
        }
        // ── `detail: keys` ASKS ABOUT THE POOL, SO IT TAKES NO KEY ───────────
        //
        // Membership is a question about what EXISTS; a key is an answer to it.
        // Answering `{shape, key, detail: keys}` with a one-key roster would be
        // a second spelling of the keyed subscribe that already exists, and two
        // spellings for one act is the collapse every repair in this file is
        // against. Decidable from the request alone.
        if detail == Detail::Keys && key.is_some() {
            return Err(ShapeError::BadDetail(format!(
                "`detail: keys` on shape `{shape}` is a question about the POOL and \
                 takes no `key`; drop the key for the membership roster, or drop \
                 `detail` for that key's rows"
            )));
        }
        // ── A KEYLESS `detail: rows` ON A PER-KEY SHAPE IS REFUSED HERE ──────
        //
        // "Every key I know about" on a shape that hydrates keys one at a time is
        // "nothing", and an ack followed by nothing is the class this whole design
        // is built against (see the armed-timer note below, which is the same
        // failure arriving through a different door). Decidable from the const
        // alone, so it is decided where the answer can still reach the caller.
        //
        // **AND IT NAMES THE CEILING NOW** — rule 2.8's third clause. The old
        // refusal told a consumer it had asked wrongly and not what to ask
        // instead; this one names `MAX_ROWS_PER_SUBSCRIBE` and points at
        // `detail: keys`, which is the ask that IS served.
        //
        // **`detail: keys` is NOT refused here, and that is the change.** A pool
        // that cannot afford to hand you its rows can still afford to tell you
        // what is in it — one statement, not a row scan.
        if key.is_none() && slot.hydrate == Hydrate::PerKey && detail == Detail::Rows {
            return Err(ShapeError::KeyRequired {
                shape: shape.to_string(),
                max_rows: slot.max.rows_per_subscribe,
            });
        }
        // ── `retain`, AGAINST THE PRODUCER'S DECLARED CEILING ────────────────
        //
        // Rule 2.8's third clause, and the two refusals are deliberately not one
        // (`crate::wire::ShapeError::NoRetention`): "there is no history here"
        // and "you asked for more history than I serve" are different facts with
        // different repairs, and collapsing them would tell a consumer to ask for
        // "0 or less", which is not the repair for either.
        //
        // **NEVER CLAMPED.** A clamped answer is a second claim about a number
        // the frame states outright, and the consumer must then decide which to
        // believe. Both refusals are decidable from the shape's const alone, so
        // both are decided where the answer can still reach the caller.
        if let Some(Retain::Count(n)) = retain {
            if slot.max.retained_items == 0 {
                return Err(ShapeError::NoRetention {
                    shape: shape.to_string(),
                });
            }
            if n > slot.max.retained_items {
                return Err(ShapeError::AboveMax {
                    shape: shape.to_string(),
                    param: "retain".to_string(),
                    asked: n.to_string(),
                    max: slot.max.retained_items.to_string(),
                });
            }
        }
        // ── `beat`, AGAINST THE PRODUCER'S DECLARED PERIOD ───────────────────
        //
        // Rule 2.8's third clause on the RATE axis. Two refusals again, and the
        // split is the same one `NoRetention`/`AboveMax` already made — but the
        // SECOND of them cannot be `AboveMax` at all, and that is the first thing
        // a rate ceiling changes about this code. `AboveMax` says "asked {asked},
        // max is {max}, ask for {max} or less"; here the ask that is beyond the
        // contract is the SMALLER number, so that sentence is false as arithmetic
        // and its remedy is advice to ask again. `TooFast` names the floor and
        // says MORE.
        //
        // **NEVER CLAMPED**, like every other ceiling here, and decidable from
        // the shape's const alone — so decided where the answer can still reach
        // the caller.
        // ── `page`, AGAINST THE PRODUCER'S DECLARED CEILING ──────────────────
        //
        // Rule 2.8's third clause for the paging chain's consumer link. The
        // ceiling is `rows_per_subscribe`, the same number a silent keyless
        // subscribe was always bounded by; `page: null` IS that number by name
        // and cannot be refused. **NEVER CLAMPED**, for the reason every other
        // refusal here is not.
        if let Paging::Rows(n) = page {
            if n > slot.max.rows_per_subscribe {
                return Err(ShapeError::AboveMax {
                    shape: shape.to_string(),
                    param: "page".to_string(),
                    asked: n.to_string(),
                    max: slot.max.rows_per_subscribe.to_string(),
                });
            }
        }
        if let Some(Beat::Every(ms)) = beat {
            if slot.max.beat_interval_ms == 0 {
                return Err(ShapeError::NoBeat {
                    shape: shape.to_string(),
                });
            }
            if ms < slot.max.beat_interval_ms {
                return Err(ShapeError::TooFast {
                    shape: shape.to_string(),
                    asked_ms: ms,
                    min_ms: slot.max.beat_interval_ms,
                });
            }
        }
        // ── THE OPEN-WINDOW CAP, ENFORCED WHERE THE CALLER CAN SEE IT ────────
        //
        // Rule 2.8 for the KEY parameter. It is taken HERE, under the lock, and
        // not inside the task, for the reason the armed-timer cap was moved here
        // on 2026-09-22: a cap the task discovers has already been answered
        // `{queued: true}`, and the only thing the task can then do is nothing —
        // which leaves the consumer waiting forever on a subscription it was told
        // it had.
        //
        // **IT IS A RESERVATION, NOT A READ**, on the armed-timer cap's own
        // argument: two concurrent subscribes that both merely READ a count of
        // `max - 1` would both enqueue and the root would hold `max + 1`.
        //
        // **IDEMPOTENT BY CONSTRUCTION.** The slot is a key in a SET, so a
        // re-subscribe to a key this connection already holds inserts nothing and
        // refuses nothing. Rule 2.5 makes re-subscribe the remedy for every doubt
        // a consumer can have about its mirror, and a ceiling that refused the
        // remedy would be a ceiling that breaks recovery.
        //
        // Only a KEYED subscribe reserves: `detail: keys` takes no key (refused
        // above if it names one) and is one standing membership interest, not a
        // window.
        if let (Some(k), Some(max)) = (key, slot.max.open_keys_per_conn) {
            let mut held = slot.open_keys.lock().unwrap();
            let set = held.entry(conn).or_default();
            if !set.contains(k) && set.len() >= max {
                // **NAMING THE CEILING** — rule 2.8's refusal clause. The number
                // comes off the slot's published `max`, never off the const, so
                // the refusal and `shapes.list` cannot disagree.
                return Err(ShapeError::TooManyKeys {
                    shape: shape.to_string(),
                    max,
                });
            }
            set.insert(k.to_string());
        }
        // Ruling 4. A past `at` is treated as `None`; a future `at` is bounded.
        let now = chrono::Utc::now();
        let at = match at {
            None => None,
            Some(t) if t <= now => None,
            Some(t) if t - now > MAX_FUTURE_AT => {
                // **THE BOUND IS DERIVED, NOT SPELLED.** It used to read "24h"
                // as a literal beside a `MAX_FUTURE_AT` the code compared
                // against, so changing the const would have left the refusal
                // naming the old ceiling — rule 2.8's clause broken by a
                // copy. `shapes.list` publishes the same number as
                // `max.future_at_secs`.
                return Err(ShapeError::BadAt(format!(
                    "{} is more than {}h ahead of the daemon's clock, which is \
                     this producer's ceiling for a future `at`",
                    t.to_rfc3339(),
                    MAX_FUTURE_AT.num_hours()
                )))
            }
            Some(t) => Some(t),
        };
        // ── THE ARMED-TIMER CAP, ENFORCED WHERE THE CALLER CAN SEE IT ────────
        //
        // **THE DEFECT THIS FIXES (2026-09-22).** The cap used to be checked
        // inside the shape task, on an item that had ALREADY been answered
        // `{"queued": true}` by the RPC. Over the limit, the task logged a
        // `warn!` and returned — no export, no error frame, no death notice.
        // The consumer waited forever on a subscription it had been told it
        // had. `ShapeError::TooManyTimers` was declared and formatted and
        // **never constructed anywhere in the crate**: the error for this exact
        // condition existed and went unused.
        //
        // That is the failure class this whole design is built against arriving
        // through a new door. An ack that means nothing is worse than an error,
        // because the consumer stops looking — the same shape as the absorb
        // refusal logged at `debug!` that left five days of work unabsorbed
        // behind a green build, and the same shape as the `{"queued": true}`
        // that `resync()` was changed to stop trusting. A control that refuses
        // quietly is a control nobody can act on.
        //
        // **It is a RESERVATION, not a read.** Two concurrent subscribes that
        // both merely READ a count of 3 would both enqueue and the task would
        // arm 5, which is the silent-drop defect again one step further along.
        // Taking the slot under the lock makes the counter authoritative, so
        // the task never has to refuse anything and never has to discover a
        // limit after the fact.
        //
        // Note this is checked AFTER the `at` normalisation above: a past `at`
        // is not a timer and must not consume a slot.
        if at.is_some() {
            let mut armed = slot.armed_timers.lock().unwrap();
            let n = armed.entry(conn).or_insert(0);
            if *n >= slot.max.armed_timers_per_conn {
                // **NAMING THE CEILING** — rule 2.8's refusal clause, which this
                // one broke until 2026-10-02. The number comes off the slot's
                // published `max`, not off the const, so the refusal and
                // `shapes.list` cannot disagree.
                return Err(ShapeError::TooManyTimers {
                    shape: shape.to_string(),
                    max: slot.max.armed_timers_per_conn,
                });
            }
            *n += 1;
        }
        if slot.lazy {
            slot.fifo.set_tracking(true);
        }
        // Minted only once every refusal above has been passed: an id handed out
        // for a subscribe that did not happen is an id no export will ever name,
        // which is the anonymous-ack problem wearing a number.
        let id = self
            .next_subscribe
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        slot.fifo.push_control(Item::Subscribe(SubscribeItem {
            conn,
            key: key.map(str::to_string),
            at,
            id,
            detail,
            retain,
            beat,
            // **RULE 2.6's WARNING, CARRIED TO THE TASK ON THE ITEM.** It travels
            // the same way `retain` and `beat` do, and for the same reason: the
            // task owns per-connection delivery state and the hub owns the
            // decision, so the decision has to ride the control item rather than
            // be re-derived inside the task against a contract it would have to be
            // handed anyway.
            //
            // `None` covers BOTH `Link::Current` and `Link::Unstated` — the two
            // states whose frames say nothing — which is why the task holds an
            // `Option` and not a `Link`: the thing the export needs is the warning
            // or its absence, and spelling "served, nothing to say" twice inside
            // the delivery path would be two spellings for one frame.
            superseded: match link {
                crate::wire::Link::Superseded(s) => Some(s),
                _ => None,
            },
            page: Paging::resolve(page, slot.page, slot.max.rows_per_subscribe),
            from: from.map(str::to_string),
        }));
        Ok((id, link))
    }

    pub fn unsubscribe(&self, shape: &str, key: Option<&str>, conn: ConnId) {
        {
            let slots = self.shapes.read().unwrap();
            if let Some(s) = slots.iter().find(|s| s.name == shape) {
                s.fifo.push_control(Item::Unsubscribe {
                    conn,
                    key: key.map(str::to_string),
                });
            }
        }
        // ── **A KEYED UNSUBSCRIBE NARROWS; A KEYLESS ONE CLOSES** ────────────
        //
        // **THE DEFECT THIS FIXES (2026-09-26).** Both used to call
        // `disconnected`, which takes no key and removes the whole
        // `(conn, shape)` entry — so `shapes.unsubscribe {shape, key: "b"}` from a
        // connection holding `a` and `b` emptied the ledger while the task still
        // held `(conn, Some("a"))` and still delivered `a`'s frames.
        // `shapes.list` then answered `subscribers: 1, consumers: 0`: one
        // question, two numbers, disagreeing — the third instance of the
        // collapsed-events disease in this path, after `subscribers`' own dedupe
        // and the keyless-unsubscribe retain.
        //
        // `narrowed` closes the entry itself when the key it drops was the last
        // one, so the deferred-close pairing has exactly one door either way.
        match key {
            // The CLEAN close. Deferred by one event in the ledger, so an
            // unsubscribe that is immediately followed by a subscribe is recorded
            // as the one recovery cycle it is — see `crate::ledger`.
            None => self
                .consumers
                .disconnected(shape, conn, crate::ledger::CloseReason::Unsubscribed),
            Some(k) => {
                self.consumers.narrowed(
                    shape,
                    k,
                    conn,
                    crate::ledger::CloseReason::Unsubscribed,
                );
            }
        }
    }

    /// `drive`'s exit path: one control item per shape, so a task's subscriber
    /// count is exact and a lazy shape actually stops.
    pub fn dropped(&self, conn: ConnId) {
        // Its frame channel goes with it, so `deliver` stops finding a sink and
        // the map cannot grow by one entry per connection this daemon has served.
        self.conns.write().unwrap().remove(&conn);
        {
            let slots = self.shapes.read().unwrap();
            for s in slots.iter() {
                s.fifo.push_control(Item::Dropped(conn));
            }
        }
        // **THE TRANSPORT IS WHAT CLOSES A LEDGER ENTRY.** This is reached from
        // `drive`'s `ConnectionGuard::drop`, so a connection that ends by a
        // close frame, a read error, a failed write, a PANIC or an ABORT all
        // arrive here identically. There is no reaper and no TTL anywhere in
        // `crate::ledger`, deliberately.
        self.consumers.dropped(conn);
    }

    /// `shapes.list`. Reads the `watch` each task publishes — **no cell is read
    /// from outside the task that owns it.**
    pub fn list(&self) -> Vec<ShapeInfo> {
        let slots = self.shapes.read().unwrap();
        slots
            .iter()
            .map(|s| {
                let mut info = s.info.borrow().clone();
                info.fifo_high_water = s.fifo.high_water();
                info.tracked = s.fifo.tracking();
                if let Some(reason) = s.dead.lock().unwrap().clone() {
                    info.epoch.state = ShapeState::Dead;
                    info.epoch.dead_reason = Some(reason);
                }
                // WHO IS SUBSCRIBED, from the hub's registry — never by
                // replaying the ledger file. See `crate::ledger`: an answer
                // computed from lines is an answer a missing line can rot.
                info.consumers = self.consumers.consumers(s.name);
                // The declared nesting edge, from the slot — see `Shape::PARENT`.
                info.parent = s.parent.map(str::to_string);
                info
            })
            .collect()
    }

    /// **THE SHAPES NESTED DIRECTLY INSIDE `parent`**, in registration order.
    ///
    /// Read off the declarations, never by splitting a name. Direct children only:
    /// a grandchild declares its own parent and appears under that.
    pub fn children(&self, parent: &str) -> Vec<&'static str> {
        self.shapes
            .read()
            .unwrap()
            .iter()
            .filter(|s| s.parent == Some(parent))
            .map(|s| s.name)
            .collect()
    }

    /// **EVERY DECLARED REFERENT WHOSE SHAPE IS NOT REGISTERED**, as
    /// `(shape, ref name, target)`. Empty on a well-formed producer once every
    /// shape is registered; a producer's own test asserts that, because
    /// references form a graph with cycles and `register` cannot decide it
    /// one shape at a time (see [`Shape::REFERS`]).
    pub fn dangling_refs(&self) -> Vec<(&'static str, String, String)> {
        let slots = self.shapes.read().unwrap();
        let mut out = Vec::new();
        for s in slots.iter() {
            for (r, target) in s.info.borrow().refs.iter() {
                if !slots.iter().any(|t| t.name == target) {
                    out.push((s.name, r.clone(), target.clone()));
                }
            }
        }
        out
    }

    pub fn is_dead(&self, shape: &str) -> Option<String> {
        let slots = self.shapes.read().unwrap();
        slots
            .iter()
            .find(|s| s.name == shape)
            .and_then(|s| s.dead.lock().unwrap().clone())
    }

    /// Register a shape and spawn its task plus its supervisor.
    ///
    /// **`producer` is a CLONE of the manager, not an `Arc<ThreadManager>`.**
    /// The design said `Arc`, and the critique's F6 flagged the resulting
    /// cycle; the code says neither is needed. `ThreadManager` is `Clone` with
    /// every field already behind an `Arc` (`runtime.rs:653`), `build_manager`
    /// returns it BY VALUE, and the crate's own idiom for moving it into a
    /// spawned task is `let mgr = self.clone()` (`start_watchers`, `:4165`).
    /// So the task owns a live manager, the `Hub` owns no manager at all, and
    /// there is no cycle to break. The hub reference the task keeps for
    /// EMITTING is a `Weak`, so a hub with no owner cannot be kept alive by its
    /// own task.
    /// `fifo_capacity` is the producer's call, not the chassis's: the number
    /// that was `FIFO_CAPACITY = 1024` here is justified by `threads`' own
    /// measured peak and means nothing to another producer (docs/shapes.md 6.2,
    /// coupling 5). [`DEFAULT_FIFO_CAPACITY`] is offered as a starting point,
    /// not as a default that silently applies.
    pub fn register<S>(self: &Arc<Self>, shape: S, producer: S::Producer, fifo_capacity: usize)
    where
        S: Shape<I>,
    {
        // ── THE NESTING DECLARATION, CHECKED AT REGISTER ─────────────────────
        //
        // A panic here fails the producer's build function, which runs at boot —
        // so a mis-declared tree is a daemon that does not start, not a consumer
        // that subscribes to something incoherent. `docs/chassis-resources.md` 8.3's
        // "declaration-time checks the chassis CAN do, panicking early".
        //
        // The parent must be registered ALREADY, which makes the tree
        // parents-first and makes an edge to a non-existent shape unrepresentable.
        // It also means the child can hold its parent's era counter directly,
        // rather than resolving a name later and finding nothing.
        let parent_gen: Option<Arc<AtomicU64>> = match S::PARENT {
            None => {
                assert!(
                    !S::NAME.contains('/'),
                    "shape `{}` has a path-shaped name but declares no PARENT. A \
                     `/` in a name is the nesting spelling, so this is either a \
                     missing declaration or a name that must not contain a slash.",
                    S::NAME
                );
                None
            }
            Some(p) => {
                let prefix = format!("{p}/");
                let leaf = S::NAME.strip_prefix(&prefix).unwrap_or_else(|| {
                    panic!(
                        "shape `{}` declares PARENT `{p}`, so its NAME must be \
                         `{p}/<leaf>`. Addressing is the name, so a name that does \
                         not spell the parent is a shape a consumer cannot find \
                         from its parent.",
                        S::NAME
                    )
                });
                assert!(
                    !leaf.is_empty() && !leaf.contains('/'),
                    "shape `{}` declares PARENT `{p}`, so the part after `{p}/` \
                     must be exactly one non-empty segment; got `{leaf}`. A deeper \
                     shape declares the shape above it as its parent, one level at \
                     a time.",
                    S::NAME
                );
                let slots = self.shapes.read().unwrap();
                let Some(parent) = slots.iter().find(|s| s.name == p) else {
                    panic!(
                        "shape `{}` declares PARENT `{p}`, which is not registered. \
                         Register parents first: the chassis holds the edge, not a \
                         name to be resolved later.",
                        S::NAME
                    )
                };
                Some(parent.reinit_gen.clone())
            }
        };

        // ── THE REFERENT DECLARATION, CHECKED AT REGISTER ────────────────────
        //
        // The key-level edge, on the nesting declaration's terms: a panic here
        // is a daemon that does not boot. Two refusals, each a lie the
        // declaration could tell about ITSELF — a referent for a `Ref` no cell
        // carries (a declaration about nothing), and one name resolving twice.
        //
        // **WHETHER THE SHAPE NAMED EXISTS IS NOT DECIDED HERE, and that is a
        // difference from `PARENT`, stated.** Nesting is a tree and parents-first
        // is its discipline. References are a GRAPH with cycles — `accounts` rows
        // name threads and `threads` rows name accounts — so whichever registers
        // first cannot see the other, and a parents-first check would refuse
        // every mutual edge or force a declaration onto the wrong shape. The
        // edge is recorded as unresolved and resolved when its target registers;
        // what remains is [`Self::dangling_refs`], which a producer's own test
        // asserts empty once every shape is registered.
        {
            let mut carried = S::CELLS.ref_names();
            for r in S::SHARED.ref_names() {
                if !carried.contains(&r) {
                    carried.push(r);
                }
            }
            for (r, target) in S::REFERS {
                assert!(
                    carried.contains(r),
                    "shape `{}` declares REFERS `{r}` -> `{target}`, but no cell or \
                     field in its manifest is a `Ref({r:?})`. A referent for a \
                     reference nothing carries is a declaration about nothing; the \
                     manifest's ref names are {carried:?}.",
                    S::NAME
                );
                assert!(
                    S::REFERS.iter().filter(|(x, _)| x == r).count() == 1,
                    "shape `{}` declares REFERS `{r}` more than once; one reference \
                     name resolves to one shape.",
                    S::NAME
                );
            }
        }

        // ── THE PAGE DECLARATION, CHECKED AT REGISTER ────────────────────────
        //
        // A producer default of zero is an answer of nothing to every silent
        // consumer; one above the ceiling is a default the producer would refuse
        // if a consumer asked for it. Both are declarations that contradict the
        // shape's own contract, so both fail the boot.
        if let Paging::Rows(n) = S::PAGE {
            assert!(
                n > 0,
                "shape `{}` declares PAGE `Rows(0)`. A page of nothing answers every \
                 silent keyless subscribe with nothing; declare `Whole`, or a number.",
                S::NAME
            );
            assert!(
                n <= S::MAX_ROWS_PER_SUBSCRIBE,
                "shape `{}` declares PAGE `Rows({n})` above its own \
                 MAX_ROWS_PER_SUBSCRIBE of {}. A default the producer would refuse \
                 a consumer for asking is not a default.",
                S::NAME,
                S::MAX_ROWS_PER_SUBSCRIBE
            );
        }

        let fifo = Arc::new(Fifo::new(fifo_capacity));
        // ── THE DECLARATION, READ ONCE — RULES 2.6, 2.8, AND THE CELL MANIFEST ─
        //
        // `ShapeInfo::declared_by` is the one read of every const on the shape:
        // the ceilings a refusal names, the version contract a link is checked
        // against, the counters seeded at zero, and the cell manifest a consumer
        // routes its reads against. One read, so the value on `shapes.list`
        // cannot drift from the value a refusal or a manifest check uses.
        //
        // Rule 2.6's declaration checks (`VERSION` not zero, `SERVES` strictly
        // descending) run INSIDE it, so a panic here still fails the producer's
        // build function at boot, on exactly the terms the `PARENT` checks above
        // are panics: a mis-declared contract must be a daemon that does not
        // start, never a consumer that links against something incoherent.
        // `docs/chassis-resources.md` 8.3's *"declaration-time checks the
        // chassis CAN do, panicking early"*. There is deliberately NO check on
        // the LENGTH of `SERVES`: rule 2.6 advises one version back, and a
        // ceiling nobody ruled is a number this chassis may not invent.
        let declared = ShapeInfo::declared_by::<I, S>();
        let max = declared.max.clone();
        let contract = declared.contract.clone();
        let reinit_gen = Arc::new(AtomicU64::new(0));
        let (info_tx, info_rx) = watch::channel(declared.clone());
        let dead: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
        // ONE counter, two holders: the hub reserves against it in `subscribe`,
        // the task releases against it as timers fire and connections end.
        let armed_timers: Arc<StdMutex<HashMap<ConnId, usize>>> =
            Arc::new(StdMutex::new(HashMap::new()));
        // ONE set, two holders, on `armed_timers`' own terms: the hub reserves in
        // `subscribe`, the task releases as keys and connections end.
        let open_keys: Arc<StdMutex<HashMap<ConnId, BTreeSet<String>>>> =
            Arc::new(StdMutex::new(HashMap::new()));

        if S::TRACKING == Tracking::Eager {
            fifo.set_tracking(true);
        }

        let task = ShapeTask::<I, S> {
            pending_promises: Resolving::none(),
            current_promises: Resolving::none(),
            carried: BTreeMap::new(),
            shape,
            producer,
            fifo: fifo.clone(),
            hub: Arc::downgrade(self),
            info_tx,
            root: None,
            subs: HashSet::new(),
            retains: HashMap::new(),
            beats: HashMap::new(),
            last_beat: HashMap::new(),
            armed: Vec::new(),
            armed_timers: armed_timers.clone(),
            open_keys: open_keys.clone(),
            pending_rows: BTreeSet::new(),
            debounce_deadline: None,
            last_reinit: None,
            reinit_gen: reinit_gen.clone(),
            parent_gen,
            // **SEEDED, like the watch channel above.** The task publishes its
            // own copy over that channel, so seeding only the initial value left
            // the counters empty the moment the task first published — which is
            // why the live wire still lacked `unplaced_status` after the unit
            // test went green. The test exercised the constructor in isolation
            // and never the wiring. Same value as the channel's seed, by being
            // the same read.
            info: declared,
            superseded: HashMap::new(),
            members_hydrated: false,
            member_subs: HashSet::new(),
        };

        let handle = tokio::spawn(task.run());

        // ── TASK SUPERVISION (ruling 1) ──────────────────────────────────
        //
        // Not a heartbeat: no timer, no periodic frame. The `JoinHandle` IS the
        // signal. When it resolves — a panic in `folds`, a cancelled runtime, a
        // clean exit that should not have happened — three things must be true
        // at once, or the failure is silent:
        //
        //   1. the FIFO stops accepting inputs, so `Hub::emit` is not feeding
        //      an orphan until it fills and `lost` climbs with nobody to read
        //      it;
        //   2. `shapes.subscribe` is REFUSED with `ShapeError::Dead`, instead
        //      of answering `{queued: true}` to a consumer that will then wait
        //      forever;
        //   3. ONE final export goes out with `epoch.state: dead`, so every
        //      held mirror flips its cells to `?` instead of showing stale
        //      state as current.
        let sup_dead = dead.clone();
        let sup_fifo = fifo.clone();
        let sup_info = info_rx.clone();
        // A `Weak`, like the task's own: a hub with no owner must not be kept
        // alive by its own supervisor.
        let sup_hub = Arc::downgrade(self);
        tokio::spawn(async move {
            let reason = match handle.await {
                Ok(()) => "the shape task exited".to_string(),
                Err(e) if e.is_panic() => "the shape task panicked".to_string(),
                Err(_) => "the shape task was cancelled".to_string(),
            };
            tracing::error!(shape = S::NAME, reason = %reason, "SHAPE TASK DIED — refusing new subscriptions and telling every mirror");
            *sup_dead.lock().unwrap() = Some(reason.clone());
            sup_fifo.set_tracking(false);
            sup_fifo.clear();
            let info = sup_info.borrow().clone();
            let epoch = Epoch {
                init_at: info.epoch.init_at,
                reinit: info.epoch.reinit,
                lost: info.epoch.lost,
                state: ShapeState::Dead,
                dead_reason: Some(reason.clone()),
                // Carried through, not recomputed: the death frame says which era
                // the state it is invalidating belonged to.
                parent_epoch: info.epoch.parent_epoch,
                // **`None` ON THE DEATH FRAME, AND THAT COSTS NOTHING BECAUSE THE
                // MIRROR IS STICKY.** This is the one frame in the chassis that is
                // built OUTSIDE the task — the supervisor has no `self.superseded`
                // to read and `deliver_to_all` addresses nobody in particular, so
                // there is no per-connection link to stamp here. Rule 2.6's warning
                // is therefore kept by the consuming half rather than re-asserted on
                // this frame: `mirror::Mirror::superseded` latches on first sight and
                // is cleared only by a reset, so a superseded consumer's final frame
                // does not repaint its link as clean on the way out. Teaching the
                // supervisor to group by link would mean one more reader of the
                // task's per-connection state from outside the task, which this file
                // forbids everywhere else.
                superseded: None,
            };
            let Some(hub) = sup_hub.upgrade() else {
                return;
            };
            // ONE frame, `key: None`, for the whole shape — one COPY per
            // connection now that each has its own channel, but still exactly one
            // frame per mirror, which is what "emit ONE final export" means with a
            // `Map` root. The delivery loop's gate (does this connection hold any
            // key of this shape) is unchanged and is what keeps it addressed.
            hub.deliver_to_all(&Arc::new(ShapeFrame {
                to: None,
                shape: S::NAME.to_string(),
                key: None,
                wire: ShapeWire {
                    promises: Vec::new(),
                    // A death export satisfies no subscribe: `shapes.subscribe` is
                    // REFUSED once the task is dead, so nothing is outstanding for
                    // this frame to answer.
                    subscribes: Vec::new(),
                    shape: S::NAME.to_string(),
                    key: None,
                    op: Op::Read,
                    at: None,
                    as_of: Some(chrono::Utc::now()),
                    epoch,
                    cells: None,
                    row: None,
                    shared: None,
                    shared_cells: None,
                    event: None,
                    members: None,
                    complete: None,
                    next: None,
                },
            }));
        });

        self.shapes.write().unwrap().push(ShapeSlot {
            name: S::NAME,
            fifo,
            info: info_rx,
            dead,
            lazy: S::TRACKING == Tracking::Lazy,
            hydrate: S::HYDRATE,
            max,
            contract,
            parent: S::PARENT,
            page: S::PAGE,
            reinit_gen,
            armed_timers,
            open_keys,
        });
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 10. The shape task
// ─────────────────────────────────────────────────────────────────────────────

struct ShapeTask<I, S: Shape<I>> {
    shape: S,
    producer: S::Producer,
    fifo: Arc<Fifo<I>>,
    hub: std::sync::Weak<Hub<I>>,
    info_tx: watch::Sender<ShapeInfo>,
    /// `None` until init. Under `Lazy` this is where "the task holds no state"
    /// is true.
    root: Option<S::Root>,
    subs: HashSet<(ConnId, Option<String>)>,
    /// **EACH CONNECTION'S DECLARED [`Retain`] ON THIS SHAPE.** Absent means
    /// [`Retain::Max`], which is the ceiling — so a connection that never asked
    /// is indistinguishable from one that asked for everything, and that is
    /// correct: they want the same frames.
    ///
    /// **Per CONNECTION, not per `(conn, key)`.** A keyless subscribe is one ask
    /// covering every key and has no per-key retain to record, so there is no
    /// spelling in which the finer map is complete. `SubscribeItem::retain`
    /// carries the same note.
    retains: HashMap<ConnId, Retain>,
    /// **EACH CONNECTION'S DECLARED [`Beat`] ON THIS SHAPE.** Absent means
    /// [`Beat::Max`], which is the ceiling, so absence and `Max` are the same
    /// delivery and need no separate spelling — exactly as for [`Self::retains`].
    beats: HashMap<ConnId, Beat>,
    /// **EACH CONNECTION LINKED ON A SUPERSEDED CONTRACT, AND WHICH ONE** — rule
    /// 2.6's middle state, held per connection because the link is a property of
    /// the connection and not of the fold.
    ///
    /// **A connection absent from this map is a link with nothing to warn about**
    /// — it asked for the current version, or it stated none — so absence is the
    /// ordinary case and every frame for it is byte-identical to what this chassis
    /// has always emitted.
    ///
    /// Written ONCE per connection (see `do_subscribe`) and cleared only when the
    /// connection goes, beside [`Self::retains`] and [`Self::beats`], so this map
    /// cannot grow by one entry per connection this daemon has ever served.
    superseded: HashMap<ConnId, crate::wire::Superseded>,
    /// **WHEN EACH CONNECTION LAST RECEIVED A BEAT FRAME.**
    ///
    /// The state a RATE projection needs and a QUANTITY projection does not.
    /// [`Retain`] is a pure function of the cell and the ask, so it added nothing
    /// here; a cadence is a property of the INTERVAL BETWEEN two frames, which
    /// means it has no representation on any single frame and cannot be computed
    /// without remembering the last one. This map is that memory, and it is the
    /// only clock the delivery path reads. The FOLD still reads none.
    last_beat: HashMap<ConnId, tokio::time::Instant>,
    /// Future-`at` subscribe items waiting on their own timers. They sit HERE,
    /// not inline in the loop, so a held item cannot stop the FIFO draining —
    /// the critique's F3 bullet.
    armed: Vec<(Timestamp, SubscribeItem)>,
    /// The hub's view of [`Self::armed`], per connection. The task does not
    /// decide the cap any more — [`Hub::subscribe`] does, where a refusal can
    /// still reach the caller — so this task's only job here is to RELEASE:
    /// when a timer fires, and when a connection unsubscribes or ends.
    armed_timers: Arc<StdMutex<HashMap<ConnId, usize>>>,
    /// See [`ShapeSlot::open_keys`] — the hub's half of this reservation. The task
    /// never decides the cap and only ever RELEASES.
    open_keys: Arc<StdMutex<HashMap<ConnId, BTreeSet<String>>>>,
    pending_rows: BTreeSet<String>,
    /// Promises whose resolving input has been folded but whose frame is still
    /// behind the debounce, **kept under the subject each one is about**. Key
    /// `k`'s ids ride out with key `k`'s flush and with nobody else's; ids whose
    /// key produces no frame stay here, because an unresolved promise is visible
    /// and a silently discarded one is not.
    pending_promises: Resolving,
    /// The promises of the input being folded RIGHT NOW, for the live frames
    /// that input emits directly — again per subject, so a transition on key A
    /// does not carry key B's id.
    current_promises: Resolving,
    /// **PROMISE IDS A FRAME HAS ALREADY CARRIED, KEPT SO A RE-EXPORT CAN CARRY
    /// THEM AGAIN** — `(stamp, id)` per subject, keyless under `None`.
    ///
    /// # THE HOLE THIS CLOSES, which is the architectural one
    ///
    /// A promise id rides a LIVE frame. A consumer that lags or reconnects loses
    /// live frames by definition — and rule 2.5's remedy for both is
    /// *re-subscribe*, which re-exports. The export hard-coded
    /// `promises: Vec::new()`, and the book had already closed the ids at emit, so
    /// **no frame that would ever be sent again could carry them**. The consumer's
    /// only recourse was to notice its promise had not resolved, decide it had
    /// been too long, and ask again — which is the precise policy rule 2.5 exists
    /// to keep out of clients (*"a re-emit request would force EVERY consumer to
    /// implement notice-a-promise-did-not-resolve, decide-it-has-been-too-long,
    /// ask-again"*). The rule's own justification says that case is *"one an ack
    /// already closes"*; the ack does not exist, and under rule 2.2 it will never
    /// cover a `row` frame, which is the frame the id rides.
    ///
    /// So the promise closes in the BOOK at the emit — an emit is owed and one
    /// happened — and its ANSWER stays retrievable here until a re-export has had
    /// the chance to carry it. Rule 2.5 says re-subscribe must be *"cheap and
    /// complete"*; this is the half of "complete" that was missing.
    ///
    /// # Why per KEY and not per (connection, key)
    ///
    /// The audit proposed either. Per connection is exact — each consumer is
    /// offered only ids it has not already been sent — and it is not available:
    /// which connections hold a key lives in `drive`'s `shape_subs`, on the other
    /// side of the transport, and the connection a reconnecting consumer comes
    /// back on is a NEW `ConnId` anyway, so a per-connection record would be
    /// keyed under the identity that just died. Per key costs a consumer the
    /// occasional already-seen id, which it ignores, and it is the only form that
    /// answers the question the reconnect asks.
    ///
    /// **Bounded twice**, by [`PROMISE_RETENTION`] and by
    /// [`PROMISE_RETENTION_PER_KEY`], so a producer resolving promises faster than
    /// anyone reconnects grows nothing. It deliberately SURVIVES `stop_if_idle`:
    /// the last consumer going away is exactly the moment these ids become the
    /// only record that a command was answered.
    carried: BTreeMap<Option<String>, VecDeque<(Timestamp, u64)>>,
    /// **HAS THE POOL'S MEMBERSHIP BEEN READ THIS EPOCH?**
    ///
    /// The membership read is ONE statement and it is still a statement, so it
    /// runs once per epoch and not once per subscribe. It runs LAZILY — on the
    /// first `detail: keys` subscribe — rather than at init, because
    /// `threads`' whole affordability argument is that INIT READS NO THREAD and
    /// a roster read at init would put an id scan in front of every keyed
    /// subscribe for a roster nobody asked for.
    ///
    /// Reset by `reinit_now` and by `stop_if_idle`, which are the two moments
    /// the root it populated goes away.
    members_hydrated: bool,
    /// **THE CONNECTIONS SUBSCRIBED AT `detail: keys`**, so a re-init can repair
    /// their rosters.
    ///
    /// It is not derivable from [`Self::subs`], and that is why it exists. A
    /// membership subscribe records `(conn, None)` — and so does a keyless
    /// `detail: rows` subscribe, beside its per-key entries. The two are
    /// indistinguishable there, and a re-init that guessed would either skip a
    /// membership subscriber (which is the silence this field prevents) or send
    /// a roster to a consumer that never asked for one.
    ///
    /// **THE HOLE THIS CLOSES.** `reinit_now` repairs mirrors by re-exporting
    /// each key to `holders_of(k)`. A membership subscriber HOLDS NO KEY, so it
    /// is in no `holders_of` set and would have received nothing at all — while
    /// the root it was mirroring had been thrown away and rebuilt. It would go
    /// on believing a roster from a previous epoch, with no frame to tell it
    /// otherwise, which is the stale-mirror class the death export and
    /// `epoch.reinit` both exist to close, arriving through the one door that
    /// did not have a repair.
    member_subs: HashSet<ConnId>,
    debounce_deadline: Option<tokio::time::Instant>,
    last_reinit: Option<Timestamp>,
    /// This shape's own era counter, shared with its slot so children can read it.
    /// This task is its only writer.
    reinit_gen: Arc<AtomicU64>,
    /// The PARENT's era counter, for a nested shape. Read-only here, and read
    /// exactly once per init — see [`crate::wire::Epoch::parent_epoch`] for the
    /// ruling that makes one read at init the right amount of coupling.
    parent_gen: Option<Arc<AtomicU64>>,
    info: ShapeInfo,
}

impl<I: Kind + Clone + Send + Sync + 'static, S: Shape<I>> ShapeTask<I, S> {
    async fn run(mut self) {
        loop {
            let debounce = self.debounce_deadline;
            let next_timer = self.armed.iter().map(|(t, _)| *t).min();

            tokio::select! {
                item = self.fifo.recv() => {
                    self.absorb_lost();
                    self.handle(item).await;
                }
                _ = async {
                    match debounce {
                        Some(d) => tokio::time::sleep_until(d).await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.flush_rows(None);
                }
                _ = async {
                    match next_timer {
                        Some(t) => sleep_until_stamp(t).await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(t) = next_timer {
                        let due: Vec<SubscribeItem> = {
                            let (fire, keep): (Vec<_>, Vec<_>) =
                                std::mem::take(&mut self.armed).into_iter().partition(|(a, _)| *a <= t);
                            self.armed = keep;
                            fire.into_iter().map(|(_, s)| s).collect()
                        };
                        for s in due {
                            // RELEASE FIRST. The reservation covers the time a
                            // subscribe spends WAITING; once it fires it is an
                            // ordinary subscribe and must not keep holding a
                            // slot, or a consumer clocking itself
                            // ("issue the next subscribe when the last export
                            // arrives") would exhaust its allowance after four
                            // perfectly successful cycles.
                            self.release_timer(s.conn);
                            self.do_subscribe(s).await;
                        }
                    }
                }
            }
        }
    }

    /// Overflow handling. `swap(0)` (not a read) plus a 5s floor is what closes
    /// the re-init livelock under sustained overload.
    fn absorb_lost(&mut self) {
        let lost = self.fifo.take_lost();
        if lost == 0 {
            return;
        }
        self.info.epoch.lost += lost;
        let now = chrono::Utc::now();
        let too_soon = self
            .last_reinit
            .is_some_and(|t| (now - t).num_seconds() < REINIT_FLOOR_SECS);
        tracing::warn!(
            shape = S::NAME,
            lost,
            reinit = !too_soon,
            "shape FIFO overflowed — events were dropped for this shape"
        );
        if too_soon || self.root.is_none() {
            self.publish();
            return;
        }
        self.reinit_now(now);
    }

    fn reinit_now(&mut self, now: Timestamp) {
        self.last_reinit = Some(now);
        self.info.epoch.reinit += 1;
        // ── WHAT THE OLD ROOT HELD, CAPTURED BEFORE IT IS THROWN AWAY ────────
        //
        // **THE DEFECT THIS FIXES (2026-10-02): A RE-INIT COULD NOT SHRINK A
        // MIRROR.** This function emitted no `gone` at all and re-exported only
        // the NEW root's keys, while `crate::mirror::Roster::page` UNIONS — it
        // has to, because `export_members` emits a walk as N frames and only the
        // last can claim `complete`. So a key that existed before a re-init and
        // not after it survived in the consumer's roster, inside a page marked
        // `complete: true`, and its row stayed in `held` with every cell still
        // `Known`. The mirror reported a key that does not exist and a row that
        // is no longer anybody's, with full confidence, and nothing on the wire
        // could ever correct it: the consumer's only recourse was to notice and
        // re-subscribe, which is the detect-and-retry policy rule 2.5 exists to
        // keep out of clients.
        //
        // The one existing re-init test floods a SINGLE key, so the before-set
        // and the after-set are equal by construction and it cannot see this.
        let before: Vec<String> = self
            .root
            .as_ref()
            .map(|r| r.keys())
            .unwrap_or_default();
        // **PUBLISHED FOR CHILDREN, and that is ALL it does to them.** A child
        // reads this at its own init and stamps it; nothing here makes a child
        // re-hydrate, re-export or stop. See `wire::Epoch::parent_epoch` for why
        // coupling the two was rejected.
        self.reinit_gen
            .store(self.info.epoch.reinit, Ordering::Relaxed);
        self.root = None;
        // A new epoch is a new root, so the membership this task read belongs to
        // the era it is invalidating. Re-read below, for the subscribers that
        // asked for it.
        self.members_hydrated = false;
        // Re-hydrate, then repair every mirror with a fresh export.
        let keys = self.init_blocking();
        let as_of = chrono::Utc::now();
        // ── THE KEYS THAT DID NOT COME BACK, SAID OUT LOUD ───────────────────
        //
        // **BEFORE the re-exports and before the roster**, for the same reason
        // the fold emits `added` before rows: a membership statement must reach a
        // consumer before the frames that depend on it. `gone` is the only word
        // on this wire that means *this key no longer exists*, and it is
        // deliberately NOT the same as withholding the key from the new roster —
        // a union cannot express a removal, and a consumer that inferred one from
        // an absence would be inferring it from the same silence that means
        // "this page did not reach you".
        //
        // Undirected and keyed, exactly as the fold's `gone` is: `drive` delivers
        // it to every connection holding that key AND to every connection holding
        // standing membership interest, which is precisely the set of mirrors that
        // hold the key being unsaid.
        //
        // The ids ride it for the fold's reason too — no `row` will ever flush for
        // a key that left, so a promise routed to that subject has no other
        // carrier.
        //
        // **`subs` IS LEFT ALONE.** A connection that asked about key `k` still
        // wants to know about key `k`; if the next epoch brings it back,
        // `holders_of` is what re-exports it. Forgetting the interest here would
        // turn "this key is gone" into "and I have stopped listening for you",
        // which the consumer did not ask for.
        if !before.is_empty() {
            let still: std::collections::HashSet<&String> = keys.iter().collect();
            let departed: Vec<String> = before
                .into_iter()
                .filter(|k| !still.contains(k))
                .collect();
            for k in departed {
                self.pending_rows.remove(&k);
                let ids = self.pending_promises.take_key(&k);
                self.emit_live_with(Some(k), as_of, Derived::Gone, ids);
            }
        }
        // ── AND REPAIR THE MEMBERSHIP MIRRORS, WHICH HOLD NO KEY ─────────────
        //
        // The loop below reaches `holders_of(k)`, and a `detail: keys`
        // subscriber is in no such set. Without this it would receive NOTHING
        // from a re-init — no roster, no `added`, not even a frame carrying the
        // new `epoch.reinit` — while the root behind its roster was discarded
        // and rebuilt. Same repair, same shape, different audience.
        if !self.member_subs.is_empty() {
            self.hydrate_members_once();
            for conn in self.member_subs.iter().copied().collect::<Vec<_>>() {
                // `satisfies: None` — a re-init's re-export answers no
                // `shapes.subscribe`; `epoch.reinit` is what announces it, and
                // naming a subscribe id would tell a consumer its own call had
                // been answered twice.
                self.export_members(conn, None, as_of);
            }
        }
        for k in keys {
            // **ADDRESSED, one connection at a time.** This was one undirected
            // send per key, which charged a slot to EVERY connection on the
            // producer — including connections subscribed to other shapes
            // entirely — for a frame the delivery loop then dropped for all but
            // the holders of that key. A shape with four keys and two other
            // consumers spent eight slots to reach one mirror, unprompted, at the
            // exact moment the producer was already under enough load to have
            // overflowed a FIFO. That is a self-inflicted lag, and its repair is
            // a re-subscribe, which is more sends again.
            //
            // The recipients are the same recipients: the delivery loop already
            // delivered a keyed undirected frame only to connections holding that
            // key. A key that came into existence during this re-hydrate reaches
            // nobody, exactly as before — that is `docs/shapes.md` 7.2's open gap
            // and it is not this change's to close.
            // A re-init's re-export satisfies no `shapes.subscribe` — nobody asked
            // for it, `epoch.reinit` is what announces it, and naming a subscribe
            // id here would tell a consumer its own call had been answered twice.
            let holders = self.holders_of(&k);
            for conn in holders {
                // `complete: None` — a re-init's re-export satisfies no
                // subscribe, so there is no answer for it to complete.
                self.export(Some(conn), Some(k.clone()), as_of, None, None, None);
            }
        }
    }

    async fn handle(&mut self, item: Item<I>) {
        match item {
            Item::Input(st) => {
                // The shape's own counter for this input, if it names one.
                // See [`Shape::counts`] for why this is here and not in the fold.
                if let Some(name) = self.shape.counts(&st) {
                    *self.info.counters.entry(name.to_string()).or_insert(0) += 1;
                    self.publish();
                }
                if self.root.is_none() {
                    // Untracked or pre-init: nothing to fold into. Under Lazy
                    // the FIFO is not even accepting, so this is the Eager
                    // pre-init case only.
                    return;
                }
                self.fold(&st);
            }
            Item::Subscribe(s) => {
                if let Some(at) = s.at {
                    // **NO REFUSAL HERE, AND THAT IS THE FIX (2026-09-22).**
                    //
                    // This arm used to check `MAX_ARMED_TIMERS_PER_CONN`
                    // against `self.armed`, log a `warn!` and `return` — on an
                    // item the RPC had already answered `{"queued": true}`. The
                    // consumer then got nothing at all: no export, no error, no
                    // death notice, waiting forever on a subscription it was
                    // told it had.
                    //
                    // The cap now lives in `Hub::subscribe`, which RESERVES a
                    // slot under a lock and answers `ShapeError::TooManyTimers`
                    // on the request id. By the time an item reaches this line
                    // its slot is already held, so there is nothing left to
                    // decide and nothing that can be dropped in silence.
                    debug_assert!(
                        self.armed_timers
                            .lock()
                            .unwrap()
                            .get(&s.conn)
                            .copied()
                            .unwrap_or(0)
                            > 0,
                        "an armed subscribe reached the task without a reservation; \
                         the cap in `Hub::subscribe` is the only thing holding this \
                         invariant and it has been bypassed"
                    );
                    self.armed.push((at, s));
                    return;
                }
                self.do_subscribe(s).await;
            }
            Item::Unsubscribe { conn, key } => {
                // ── A KEYLESS SUBSCRIBE INSERTED N+1 ENTRIES; A KEYLESS
                //    UNSUBSCRIBE REMOVES N+1 ─────────────────────────────────
                //
                // **THE DEFECT THIS FIXES.** `do_subscribe` on a keyless
                // subscribe inserts `(conn, None)` AND `(conn, Some(k))` for
                // every key it exports — that is how the delivery gate knows
                // which keys this connection holds. A keyless UNSUBSCRIBE removed
                // exactly `(conn, None)`, so every per-key entry survived: the
                // set stayed non-empty, `stop_if_idle` saw a live subscriber, and
                // a LAZY shape whose one keyless consumer cleanly unsubscribed
                // NEVER WENT IDLE AGAIN — it kept its root, kept accepting inputs,
                // and kept folding for nobody until the process ended.
                //
                // It also made two fields disagree for the wrong reason: the
                // consumer ledger, keyed by `(conn, shape)`, closed its entry at
                // once, so `consumers` was empty while `subscribers` still said 1.
                //
                // Retain-by-conn is what `Item::Dropped` below has always done,
                // for the same reason, and the two acts differ only in whether the
                // transport is still there. There is no way to distinguish the
                // pairs a keyless subscribe created from pairs an explicit keyed
                // subscribe created — nor should there be: "stop sending me this
                // shape" is what a keyless unsubscribe means.
                //
                // ── **AND AN ARMED FUTURE `at` IS CANCELLED BY KEY, NOT BY
                //    CONNECTION** ──────────────────────────────────────────────
                //
                // **THE DEFECT THIS FIXES (2026-09-26).** Both arms fell through
                // to `self.armed.retain(|(_, s)| s.conn != conn)`, which drops
                // EVERY armed subscribe this connection holds whatever key it
                // named. So a consumer holding a future-`at` subscribe on `a` and
                // a live one on `b`, unsubscribing `b`, silently lost `a`: it had
                // been answered `{queued: true, subscribe_id}` — rule 2.7's
                // `accepted` — and no export, no error and no death notice ever
                // followed. Ack-then-silence is the exact class the `accepted` /
                // `visible` split and the move of `TooManyTimers` out of this task
                // exist to close, and a keyed unsubscribe is not a statement about
                // any other key.
                //
                // The reservations go back one per cancelled item rather than by
                // clearing the counter, so the `debug_assert!` above — a reserved
                // slot per armed item — stays exactly true, and a connection that
                // cancels one of four timers can arm one more and not four.
                match key {
                    None => {
                        self.subs.retain(|(c, _)| *c != conn);
                        // "Stop sending me this shape" includes its membership:
                        // a keyless unsubscribe is the ONLY door a `detail:
                        // keys` subscriber has, so leaving it here would have a
                        // re-init push a roster at a consumer that had asked to
                        // stop.
                        self.member_subs.remove(&conn);
                        // Its declared `retain` goes with it — "stop sending me
                        // this shape" leaves nothing for a retain to bound, and a
                        // stale one would silently apply to a LATER subscribe from
                        // the same connection that asked for nothing.
                        self.retains.remove(&conn);
                        // Its `beat` and its gate state go with it, for the same
                        // reason: "stop sending me this shape" leaves no cadence
                        // to honour, and a `last_beat` kept for a gone connection
                        // would make its NEXT subscribe wait out an interval it
                        // never asked for.
                        self.beats.remove(&conn);
                        // **AND ITS LINK, WHICH IS NOT A PARAMETER.** "Stop sending
                        // me this shape" ends the link, and a LATER subscribe from
                        // this connection is a new one that states its own version:
                        // keeping the warning would make the next link's frames
                        // carry a verdict about a link that is over, and clearing it
                        // on a KEYED unsubscribe (below) would be the opposite
                        // error. Rule 2.6 is about a link, so this follows the link.
                        self.superseded.remove(&conn);
                        self.last_beat.remove(&conn);
                        self.armed.retain(|(_, s)| s.conn != conn);
                        self.clear_timers(conn);
                        self.clear_open_keys(conn);
                    }
                    // **A KEYED UNSUBSCRIBE LEAVES `retains` ALONE**, and that is
                    // the honest reading of a per-CONNECTION parameter: the
                    // connection still holds this shape's other keys under the
                    // retain it declared. Dropping it here would widen those keys'
                    // frames back to the ceiling because a different key went away.
                    Some(k) => {
                        self.subs.remove(&(conn, Some(k.clone())));
                        self.release_key(conn, &k);
                        let before = self.armed.len();
                        self.armed
                            .retain(|(_, s)| s.conn != conn || s.key.as_deref() != Some(k.as_str()));
                        for _ in 0..(before - self.armed.len()) {
                            self.release_timer(conn);
                        }
                    }
                }
                self.stop_if_idle();
            }
            Item::Dropped(conn) => {
                self.subs.retain(|(c, _)| *c != conn);
                self.member_subs.remove(&conn);
                // Connection ids are minted monotonically and never reused, so
                // an entry nobody removes is an entry nobody ever will — the
                // same reason `clear_timers` removes rather than zeroes.
                //
                // **UNPROVEN, AND DELIBERATELY RECORDED AS SUCH.** Deleting this
                // line leaves the whole suite GREEN. It is not a test that is
                // missing: because connection ids are never reused, a leaked
                // entry can never be read back by anybody, so the defect has NO
                // FRAME-LEVEL WITNESS at all — it is growth, not behaviour, and
                // a test that asserted on `self.retains` would be asserting that
                // a mechanism ran rather than that anything reached a consumer,
                // which is the exact trap this suite is built against. It sits
                // on the same footing as `clear_timers`' own removal, which is
                // equally unproven for equally good reasons.
                self.retains.remove(&conn);
                self.beats.remove(&conn);
                // On the same footing, and the same honest note applies: a leaked
                // entry here is growth with no frame-level witness, because
                // connection ids are never reused.
                self.superseded.remove(&conn);
                self.last_beat.remove(&conn);
                self.armed.retain(|(_, s)| s.conn != conn);
                self.clear_timers(conn);
                self.clear_open_keys(conn);
                self.stop_if_idle();
            }
        }
    }

    /// One armed timer for `conn` has fired: give its slot back.
    fn release_timer(&self, conn: ConnId) {
        let mut m = self.armed_timers.lock().unwrap();
        if let Some(n) = m.get_mut(&conn) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                // Removed rather than left at zero, so the map cannot grow by
                // one entry per connection this daemon has ever served.
                m.remove(&conn);
            }
        }
    }

    /// The connection unsubscribed or ended: its armed items are gone from
    /// `self.armed`, so its reservations go with them. This is also what keeps
    /// the map from leaking — connection ids are minted monotonically and never
    /// reused, so an entry nobody removes is an entry nobody ever will.
    fn clear_timers(&self, conn: ConnId) {
        self.armed_timers.lock().unwrap().remove(&conn);
    }

    /// One key of `conn`'s is closed: give its slot back. The entry is REMOVED
    /// when it empties rather than left as an empty set, for `clear_timers`' own
    /// reason — connection ids are minted monotonically and never reused, so a map
    /// entry nobody removes is one nobody ever will.
    fn release_key(&self, conn: ConnId, key: &str) {
        let mut m = self.open_keys.lock().unwrap();
        if let Some(set) = m.get_mut(&conn) {
            set.remove(key);
            if set.is_empty() {
                m.remove(&conn);
            }
        }
    }

    /// The connection unsubscribed wholesale or ended: every key it held is
    /// released at once.
    fn clear_open_keys(&self, conn: ConnId) {
        self.open_keys.lock().unwrap().remove(&conn);
    }

    async fn do_subscribe(&mut self, s: SubscribeItem) {
        if self.root.is_none() {
            self.fifo.set_tracking(true);
            let _ = self.init_blocking();
        }
        // The pending debounce for this key is FLUSHED before the export, so
        // the export is not overtaken by a `row` for a change it already
        // contains.
        self.flush_rows(s.key.as_deref());
        // ── `detail: keys`: THE POOL'S MEMBERSHIP, AND THEN STANDING INTEREST ─
        //
        // The `(conn, None)` entry `subs` already recorded for a keyless
        // subscribe now MEANS something on its own: standing membership
        // interest. `added`/`gone` for every key, present and future, reach it —
        // which is the sentence `docs/shapes.md` §7.2 asked for, in the place it
        // asked for it, and the reason a keyless subscribe is no longer blind to
        // a key that appears after it.
        //
        // No per-key `subs` entries are inserted, because no rows were asked
        // for: a membership subscriber that wants a row names the key, and that
        // is an ordinary keyed subscribe.
        // **THE DECLARED RETAIN, RECORDED BEFORE ANY FRAME IS BUILT** — so the
        // export that satisfies this very subscribe is already bounded by it.
        // Recorded even for `Retain::Max`, which overwrites a narrower one a
        // previous subscribe on this shape left behind: the last ask is the
        // consumer's current ask, and a stale narrower bound would silently keep
        // shortening frames it was never asked to shorten.
        //
        // **AND ONLY WHEN THERE WAS AN ASK.** `None` is the chassis's own lag
        // repair, which nobody requested, so there is nothing to record and the
        // connection's negotiated bound stands. Writing `Max` here on a repair is
        // the defect `SubscribeItem::retain` documents: it upgraded a consumer
        // that had just proved it could not keep up to the largest frames and the
        // fastest cadence the contract allows.
        if let Some(r) = s.retain {
            self.retains.insert(s.conn, r);
        }
        // Recorded even for `Beat::Max`, which overwrites a slower cadence an
        // earlier subscribe from this connection declared — a second subscribe on
        // one shape REPLACES the first one's ask, it does not intersect with it.
        // `None`, again, is not an ask.
        // ── RULE 2.6's WARNING, RECORDED BEFORE ANY FRAME IS BUILT ───────────
        //
        // So the export that satisfies this very subscribe already carries it. A
        // warning that started on the SECOND frame would be a link that looked
        // healthy for exactly as long as a consumer takes to read its first export,
        // which is the window a mirror decides what to draw in.
        //
        // **THE LAST STATED LINK WINS, EXACTLY AS THE LAST `retain` DOES — and the
        // `if let` is what protects the repair, not a first-wins rule.** A consumer
        // that subscribes again stating a different served version has made a new
        // claim, and honouring the older one would be this chassis telling a
        // consumer it is on a contract it just said it is not on.
        //
        // What must NOT happen is the chassis's own lag repair clearing the
        // warning: it states no version, so it arrives with `None`, and `None` is
        // not a statement about the link. `if let Some(..)` is the whole of that
        // guarantee — the repair writes nothing and the negotiated warning stands.
        // That is `SubscribeItem::retain`'s defect read onto this axis: a repair
        // that quietly improves a consumer's terms is invisible from the consumer's
        // side, because nothing refuses and nothing logs. Here it would quietly
        // DOWNGRADE a refusal-adjacent fact into a clean-looking link.
        //
        // **An earlier version of this line was `or_insert`, and it was removed for
        // being unwitnessed.** No test could tell the two apart — the `if let`
        // already covers the repair — so it was a rule nothing proved, disagreeing
        // with how every other per-connection ask on this task is written. The
        // behaviour that IS witnessed is the one spelled here: see
        // `a_second_link_restating_a_different_served_version_replaces_the_warning`.
        if let Some(sup) = s.superseded {
            self.superseded.insert(s.conn, sup);
        }
        if let Some(b) = s.beat {
            self.beats.insert(s.conn, b);
        }
        if s.detail == Detail::Keys {
            self.subs.insert((s.conn, None));
            self.member_subs.insert(s.conn);
            self.hydrate_members_once();
            self.export_members(s.conn, Some(s.id), chrono::Utc::now());
            self.publish();
            return;
        }
        // ── PER-KEY: A KEY THE ROOT DOES NOT HOLD IS A REQUEST ───────────────
        //
        // Only for `Hydrate::PerKey`. A `Whole` shape never reaches this, so
        // nothing existing changes — a keyed subscribe to an unheld key still
        // gets the empty export the Python mirror already holds as a row with
        // every cell `NotHydrated`.
        if S::HYDRATE == Hydrate::PerKey {
            if let Some(k) = s.key.clone() {
                if !self.holds_key(&k) {
                    self.hydrate_one_key(&k);
                }
            }
        }
        self.subs.insert((s.conn, s.key.clone()));
        let key = s.key.clone().or_else(|| {
            // A keyless subscribe on a keyed shape means "every key I know
            // about"; each gets its own export, in key order.
            None
        });
        let as_of = chrono::Utc::now();
        // **EVERY EXPORT THIS ITEM CAUSES NAMES THE SUBSCRIBE THAT ASKED FOR IT** —
        // rule 2.7's `exported`. A keyless subscribe yields one export per key and
        // ALL of them carry the one id, because all of them are what that one call
        // asked for; a consumer knows it has the whole answer when the exports stop,
        // which is the same thing the ordering rule already tells it.
        //
        // **AND THE LAST OF THEM SAYS SO** — `complete`. "A consumer knows it has
        // the whole answer when the exports stop" was unobservable: the next
        // frame is a live one or nothing, and "nothing" has no arrival time, so
        // the board built a deadline (`RESYNC_DEADLINE`) to detect it — the
        // timeout-and-retry policy rule 2.5 exists to keep out of clients.
        match key {
            Some(k) => self.export(Some(s.conn), Some(k), as_of, Some(s.id), Some(true), None),
            None => {
                // **ONE PAGE: THE FIRST `page` KEYS FROM `from`, IN KEY ORDER** —
                // the paging chain resolved at the door (`crate::wire::Paging`),
                // and `docs/chassis-resources.md` §11.1's key-range subscribe.
                // The root is a `BTreeMap`, so `from` is a range bound and the
                // producer holds nothing per subscriber: the same ask yields the
                // same keys, which is what lets rule 2.5's re-subscribe be the
                // whole recovery story for a paged read too.
                //
                // **A SHORT ANSWER SAYS IT IS SHORT, AND SAYS WHERE IT STOPPED** —
                // rule 2.8's fourth clause. The last frame carries `complete:
                // false` and `next`, the first key NOT exported; it does NOT
                // silently truncate, which is the lie the rule forbids, and it
                // does not refuse, because the page was inside the ceiling.
                let mut rest = self.root.as_ref().map(|r| r.keys()).unwrap_or_default();
                if let Some(from) = s.from.as_deref() {
                    rest.retain(|k| k.as_str() >= from);
                }
                let cap = s.page.max(1);
                let complete = rest.len() <= cap;
                let next = (!complete).then(|| rest[cap].clone());
                if !complete {
                    tracing::debug!(
                        shape = S::NAME,
                        remaining = rest.len(),
                        page = cap,
                        "a keyless subscribe answered one page; the last frame \
                         carries `complete: false` and `next`"
                    );
                }
                let keys: Vec<String> = rest.into_iter().take(cap).collect();
                let last = keys.len().saturating_sub(1);
                for (i, k) in keys.into_iter().enumerate() {
                    self.subs.insert((s.conn, Some(k.clone())));
                    let mark = (i == last).then_some(complete);
                    let tail = (i == last).then(|| next.clone()).flatten();
                    self.export(Some(s.conn), Some(k), as_of, Some(s.id), mark, tail);
                }
            }
        }
        self.publish();
    }

    /// **THE POOL'S MEMBERSHIP, READ ONCE PER EPOCH.**
    ///
    /// Only for a [`Hydrate::PerKey`] shape: a `Whole` root's members ARE its
    /// held keys (`Root::members`' default), so there is nothing to ask for and
    /// nothing to pay.
    fn hydrate_members_once(&mut self) {
        if self.members_hydrated || S::HYDRATE != Hydrate::PerKey {
            return;
        }
        // Set BEFORE the read, not after: a producer that refuses must not be
        // asked again on every subscribe of the epoch. `false` is an answer.
        self.members_hydrated = true;
        let as_of = chrono::Utc::now();
        let Some(root) = self.root.as_mut() else {
            return;
        };
        let (shape, producer) = (&mut self.shape, &self.producer);
        // Same reason as `init_blocking`: `block_in_place` PANICS on a
        // current-thread runtime, which is what `#[tokio::test]` gives.
        let got = match tokio::runtime::Handle::current().runtime_flavor() {
            tokio::runtime::RuntimeFlavor::CurrentThread => {
                shape.hydrates_members(producer, root, as_of)
            }
            _ => tokio::task::block_in_place(|| shape.hydrates_members(producer, root, as_of)),
        };
        if !got {
            tracing::warn!(
                shape = S::NAME,
                "a per-key shape was asked for its pool's membership and did not \
                 answer — exporting whatever state the root carries, which is \
                 `not_hydrated` unless the shape marked one"
            );
        }
        // A hydrate publishes nothing; the export IS the publication. This also
        // discards the `added` events the membership read itself minted — a key
        // that was already there did not just arrive.
        let _ = root.drain();
    }

    /// **THE MEMBERSHIP EXPORT, PAGED.**
    ///
    /// One frame per [`Shape::MAX_KEYS_PER_FRAME`] keys, every frame naming the
    /// subscribe that asked, the LAST one carrying `complete: true`. The chassis
    /// pages; there is no `from` parameter and no resume the consumer has to
    /// drive, because a page it was told about and cannot ask for would be the
    /// ack-then-silence class wearing a cursor.
    ///
    /// `next` on a non-final page names the first key of the page after it, so a
    /// mirror that lost a frame knows where the hole starts.
    fn export_members(&mut self, conn: ConnId, satisfies: Option<SubscribeId>, as_of: Timestamp) {
        let (Some(hub), Some(root)) = (self.hub.upgrade(), self.root.as_ref()) else {
            return;
        };
        let pages: Vec<MembersPage> = match root.members() {
            Membership::NotHydrated { reason } => vec![MembersPage::NotHydrated { reason }],
            Membership::Known { keys, complete } => {
                let per = S::MAX_KEYS_PER_FRAME.max(1);
                if keys.is_empty() {
                    // An empty pool is a COMPLETE answer of zero, exactly as
                    // `Hydration::Complete(vec![])` is — not a missing frame.
                    vec![MembersPage::Known {
                        keys: Vec::new(),
                        complete,
                        next: None,
                    }]
                } else {
                    let chunks: Vec<Vec<String>> =
                        keys.chunks(per).map(<[String]>::to_vec).collect();
                    let n = chunks.len();
                    chunks
                        .iter()
                        .enumerate()
                        .map(|(i, page)| MembersPage::Known {
                            keys: page.clone(),
                            // Only the LAST page can claim the answer is whole,
                            // and it claims it only if the READ was whole.
                            complete: i + 1 == n && complete,
                            next: chunks.get(i + 1).and_then(|c| c.first().cloned()),
                        })
                        .collect()
                }
            }
        };
        let n = pages.len();
        for (i, page) in pages.into_iter().enumerate() {
            let last = i + 1 == n;
            let whole = matches!(&page, MembersPage::Known { complete: true, .. });
            let frame = ShapeFrame {
                to: Some(conn),
                shape: S::NAME.to_string(),
                // **KEYLESS, AND NOT A ROW-LESS EXPORT.** `members` is what makes
                // this frame a membership frame; a row-less export carries
                // `row: None` and means "I do not hold this key", which is a
                // different claim (`crate::wire::MembersPage`).
                key: None,
                wire: ShapeWire {
                    promises: Vec::new(),
                    subscribes: satisfies.into_iter().collect(),
                    shape: S::NAME.to_string(),
                    key: None,
                    op: Op::Read,
                    at: None,
                    as_of: Some(as_of),
                    // **RULE 2.6's WARNING RIDES A MEMBERSHIP EXPORT TOO.** This
                    // frame is already addressed `to: Some(conn)`, so there is no
                    // grouping to do — and leaving it off would mean a
                    // `detail: keys` consumer on a superseded contract had a link
                    // that looked clean, which is the one consumer whose only
                    // frames are these.
                    epoch: self.epoch_for(self.superseded_of(conn)),
                    cells: None,
                    row: None,
                    // **NO `shared` HERE, DELIBERATELY.** A membership subscriber
                    // asked what exists, not what the shape's scalars are; a
                    // `shared` block on this frame would be rows arriving under
                    // `detail: keys`.
                    shared: None,
                    shared_cells: None,
                    event: None,
                    members: Some(page),
                    // Membership pages the chassis drives itself; `next` rides
                    // inside `members`, so the frame-level one is for rows only.
                    next: None,
                    complete: last.then_some(whole),
                },
            };
            hub.deliver(conn, &Arc::new(frame));
        }
    }

    /// Does the root declare this key right now?
    fn holds_key(&self, key: &str) -> bool {
        self.root
            .as_ref()
            .is_some_and(|r| r.keys().iter().any(|k| k == key))
    }

    /// **ONE KEY, HYDRATED BECAUSE SOMEBODY ASKED FOR IT** — the `PerKey` half
    /// of init. Same blocking treatment and the same "a hydrate publishes nothing"
    /// rule as `init_blocking`; a refusal is logged rather than swallowed, and the
    /// caller still exports, so the consumer receives a frame saying there is
    /// nothing for this key instead of receiving nothing at all.
    fn hydrate_one_key(&mut self, key: &str) {
        let as_of = chrono::Utc::now();
        let Some(root) = self.root.as_mut() else {
            return;
        };
        let (shape, producer) = (&mut self.shape, &self.producer);
        // Same reason as `init_blocking`: `block_in_place` PANICS on a
        // current-thread runtime, which is what `#[tokio::test]` gives.
        let got = match tokio::runtime::Handle::current().runtime_flavor() {
            tokio::runtime::RuntimeFlavor::CurrentThread => {
                shape.hydrates_key(producer, root, key, as_of)
            }
            _ => tokio::task::block_in_place(|| shape.hydrates_key(producer, root, key, as_of)),
        };
        if !got {
            tracing::warn!(
                shape = S::NAME,
                key,
                "an on-demand shape refused to hydrate a requested key — exporting \
                 a row-less frame so the consumer is told, rather than nothing"
            );
            return;
        }
        // A hydrate publishes nothing; the export IS the publication.
        let _ = root.drain();
        let keys = root.keys();
        self.info.keys = keys;
    }

    fn stop_if_idle(&mut self) {
        if S::TRACKING != Tracking::Lazy || !self.subs.is_empty() {
            self.publish();
            return;
        }
        // The last unsubscribe clears state and stops the FIFO. No grace
        // period; a reconnect re-hydrates.
        self.fifo.set_tracking(false);
        self.fifo.clear();
        self.root = None;
        self.members_hydrated = false;
        // Empty already — every door that empties `subs` empties this too — and
        // cleared here so the two cannot drift into disagreeing about who is
        // watching.
        self.member_subs.clear();
        self.pending_rows.clear();
        // ── **THE PENDING IDS ARE HANDED TO THE BOUNDED RECORD** ─────────────
        //
        // **THE DEFECT THIS FIXES (2026-09-26), AND THE ONE IT MUST NOT CAUSE.**
        // `pending_promises` means *a frame is about to carry this*, and the line
        // above just deleted every pending row, so nothing ever will. It survived
        // this function by omission and nothing bounded it, so its ids rode every
        // export of every LATER epoch — a fresh hydrate, a fresh `init_at`, stale
        // ids from a generation the consumer was not connected for — forever,
        // because an export copies rather than takes.
        //
        // **Clearing them is the wrong repair, and the suite says so.** Rule 2.5:
        // *"the last consumer leaving is exactly the moment those ids become the
        // only record that a command was answered"* — and
        // `a_promise_survives_the_connection_and_rides_the_re_export` phase 1 is
        // exactly that case, a socket dropped INSIDE the row debounce, where the
        // pending id is all there is. Dropping it here would re-open the recovery
        // hole 2.5's repair closed, one door along.
        //
        // So they MOVE, into `carried` — the record that says *a frame is owed
        // this id*, which is bounded twice and which deliberately outlives an
        // idle shape. One sentence changes at the moment of the idle ("about to
        // be carried" becomes "owed"), and with it the ids inherit
        // `PROMISE_RETENTION` and `PROMISE_RETENTION_PER_KEY` instead of nothing.
        // They are stamped with the IDLE MOMENT because a pending id has no stamp
        // of its own; that is the most generous honest reading and it is still
        // finite, where before there was no reading at all.
        let idled_at = chrono::Utc::now();
        let subjects: Vec<String> = self.pending_promises.keys().cloned().collect();
        for k in subjects {
            let ids = self.pending_promises.take_key(&k);
            self.remember_carried(Some(&k), idled_at, &ids);
        }
        let keyless = self.pending_promises.take_keyless();
        self.remember_carried(None, idled_at, &keyless);
        self.pending_promises = crate::wire::Resolving::none();
        self.current_promises = crate::wire::Resolving::none();
        self.debounce_deadline = None;
        self.info.epoch.state = ShapeState::NotHydrated;
        self.info.epoch.init_at = None;
        self.info.keys.clear();
        self.publish();
    }

    /// **Init: attach, then read, then fold forward forever.**
    ///
    /// Step order matters and the ordering argument needs only what is already
    /// true in `threads`: tracking is on BEFORE any read starts, and every
    /// store write happens-before its emit. So an event whose emit stamp is
    /// earlier than `as_of` had its write committed before the read began, and
    /// the read reflects it.
    ///
    /// The handlers are synchronous `rusqlite` reads plus one keychain sample —
    /// ~0.8s — so they run on a BLOCKING thread. 0.8s of blocking on a tokio
    /// worker is the critique's "handler execution model" finding.
    fn init_blocking(&mut self) -> Vec<String> {
        let as_of = chrono::Utc::now();
        let mut root = self.shape.root();
        // `block_in_place` rather than `spawn_blocking`: the shape and its root
        // are `&mut` on this task and moving them out and back would need them
        // `'static`. This is a multi-thread runtime, so `block_in_place` moves
        // the other tasks off this worker for the duration, which is the same
        // protection for the same reason.
        let (shape, producer) = (&mut self.shape, &self.producer);
        // **`block_in_place` IS NOT UNCONDITIONALLY AVAILABLE, AND THAT IS A
        // FINDING, NOT A DETAIL.**
        //
        // The design and the critique both say the ~0.8s of synchronous
        // `rusqlite` and keychain work must not sit on a tokio worker, and on
        // the daemon's own runtime (`#[tokio::main]`, multi-thread) it does
        // not: `block_in_place` moves the other tasks off this worker for the
        // duration. But `block_in_place` PANICS on a current-thread runtime,
        // which is what `#[tokio::test]` gives by default and what every
        // existing test in this crate runs on — so an unconditional call would
        // have made the shape task die on init in every test, and the death
        // path would have papered over it with a plausible-looking dead export.
        //
        // On a single-threaded runtime there are no sibling workers to protect,
        // so calling inline is correct there and merely slower. Asking the
        // runtime which one it is costs nothing and removes the panic.
        match tokio::runtime::Handle::current().runtime_flavor() {
            tokio::runtime::RuntimeFlavor::CurrentThread => {
                shape.hydrates(producer, &mut root, as_of)
            }
            _ => tokio::task::block_in_place(|| shape.hydrates(producer, &mut root, as_of)),
        }
        // Quiet drain: fold what arrived during the read, discarding derived
        // events — the consumer has not been told anything yet, so there is
        // nothing to update. CONTROL items are NOT discarded (the critique's
        // F8): a second connection's subscribe that landed during init is put
        // back so it still gets its export.
        //
        // ── **THE PROMISES ARE NOT DERIVED EVENTS AND ARE NOT DISCARDED** ────
        //
        // **THE DEFECT THIS FIXES (2026-09-26).** This loop folded each input
        // and threw its `Outbox` away, and it never touched `st.promises` — the
        // only `pending_promises.merge` in the file was in [`Self::fold`]. But
        // `Hub::subscribe` flips `tracking` BEFORE the task ever reaches this
        // function, so for the whole ~0.8 s of a cold hydrate every producer
        // gate reads `any_tracking() == true` and answers `Visible::Expected`.
        // A command issued in that window was told `visible: true` — *a frame
        // carrying this id will reach the subject's subscribers* — and then no
        // frame ever carried it, not the live frame (the `Outbox` was
        // discarded), not the export below (`pending_promises` was empty), and
        // not a later re-subscribe (`carried` was never written either). That
        // is the SAME CLASS as the not-tracking reply this rule closed, in a
        // different window, and it degrades to *forever*.
        //
        // **WHY THE DRAIN MERGES RATHER THAN THE GATE REFUSING.** `visible` is
        // not wrong here: a fold IS tracking, and an export IS about to go out
        // to the connection whose subscribe caused this hydrate. The honest
        // repair is to make that export carry the ids, which is the same
        // sentence this function already lives by — *a hydrate publishes
        // nothing; the export IS the publication* — extended to promises,
        // exactly as rule 2.5's "complete now includes promises" requires. The
        // alternative, answering `Visible::Never` while hydrating, would need a
        // second predicate read at a second instant (the disease
        // [`crate::command::Visible`]'s own doc forbids), would race init's
        // completion, and would deny the id to the one consumer certain to
        // receive a frame for it.
        //
        // The value is dropped rather than published, so nothing here emits: the
        // ids go into `pending_promises`, and [`Self::export`] — which runs a few
        // lines later, for the subscribe that triggered this init — is what puts
        // them on the wire.
        let n = self.fifo.len();
        let mut deferred: Vec<Item<I>> = Vec::new();
        for _ in 0..n {
            match self.fifo.try_pop() {
                Some(Item::Input(st)) => {
                    self.remember_pending(st.promises.clone());
                    let mut tx = Tx::new(&mut root, st.at);
                    self.shape.folds(&st, &mut tx);
                    let _ = root.drain();
                }
                Some(other) => deferred.push(other),
                None => break,
            }
        }
        for it in deferred {
            self.fifo.push_control(it);
        }
        let keys = root.keys();
        self.root = Some(root);
        self.info.epoch.init_at = Some(as_of);
        // **THE PARENT'S ERA, STAMPED ONTO THIS SHAPE'S OWN.** One relaxed load,
        // here and nowhere else, because this is the moment the state being stamped
        // was built. `None` for a root shape, which is every shape today, so no
        // frame any producer emits gains a field.
        self.info.epoch.parent_epoch = self
            .parent_gen
            .as_ref()
            .map(|g| g.load(Ordering::Relaxed));
        self.info.epoch.state = ShapeState::Known;
        self.info.keys = keys.clone();
        self.publish();
        keys
    }

    fn fold(&mut self, st: &Stamped<I>) {
        // Every frame this input causes carries the promises FOR ITS OWN SUBJECT
        // — the live ones below, and the debounced `row` when it flushes.
        self.current_promises = st.promises.clone();
        self.remember_pending(st.promises.clone());
        let (out, shared, keys) = {
            let Some(root) = self.root.as_mut() else {
                return;
            };
            {
                let mut tx = Tx::new(root, st.at);
                self.shape.folds(st, &mut tx);
            }
            let out = root.drain();
            let shared = out.shared.then(|| root.shared());
            (out, shared, root.keys())
        };

        // ── BURST DETECTION, the backstop ─────────────────────────────────
        // Nothing is suppressed — the frames still go out. The warning is the
        // signal that a fold re-emits keys it did not change, and it NAMES THE
        // INPUT KIND so the operator can act on it.
        if out.len() > S::BURST {
            self.info.bursts += 1;
            tracing::warn!(
                shape = S::NAME,
                input_kind = st.value.kind(),
                emitted = out.len(),
                burst = S::BURST,
                "burst"
            );
        }

        // Rows coalesce per key behind a short debounce; occurrences do not.
        for k in out.rows {
            self.pending_rows.insert(k);
        }
        if !self.pending_rows.is_empty() && self.debounce_deadline.is_none() {
            self.debounce_deadline =
                Some(tokio::time::Instant::now() + std::time::Duration::from_millis(S::DEBOUNCE_MS));
        }
        for (k, cell, item) in out.transitions {
            self.emit_live(
                Some(k),
                st.at,
                Derived::Transition {
                    // The producer names the cell with a `&'static str`; the frame
                    // owns its copy, because a wire type has to be parseable.
                    cell: cell.to_string(),
                    item,
                },
            );
        }
        // ── `added`, THE MEMBERSHIP TWIN OF `gone` ───────────────────────────
        //
        // Emitted BEFORE the rows, because a membership subscriber that upgrades
        // by naming the key must be able to name it as soon as it hears of it,
        // and a `row` for a key a consumer has not been told exists is a frame
        // it has no slot for. Never coalesced — an arrival is an occurrence
        // (rule 2.2), exactly as `gone` is.
        //
        // `drive` gates these: a keyed `added` reaches a connection holding that
        // key OR one holding standing membership interest in the shape, and
        // nobody else.
        // **AND IT CARRIES PROMISE IDS, for the same reason `gone` does.**
        //
        // A key that has just come into existence is, for a pool nobody holds
        // that key of, a key whose `row` will NEVER flush — the producer's fold
        // guards every row-touching arm on `holds`, so `added` is the only frame
        // it will ever produce. A promise routed to that subject would sit in
        // `pending_promises` until retention dropped it, and the command that
        // opened it would have answered `visible: true` about a frame that never
        // comes.
        //
        // That is exactly `threads/create`'s case, and the membership statement
        // IS its resolving fact: *this thread now exists* is what the command
        // caused and what a `detail: keys` subscriber is owed. Taking the ids
        // here rather than at the `row` flush is not a loss for a consumer that
        // also holds the key — `drive` delivers a keyed `added` to a connection
        // holding that key as well as to one holding membership interest, so it
        // gets the id sooner and on a frame it was going to receive anyway.
        for k in out.added {
            let ids = self.pending_promises.take_key(&k);
            self.emit_live_with(Some(k), st.at, Derived::Added, ids);
        }
        for k in out.gone {
            self.pending_rows.remove(&k);
            // **The `gone` frame is this key's last frame, so it is the carrier.**
            // No `row` will ever flush for a key that left, so its ids are TAKEN
            // here rather than left pending against a flush that cannot come.
            let ids = self.pending_promises.take_key(&k);
            self.emit_live_with(Some(k), st.at, Derived::Gone, ids);
        }
        if let Some(shared) = shared {
            // The keyless frame is the only carrier a shape-level id has, and
            // `drive` delivers it to every connection holding any key.
            let ids = self.pending_promises.take_keyless();
            self.emit_live_with(None, st.at, Derived::Shared { shared }, ids);
        }
        if keys != self.info.keys {
            self.info.keys = keys;
        }
        self.publish();
    }

    fn flush_rows(&mut self, only: Option<&str>) {
        let keys: Vec<String> = match only {
            Some(k) => self
                .pending_rows
                .take(k)
                .into_iter()
                .collect::<Vec<String>>(),
            None => std::mem::take(&mut self.pending_rows).into_iter().collect(),
        };
        let at = chrono::Utc::now();
        // **KEY `k`'s OWN IDS ON KEY `k`'s FRAME, and nobody else's.**
        //
        // This used to put the WHOLE accumulated bag on the first row frame of
        // the flush (`if first { resolving.clone() }`), on the reasoning that a
        // promise is resolved once by the arrival of its fact and not once per
        // key that fact touched. The reasoning was right and the conclusion did
        // not follow: `drive` delivers a keyed frame only to connections in
        // `shape_subs` for that key, so "the first key of the flush" is a frame a
        // consumer watching any other key never receives. A promise about `bb`
        // whose id left on `aa`'s frame is a promise nobody can resolve — and the
        // book has already closed it, so no later frame carries it either. The
        // ids are routed by subject from the emit onwards; this is where that
        // routing is spent.
        for k in keys {
            // **`row_at(Retain::Max)` ONLY TO ASK WHETHER THERE IS A ROW AT
            // ALL.** The bodies are built per audience-retain inside
            // `emit_live_row`; this is the existence question, and it is asked
            // at the ceiling so a subscriber's narrow ask can never be the
            // reason a frame is not sent.
            let held = self
                .root
                .as_ref()
                .and_then(|r| r.row_at(&k, Retain::Max))
                .is_some();
            let ids = self.pending_promises.take_key(&k);
            if held {
                self.emit_live_row(k, at, ids);
            } else {
                // No row frame for this key (it vanished between the fold and
                // the flush). Put its ids back rather than dropping them: an
                // unresolved promise is visible, a silently discarded one is not.
                self.pending_promises.put_back(k, ids);
            }
        }
        if self.pending_rows.is_empty() {
            self.debounce_deadline = None;
        }
    }

    /// **THE ONE DOOR FOR A NEW ID INTO `pending_promises`, AND THE ONLY PLACE
    /// ITS BOUND IS APPLIED.** (`Resolving::put_back` is the other writer and is
    /// deliberately not routed here: it restores ids this task took a moment ago
    /// and found no frame for, so it can never carry the set past a bound the
    /// take was already inside.)
    ///
    /// **THE DEFECT THIS FIXES (2026-09-26).** `carried` is bounded twice and
    /// `prune_carried` is a free function precisely so *"the write path and the
    /// read path cannot apply two different policies — which is how a bound
    /// becomes advisory."* Its sibling had NO policy at all: `pending_promises`
    /// was `merge`d into and only ever emptied by a `row` that actually flushed,
    /// so a subject whose row never flushes (the honest-failure case, and now
    /// every id the quiet drain rescues) accumulated without limit, past the
    /// documented [`PROMISE_RETENTION_PER_KEY`] cap, and [`Self::export`] COPIES
    /// rather than takes — so every export for that subject, forever, re-offered
    /// the whole pile.
    ///
    /// The cap is the same constant `carried` obeys, because it answers the same
    /// question — how many ids is a reconnecting consumer plausibly still waiting
    /// on — and two numbers for one question is the collapse wearing a second
    /// spelling. The oldest goes first, for `prune_carried`'s reason: the oldest
    /// id is the one a consumer is least likely to still be waiting on.
    ///
    /// There is no AGE half here and that is deliberate, not an omission: a
    /// pending id has no stamp of its own to age against, and giving it one would
    /// be a second record of a moment `carried` already holds. The count bound is
    /// what makes it finite; [`Self::stop_if_idle`] is what makes it mortal.
    fn remember_pending(&mut self, r: crate::wire::Resolving) {
        self.pending_promises.merge(r);
        self.pending_promises
            .cap_per_subject(PROMISE_RETENTION_PER_KEY);
    }

    /// **REMEMBER THAT THIS FRAME CARRIED THESE IDS**, so a re-export can carry
    /// them again. Prunes as it writes, which is the only place it needs to: a
    /// subject nothing resolves for never grows, and a subject that does is pruned
    /// on its next resolve and again on every read.
    fn remember_carried(&mut self, key: Option<&str>, at: Timestamp, ids: &[u64]) {
        if ids.is_empty() {
            return;
        }
        let slot = self
            .carried
            .entry(key.map(str::to_string))
            .or_insert_with(VecDeque::new);
        for id in ids {
            slot.push_back((at, *id));
        }
        prune_carried(slot, at);
    }

    /// **THE IDS AN EXPORT FOR `key` SHOULD RE-OFFER.** Pruned on read, so an
    /// export never re-offers an id past the window even if nothing has resolved
    /// for that subject since.
    fn carried_for(&mut self, key: Option<&str>, now: Timestamp) -> Vec<u64> {
        let Some(slot) = self.carried.get_mut(&key.map(str::to_string)) else {
            return Vec::new();
        };
        prune_carried(slot, now);
        let out: Vec<u64> = slot.iter().map(|(_, id)| *id).collect();
        if slot.is_empty() {
            self.carried.remove(&key.map(str::to_string));
        }
        out
    }

    fn emit_live(&mut self, key: Option<String>, at: Timestamp, event: Derived) {
        // The ids for THIS frame's subject. A copy, not a take: `emit_live` is
        // the transition path, and the resolving `row` that follows it out of the
        // same input is the frame the consumer's promise is keyed to. The keyless
        // case carries nothing here BY CONSTRUCTION — the one keyless frame a
        // fold produces is `shared`, which takes the keyless ids explicitly above,
        // so routing them here as well would deliver one id twice.
        let ids = match key.as_deref() {
            Some(k) => self.current_promises.for_key(k),
            None => Vec::new(),
        };
        self.emit_live_with(key, at, event, ids)
    }

    /// **THE CONNECTIONS THIS SHAPE COULD POSSIBLY OWE A FRAME**, in id order.
    ///
    /// The coarse filter, deliberately: any connection with any subscription to
    /// this shape. The fine one — "has this connection already had its own export
    /// for this `(shape, key)`" — stays in the producer's delivery loop, where it
    /// has always lived and where its only input is an export addressed to that
    /// connection. Moving it here would mean the task and the loop each held half
    /// of one gate, and the loop's half is the one the invariant is written
    /// against.
    ///
    /// So this narrows the audience from "every connection on the producer" to
    /// "every connection watching this shape", which is strictly better isolation
    /// than the shared broadcast had, and it changes what any connection is
    /// DELIVERED not at all.
    fn audience(&self) -> Vec<ConnId> {
        let mut v: Vec<ConnId> = self
            .subs
            .iter()
            .map(|(c, _)| *c)
            .collect::<BTreeSet<ConnId>>()
            .into_iter()
            .collect();
        v.dedup();
        v
    }

    /// Every connection holding `key` explicitly — the audience for a re-init's
    /// re-export of that key.
    fn holders_of(&self, key: &str) -> Vec<ConnId> {
        self.subs
            .iter()
            .filter(|(_, k)| k.as_deref() == Some(key))
            .map(|(c, _)| *c)
            .collect::<BTreeSet<ConnId>>()
            .into_iter()
            .collect()
    }

    fn emit_live_with(
        &mut self,
        key: Option<String>,
        at: Timestamp,
        event: Derived,
        promises: Vec<u64>,
    ) {
        // **RECORDED BEFORE IT IS SENT, and recorded even if the send finds no
        // audience.** A frame that reaches nobody is precisely the case the
        // retention exists for: the consumer that issued the command has already
        // gone, and the export it comes back for is the only carrier left.
        self.remember_carried(key.as_deref(), at, &promises);
        let Some(hub) = self.hub.upgrade() else {
            return;
        };
        let frame = ShapeFrame {
            to: None,
            shape: S::NAME.to_string(),
            key: key.clone(),
            wire: ShapeWire {
                promises,
                subscribes: Vec::new(),
                shape: S::NAME.to_string(),
                key,
                op: Op::Live,
                at: Some(at),
                as_of: None,
                epoch: self.info.epoch.clone(),
                cells: None,
                row: None,
                shared: None,
                shared_cells: None,
                event: Some(event),
                members: None,
                complete: None,
                next: None,
            },
        };
        // UNDIRECTED: one copy per connection watching this shape, down each
        // connection's own channel, in this task's send order.
        let frame = Arc::new(frame);
        for conn in self.audience() {
            hub.deliver(conn, &frame);
        }
    }

    /// **THE LIVE `row` FRAME, BUILT PER DISTINCT [`Retain`] IN THE AUDIENCE.**
    ///
    /// Split out of [`Self::emit_live_with`] because it is the one derived event
    /// whose BODY depends on who is receiving it. A `transition`, a `gone`, an
    /// `added` and a `shared` are each one statement with one spelling; a `row`
    /// carries every cell, windowed cells included, and a subscriber that
    /// declared `retain` declared it about these frames too. A retain that held
    /// only on the subscribe export would be the knob that pretends to work.
    ///
    /// The ids ride on EVERY copy: a promise is resolved by the arrival of its
    /// fact at the consumer holding the subject, and how much of a window that
    /// consumer asked to see is not a fact about the promise.
    fn emit_live_row(&mut self, key: String, at: Timestamp, promises: Vec<u64>) {
        self.remember_carried(Some(&key), at, &promises);
        let (Some(hub), Some(root)) = (self.hub.upgrade(), self.root.as_ref()) else {
            return;
        };
        // ── THE BEAT GATE, AND IT IS ONLY HERE ───────────────────────────────
        //
        // The rate projection: on a shape that declares a period, each connection
        // receives at most one live `row` per the interval it asked for. It is
        // DECIMATION and not coalescing, which is legal here and nowhere else
        // because a beat SUPERSEDES — the newest one says everything every dropped
        // one said. See [`crate::wire::Beat`] §5.
        //
        // **The export is never gated**, only the live path: an export answers a
        // subscribe, and a first frame withheld to honour a cadence is the
        // ack-then-silence class this whole design is against.
        //
        // **A frame carrying a promise id is never gated either.** A promise is
        // resolved by the arrival of its fact at the consumer holding the subject,
        // so dropping that frame would strand the id — the cadence a consumer
        // asked about its BEATS is not permission to withhold an answer to its
        // WRITE. A beating shape has no command today, so this guard is the one
        // that keeps the combination correct rather than merely absent.
        let audience: Vec<ConnId> = if S::MIN_BEAT_INTERVAL_MS == 0 || !promises.is_empty() {
            self.audience()
        } else {
            let now = tokio::time::Instant::now();
            let mut keep = Vec::new();
            for conn in self.audience() {
                let want = self
                    .beats
                    .get(&conn)
                    .copied()
                    .unwrap_or_default()
                    .interval_ms(S::MIN_BEAT_INTERVAL_MS);
                let due = match self.last_beat.get(&conn) {
                    // The FIRST live beat on a connection is always due: nothing
                    // has been withheld from it yet, and making it wait would put
                    // a silent gap at the start of every subscription.
                    None => true,
                    Some(last) => {
                        (now - *last).as_millis() as u64 + BEAT_SLACK_MS >= want
                    }
                };
                if due {
                    self.last_beat.insert(conn, now);
                    keep.push(conn);
                }
            }
            keep
        };
        let mut by: BTreeMap<Retain, Vec<ConnId>> = BTreeMap::new();
        for conn in audience {
            by.entry(self.retain_of(conn)).or_default().push(conn);
        }
        for (retain, conns) in by {
            let Some(row) = root.row_at(&key, retain) else {
                continue;
            };
            let frame = Arc::new(ShapeFrame {
                to: None,
                shape: S::NAME.to_string(),
                key: Some(key.clone()),
                wire: ShapeWire {
                    promises: promises.clone(),
                    subscribes: Vec::new(),
                    shape: S::NAME.to_string(),
                    key: Some(key.clone()),
                    op: Op::Live,
                    at: Some(at),
                    as_of: None,
                    epoch: self.info.epoch.clone(),
                    cells: None,
                    row: None,
                    shared: None,
                    shared_cells: None,
                    event: Some(Derived::Row { row }),
                    members: None,
                    complete: None,
                    next: None,
                },
            });
            for conn in conns {
                hub.deliver(conn, &frame);
            }
        }
    }

    /// **The export.** Because ONE task emits both the derived events and the
    /// export, and ONE receiver per connection delivers them in order, every
    /// derived event before the export precedes it and every one after follows
    /// it. The consumer rule is one line — hold the export, apply everything
    /// after it — and no version number exists anywhere. That ordering claim is
    /// exactly why every frame for a connection rides that connection's ONE
    /// channel; see [`ConnFrames`], which is where a second channel was ruled out.
    ///
    /// **`to` is a delivery instruction now, not only a label.** `Some(conn)` is
    /// delivered to that connection and to nobody else, which is the whole of the
    /// capacity-isolation fix: a directed export consumes one slot in one buffer,
    /// not one in every buffer on the producer.
    fn export(
        &mut self,
        to: Option<ConnId>,
        key: Option<String>,
        as_of: Timestamp,
        satisfies: Option<SubscribeId>,
        complete: Option<bool>,
        next: Option<String>,
    ) {
        // ── THE EXPORT IS THE CARRIER OF LAST RESORT FOR A PROMISE ───────────
        //
        // It used to hard-code `promises: Vec::new()`, which is the whole of the
        // recovery hole — see [`Self::carried`]. It now carries, for this frame's
        // subject:
        //
        // * ids whose fact has folded and whose `row` has not flushed yet
        //   (`pending_promises`), COPIED rather than taken: the flush still owes
        //   them to every OTHER connection holding the key, and a consumer
        //   receiving one id twice ignores the second;
        // * ids a frame has already carried, inside the retention window — which is
        //   what makes a re-subscribe answer a promise whose live frame was lost;
        // * the same two for the SHAPE level, because a keyless id belongs to any
        //   subscriber of the shape and an export is one, and because the keyless
        //   `shared` frame that normally carries them only fires when `shared`
        //   moves.
        let mut promises: Vec<u64> = Vec::new();
        if let Some(k) = key.as_deref() {
            promises.extend(self.pending_promises.for_key(k));
            promises.extend(self.carried_for(Some(k), as_of));
        }
        promises.extend(self.pending_promises.keyless_ids());
        promises.extend(self.carried_for(None, as_of));
        promises.sort_unstable();
        promises.dedup();
        let (Some(hub), Some(root)) = (self.hub.upgrade(), self.root.as_ref()) else {
            return;
        };
        // ── ONE FRAME PER DISTINCT `retain` IN THE AUDIENCE ──────────────────
        //
        // A `retain` that held only on the first frame would be the knob that
        // pretends to work, so a broadcast is built per distinct retain rather
        // than once — and the count of distinct retains is bounded by the number
        // of connections, not by the ask, because the projection is read-only:
        // `Root::row_at` never evicts, so every frame here is built from the same
        // untouched cells. Nobody's ask is chargeable to anybody else.
        // ── AND ONE FRAME PER DISTINCT LINK IN THE AUDIENCE — RULE 2.6 ───────
        //
        // The grouping key gained [`crate::wire::Superseded`] beside the retain,
        // and the SAME affordability argument carries it: the count of distinct
        // groups is bounded by the number of connections and NOT by anything a
        // consumer can ask for, because a link is settled once and cannot be
        // re-negotiated per frame. In practice it adds no groups at all on a
        // producer nobody links to on an old version — `superseded_of` answers
        // `None` for every connection and the key is the retain alone.
        //
        // **The alternative was `superseded` on the subscribe REPLY, and rule 2.6
        // rules it out**: the warning has to ride every export for the life of the
        // link, or a consumer that reads its reply and forgets is a dangling
        // consumer that looks connected — *"the exact failure the three-valued
        // unknown exists to prevent"*.
        type LinkGroup = (Retain, Option<crate::wire::Superseded>);
        let targets: Vec<(LinkGroup, Vec<ConnId>)> = match to {
            Some(conn) => vec![((self.retain_of(conn), self.superseded_of(conn)), vec![conn])],
            None => {
                let mut by: BTreeMap<LinkGroup, Vec<ConnId>> = BTreeMap::new();
                for conn in self.audience() {
                    by.entry((self.retain_of(conn), self.superseded_of(conn)))
                        .or_default()
                        .push(conn);
                }
                by.into_iter().collect()
            }
        };
        for ((retain, superseded), conns) in targets {
            let (row, cells) = match &key {
                Some(k) => (root.row_at(k, retain), root.cells(k)),
                None => (None, None),
            };
            let frame = Arc::new(ShapeFrame {
                to,
                shape: S::NAME.to_string(),
                key: key.clone(),
                wire: ShapeWire {
                    promises: promises.clone(),
                    subscribes: satisfies.into_iter().collect(),
                    shape: S::NAME.to_string(),
                    key: key.clone(),
                    op: Op::Read,
                    at: None,
                    as_of: Some(as_of),
                    epoch: self.epoch_for(superseded),
                    cells,
                    row,
                    shared: Some(root.shared()),
                    shared_cells: Some(root.shared_cells()),
                    event: None,
                    // A ROW export never carries a membership page — the two
                    // frames mean two things and share no bytes.
                    members: None,
                    complete,
                    next: next.clone(),
                },
            });
            for conn in conns {
                hub.deliver(conn, &frame);
            }
        }
    }

    /// What this connection declared, or the ceiling. Absent and
    /// [`Retain::Max`] are the same frame, which is why absence needs no
    /// separate spelling.
    fn retain_of(&self, conn: ConnId) -> Retain {
        self.retains.get(&conn).copied().unwrap_or(Retain::Max)
    }

    /// **RULE 2.6's WARNING FOR THIS CONNECTION, OR NONE** — the per-connection
    /// half of the epoch on an export.
    ///
    /// Absent means *there is nothing to warn about*: it asked for the current
    /// version, or it stated none. Unlike [`Self::retain_of`] there is no ceiling
    /// to fall back to, because a link is not a quantity.
    fn superseded_of(&self, conn: ConnId) -> Option<crate::wire::Superseded> {
        self.superseded.get(&conn).copied()
    }

    /// **THE SHAPE'S EPOCH WITH ONE CONNECTION'S LINK STAMPED ON IT** — rule 2.6.
    ///
    /// Everything else on an [`Epoch`] is a fact about the FOLD and is identical
    /// for every subscriber; `superseded` is the one field that is a fact about the
    /// connection, so it is applied here, at the point a frame is addressed, rather
    /// than held on `self.info.epoch` where it would be a per-connection fact
    /// living in a shape-wide place and would leak onto `shapes.list`.
    ///
    /// A `None` link returns the epoch unchanged, which is the overwhelming case
    /// and is why no byte of any existing export moved.
    fn epoch_for(&self, superseded: Option<crate::wire::Superseded>) -> Epoch {
        let mut epoch = self.info.epoch.clone();
        epoch.superseded = superseded;
        epoch
    }

    fn publish(&mut self) {
        // **DISTINCT CONNECTIONS, not `(conn, key)` pairs.** A keyless subscribe
        // on a keyed shape inserts one pair per key, so the pair count reports
        // one board as N watchers. `wire::ShapeInfo::subscribers` carries the
        // reasoning and the history; `subscribers_counts_connections_and_not_
        // conn_key_pairs` is the test that can tell the two apart.
        self.info.subscribers = self
            .subs
            .iter()
            .map(|(c, _)| *c)
            .collect::<HashSet<_>>()
            .len();
        self.info.tracked = self.fifo.tracking();
        self.info.fifo_high_water = self.fifo.high_water();
        let _ = self.info_tx.send(self.info.clone());
    }
}

/// **BOTH BOUNDS ON THE PROMISE RETENTION, IN ONE PLACE.**
///
/// Age first, then the cap, and the cap drops from the FRONT: the oldest id is the
/// one a reconnecting consumer is least likely to still be waiting on. A free
/// function rather than a method so the write path and the read path cannot apply
/// two different policies — which is how a bound becomes advisory.
fn prune_carried(slot: &mut VecDeque<(Timestamp, u64)>, now: Timestamp) {
    while slot
        .front()
        .is_some_and(|(at, _)| now - *at > PROMISE_RETENTION)
    {
        slot.pop_front();
    }
    while slot.len() > PROMISE_RETENTION_PER_KEY {
        slot.pop_front();
    }
}

/// `sleep_until` on a wall-clock stamp, clamped so a conversion cannot
/// overflow tokio's `Instant` (the bound is enforced at `Hub::subscribe`; this
/// is the second lock).
async fn sleep_until_stamp(t: Timestamp) {
    let now = chrono::Utc::now();
    let delta = (t - now).num_milliseconds().clamp(0, 24 * 60 * 60 * 1000) as u64;
    tokio::time::sleep(std::time::Duration::from_millis(delta)).await;
}

/// The runtime's own suite, in `hub/tests.rs` beside this file — the
/// arrangement `fold`, `ledger` and `mirror` already use in this crate. 32
/// assertions: substrate's three inline `ShapeInfo`-on-the-wire tests, plus
/// the 29 it kept in a crate-level `tests/` directory this crate does not
/// have.
#[cfg(test)]
mod tests;
