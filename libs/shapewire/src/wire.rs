//! # `wire` — the frame types, and nothing a producer needs a runtime for
//!
//! This is the floor of the floor. Every type that crosses the socket is
//! declared here: the stamps, the three-valued cell state, the derived events,
//! the export frame, the introspection frame, the refusals, and the subscriber
//! row that rides inside `shapes.list`. A face vendors this module with a
//! `Cargo.toml` it writes itself, so the only dependencies reachable from here
//! are `serde`, `serde_json` and `chrono` — no tokio, no axum, no `env`, no
//! socket and no clock.
//!
//! Every type here is both `Serialize` **and** `Deserialize`. That pairing is
//! the capability the module exists for: a consumer that can emit a frame and
//! not read one cannot mirror a shape, and the first cut of this chassis in
//! substrate shipped exactly that — `Serialize` only, so of the eight things a
//! mirror needs, five sat behind tokio and none could be parsed.
//!
//! ## The line is drawn on ROLE
//!
//! `Op`, `ShapeState`, `Epoch`, `ShapeWire`, `ShapeInfo` and `ShapeError` are
//! inert data structs. They have no async anywhere in them; in substrate they
//! lived behind the tokio flag only because that is where the file they were
//! carved out of happened to end. A consumer needs every one of them and a
//! runtime for none of them, so they are unflagged here.
//!
//! ## Two fields are `String` where a producer names them with `&'static str`
//!
//! `ShapeInfo::tracking` and `Derived::Transition::cell`. `&'static str` has no
//! `Deserialize` impl at all, so a frame carrying one is a frame a consumer
//! cannot parse — which is not a wire type. Both serialize as the same JSON
//! string either way, and the golden-bytes tests at the bottom of this file
//! pin that.
//!
//! ## What is frozen
//!
//! All of it. Two faces and a live service talk this protocol and the
//! conformance transcripts are byte comparisons, so every field name, variant
//! name, serde attribute, declaration order, numeric type and refusal sentence
//! in this file is the contract rather than a choice. The `GOLD_*` constants in
//! the test module are substrate's own output, and a red one means a producer's
//! wire moved.
//!
//! The rules this file implements are `docs/shapes.md` 2.1–2.9 in
//! substrate-interface-layer; where a doc comment names a rule number, that is
//! where it lives.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─────────────────────────────────────────────────────────────────────────────
// 1. Stamps
// ─────────────────────────────────────────────────────────────────────────────

/// The chassis's clock. One clock, deliberately: the subscriber ledger stamps
/// every row with `Utc::now()`, so a shape reading anything else would be
/// comparing two clocks and calling the difference a fact.
pub type Timestamp = chrono::DateTime<chrono::Utc>;

/// A websocket connection's identity, minted per connect by the door.
pub type ConnId = u64;

/// **PROMISE IDS, ROUTED BY THE SUBJECT THEY ARE ABOUT** (rule 2.4).
///
/// A promise is always about something — the account `accounts.disable` was
/// called on, the thread whose log moved. The fact that resolves it arrives on
/// the frame for THAT subject's key, and the door delivers a keyed frame only
/// to connections holding that key. So a bag of ids with no notion of subject
/// is a bag that cannot be delivered: in substrate it rode whichever key
/// flushed first, and a consumer subscribed to any other key waited for ever.
/// This type is that repair, and it is the only way an id can reach the input —
/// there is no constructor taking a bare list and no public field to push onto.
///
/// ## Why a struct and not `BTreeMap<Option<String>, Vec<u64>>`
///
/// 1. **The keyless slot must not be spellable as a key.** A sentinel (`""`,
///    `"*"`) is a string a producer could legitimately mint as a key, and then
///    one subject silently collides with the shape level. Two fields make that
///    unrepresentable.
/// 2. **`Option<String>` is not a JSON map key.** This type round-trips through
///    `serde_json`, which refuses a non-string key at runtime — so the map
///    shape would compile and fail precisely where the test proves it works.
/// 3. The routing bookkeeping wants a home. [`Self::take_key`],
///    [`Self::put_back`] and [`Self::merge`] are the whole of it, and they
///    belong beside the invariant they keep rather than spread through the
///    shape task.
///
/// ## Keyed and keyless are both real
///
/// A keyed id rides the `row` (or `gone`, or transition) frame for its key. A
/// keyless id — a promise about the SHAPE and not about any one key — rides the
/// keyless `shared` frame, which reaches every connection holding any key of
/// the shape.
///
/// **A subject is a KEY of the shape, in the shape's own spelling**, because
/// the spelling is what the routing matches on. A producer whose fold re-keys
/// its input must name the key the fact will fold under, not the alias the call
/// arrived with. An id filed under a key that produces no frame is NOT handed
/// to some other key as a consolation: it stays pending and the promise stays
/// visibly unanswered, which is the failure an operator can act on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Resolving {
    /// Subject key -> the ids the frame for that key carries.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    by_key: BTreeMap<String, Vec<u64>>,
    /// Ids whose promise named no subject. They belong to the shape and ride a
    /// keyless frame.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    keyless: Vec<u64>,
}

impl Resolving {
    /// Nothing is resolved — which is almost every input there has ever been.
    pub fn none() -> Self {
        Self::default()
    }

    /// These ids are about one subject, so they belong to that key's frame. An
    /// empty list mints no subject at all.
    pub fn keyed(key: impl Into<String>, ids: Vec<u64>) -> Self {
        let mut r = Self::default();
        if !ids.is_empty() {
            r.by_key.insert(key.into(), ids);
        }
        r
    }

    /// These ids are about the SHAPE, not about any key.
    pub fn shape_level(ids: Vec<u64>) -> Self {
        Self {
            by_key: BTreeMap::new(),
            keyless: ids,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty() && self.keyless.is_empty()
    }

    /// Fold another routing in, per subject. The shape task accumulates this
    /// way across the inputs that coalesce behind one debounce.
    pub fn merge(&mut self, other: Self) {
        for (key, ids) in other.by_key {
            self.by_key.entry(key).or_default().extend(ids);
        }
        self.keyless.extend(other.keyless);
    }

    /// **CAP EACH SUBJECT'S PENDING LIST, DROPPING THE OLDEST FIRST.**
    ///
    /// The hub's pending set is merged into on every resolve and emptied only
    /// by a frame that actually flushes, so in substrate a subject whose frame
    /// never came grew without bound and rode every subsequent export for that
    /// subject. `cap` is the hub's `PROMISE_RETENTION_PER_KEY`, reused rather
    /// than restated: the retained-promise record already answers the same
    /// question with that number, and two numbers for one question is the
    /// collapse this file is written against.
    ///
    /// The FRONT goes. Ids are appended in resolve order, so the oldest is the
    /// one a consumer is least likely to still be waiting on — the same reason
    /// the hub's carried-promise prune drops its front. A subject emptied by
    /// the cap is REMOVED, so [`Self::keys`] keeps meaning *has something to
    /// deliver*.
    pub fn cap_per_subject(&mut self, cap: usize) {
        for ids in self.by_key.values_mut() {
            if ids.len() > cap {
                ids.drain(..ids.len() - cap);
            }
        }
        self.by_key.retain(|_, ids| !ids.is_empty());
        if self.keyless.len() > cap {
            let drop = self.keyless.len() - cap;
            self.keyless.drain(..drop);
        }
    }

    /// **Key `key`'s own ids and nobody else's**, removed from this routing, so
    /// one id cannot ride two keys' frames.
    pub fn take_key(&mut self, key: &str) -> Vec<u64> {
        self.by_key.remove(key).unwrap_or_default()
    }

    /// Key `key`'s own ids, left in place — for a frame that copies rather than
    /// consumes. A transition frame does; the resolving `row` does not.
    pub fn for_key(&self, key: &str) -> Vec<u64> {
        self.by_key.get(key).cloned().unwrap_or_default()
    }

    /// Put ids back under their key, because the caller found no frame to carry
    /// them. An unresolved promise is visible; a silently dropped one is not.
    /// Putting back nothing does nothing, so no empty subject is minted.
    pub fn put_back(&mut self, key: String, ids: Vec<u64>) {
        if ids.is_empty() {
            return;
        }
        self.by_key.entry(key).or_default().extend(ids);
    }

    /// The shape-level ids, removed.
    pub fn take_keyless(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.keyless)
    }

    /// The shape-level ids, left in place — for a frame that COPIES. An export
    /// does: it is the carrier of last resort for every id still owed, and
    /// consuming them there would starve the `shared` frame that is their
    /// primary carrier on every OTHER connection.
    pub fn keyless_ids(&self) -> Vec<u64> {
        self.keyless.clone()
    }

    /// Every id in this routing, keys in order then keyless. Diagnostics and
    /// tests only: no frame carries this, because a frame has one key.
    pub fn ids(&self) -> Vec<u64> {
        let mut all: Vec<u64> = self.by_key.values().flatten().copied().collect();
        all.extend(self.keyless.iter().copied());
        all
    }

    /// The subjects named here, which is exactly the set of keys whose frames
    /// owe an id. A subject with an empty list never appears, so *names a
    /// subject* and *has something to deliver for it* are one statement.
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.by_key.keys()
    }
}

/// An input with the producer's stamp on it.
///
/// **The stamp is on the FIFO ITEM, never on the input value.** That is the
/// whole reason a migrated producer's legacy wire stays byte-identical: nothing
/// a consumer already parses gains a field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stamped<T> {
    pub at: Timestamp,
    pub value: T,
    /// **The promises this input resolves, routed by subject** (rule 2.4).
    ///
    /// A command answers with an acknowledgement — `{promise_id}` — and NEVER
    /// with the fact it caused. The fact arrives where every other fact
    /// arrives, through the shape, and the frame that carries it carries these
    /// ids; so a consumer can match *it took* to the call it issued without the
    /// producer offering a second source for the value.
    ///
    /// A [`Resolving`] rather than a list because WHICH frame carries an id is
    /// the whole question: a consumer holding key B receives key B's frames and
    /// nothing else. Empty in the ordinary case, and therefore absent from the
    /// wire.
    #[serde(default, skip_serializing_if = "Resolving::is_empty")]
    pub promises: Resolving,
}

impl<T> Stamped<T> {
    /// The ordinary input: a stamp and a value, resolving nothing.
    pub fn new(at: Timestamp, value: T) -> Self {
        Self {
            at,
            value,
            promises: Resolving::none(),
        }
    }

    /// The input a command caused, carrying the promises its arrival closes and
    /// the subject each one is about.
    pub fn resolving(at: Timestamp, value: T, promises: Resolving) -> Self {
        Self {
            at,
            value,
            promises,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Cell state — the three-valued unknown
// ─────────────────────────────────────────────────────────────────────────────

/// **THE THREE STATES ONE CELL CAN BE IN, AS A SUBSCRIBER SEES THEM.**
///
/// *Not hydrated* and *dead* are different claims and a mirror must be able to
/// render two different `?`s, so the distinction is on the wire rather than
/// left to each consumer to invent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CellState {
    /// A value has reached this cell. `folded_at` is the stamp of the last
    /// input — or of the hydrate's `as_of` — that set it.
    Known {
        /// `false` when the hydrate that populated it was partial.
        complete: bool,
        folded_at: Timestamp,
        /// This cell had no hydrate entry, so it covers only what the shape has
        /// seen since init — and on a lazy shape, since the first subscribe.
        ///
        /// Skipped on the way out and defaulted on the way in: the same fact
        /// stated from both ends, which is what lets a consumer parse a frame
        /// that omits the field. It changes no byte a producer writes.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        since_init: bool,
        /// The oldest stamp a windowed cell still retains, which is the honest
        /// bound on any count a consumer derives from it.
        #[serde(skip_serializing_if = "Option::is_none")]
        covers_since: Option<Timestamp>,
    },
    /// Nothing has ever populated this cell: no hydrate reached it — the
    /// handler errored, or answered no row for this key — and no live input has
    /// set it. **Renders `?`.**
    NotHydrated {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The fold that maintained this cell is gone. Every value under it is the
    /// last one it managed to compute and must not be shown as current.
    /// **Renders `?`, differently.**
    Dead { reason: String },
}

impl CellState {
    /// A live emit or a hydrate read moves any state to `Known`. Crate-private:
    /// only a write cell may mint this, and a consumer only ever parses one.
    pub(crate) fn known(at: Timestamp) -> Self {
        CellState::Known {
            complete: true,
            folded_at: at,
            since_init: false,
            covers_since: None,
        }
    }

    pub fn folded_at(&self) -> Option<Timestamp> {
        match self {
            CellState::Known { folded_at, .. } => Some(*folded_at),
            _ => None,
        }
    }

    pub fn is_known(&self) -> bool {
        matches!(self, CellState::Known { .. })
    }
}

impl Default for CellState {
    /// Nothing known and no reason to give: the state a cell is in before
    /// anything has touched it.
    fn default() -> Self {
        CellState::NotHydrated { reason: None }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. The derived events
// ─────────────────────────────────────────────────────────────────────────────

/// One derived event, as the chassis generates it. **A shape author never hand
/// writes one**: they fall out of what the fold emitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Derived {
    /// The whole row for a key. It is an idempotent upsert for the consumer,
    /// which is why coalescing many cell writes into one `row` loses nothing.
    Row { row: Value },
    /// A key left the shape.
    Gone,
    /// **A key JOINED THE POOL** — the membership twin of [`Self::Gone`].
    ///
    /// A claim about EXISTENCE, carrying no row: a member is a key the producer
    /// says is there, whether or not anybody holds it. A consumer at
    /// `detail: rows` upgrades by naming the key; one at `detail: keys` writes
    /// it into its roster and is done.
    ///
    /// **Why a second event and not a row-less `row`.** An export already emits
    /// `row: None, cells: None` for a key the root does not hold, and *I do not
    /// hold this key* and *this key exists* are two different claims. One
    /// spelling for two claims is the collapse every repair in this file is
    /// against.
    Added,
    /// One item appended to an edge-shaped cell. `cell` is a `String` and not
    /// the `&'static str` the producer declares it with, because `&'static str`
    /// has no `Deserialize` impl and a frame a consumer cannot parse is not a
    /// wire type. The JSON is the same; the producer's outbox still carries the
    /// static name.
    Transition { cell: String, item: Value },
    /// A shape-level (non-keyed) scalar moved.
    Shared { shared: Value },
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. Frames, parameters and introspection
// ─────────────────────────────────────────────────────────────────────────────

/// What kind of read a frame is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// An export: a labelled read of current state.
    Read,
    /// A derived event: an occurrence.
    Live,
}

/// The shape-level twin of [`CellState`] — the same three values, for the whole
/// fold rather than for one cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeState {
    /// Hydrated and folding.
    Known,
    /// Registered and never initialised: lazy, and nobody has subscribed.
    NotHydrated,
    /// The task is gone. Everything under it is last-known, not current.
    Dead,
}

/// **THE SHAPE'S OWN ERA, ON EVERY FRAME IT EMITS.** How many times it has
/// re-initialised, how many inputs it has lost, whether it is alive at all —
/// the bookkeeping a mirror needs to know whether the state it holds is still
/// from the run that produced it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Epoch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_at: Option<Timestamp>,
    pub reinit: u64,
    pub lost: u64,
    pub state: ShapeState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dead_reason: Option<String>,
    /// **THE PARENT'S `reinit` COUNT AS OF THIS SHAPE'S OWN LAST INIT** — the
    /// one thing a consumer holding both a parent mirror and a child mirror
    /// needs in order to answer *are these two from the same era*.
    ///
    /// `None`, and so absent from the wire, for a shape with no parent — which
    /// is every root shape, so nothing any producer emits moved when this field
    /// came into existence.
    ///
    /// ## The epoch ruling for nesting
    ///
    /// **INDEPENDENT. A parent re-init does NOT invalidate its children, and
    /// neither one's death touches the other.** The opposite instinct — couple
    /// them, because a parent re-init means the producer lost confidence in the
    /// key — was put up to be tested rather than adopted, and it does not
    /// survive the test:
    ///
    /// 1. **A parent re-init does not mean that.** Exactly one thing causes a
    ///    re-init: the shape's OWN FIFO overflowed and dropped inputs. The FIFO
    ///    drops and counts PER SHAPE, each shape fed by its own queue in
    ///    arrival order, so a parent's overflow is evidence about the parent's
    ///    queue and about nothing else. A child fed the same inputs into an
    ///    un-overflowed queue missed nothing; a child that DID overflow
    ///    re-inits on its own account already.
    /// 2. **Coupling amplifies, and that class of thing is why the re-init
    ///    floor exists.** A fleet-level parent under sustained overload
    ///    re-inits every `REINIT_FLOOR_SECS`; coupled, each of those re-hydrates
    ///    every subscribed child. One overloaded parent times N open children
    ///    is N real reads per five seconds, and an open child is somebody's
    ///    open thread. That is the livelock the floor was added to stop,
    ///    rebuilt one level down and multiplied.
    /// 3. **It contradicts the property nesting is for**: a nested shape nobody
    ///    subscribes to costs nothing, and one somebody does subscribe to
    ///    should cost what IT needs rather than what an unrelated sibling's
    ///    parent's load happens to make it cost.
    ///
    /// **This field is what makes independence safe.** A key that vanished
    /// during the parent's re-hydrate is a `gone` on the parent, which a
    /// consumer already handles. What independence genuinely loses is the
    /// ability to tell whether a parent summary and a child row were built
    /// under the same parent era — so the child stamps the parent's era onto
    /// every frame it emits, and the comparison is one integer:
    ///
    /// * equal to the parent's live `reinit` → same era, and the two may be
    ///   read together;
    /// * less than it → the parent has re-inited since this child last did.
    ///   **The child is still valid on its own terms**, which is the ruling,
    ///   but anything the consumer inferred FROM THE PARENT about this child is
    ///   from before. The repair is to re-read the parent, never the child.
    ///
    /// **Where this is most likely to be wrong**, said plainly because it is
    /// the thinnest-evidenced decision in the nesting design: the value is
    /// sampled at the child's INIT and not maintained afterwards, so a child
    /// that is subscribed and quiet keeps reporting a parent era that has moved
    /// on. That is honest — it says when the child's state was built — but it
    /// is not *the parent has re-inited since you last looked*, and a consumer
    /// wanting that compares against the parent's live `reinit`, which it has
    /// on every parent frame. If this turns out to be the wrong half to store,
    /// the fix is to re-stamp on each export, never to couple the re-inits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_epoch: Option<u64>,
    /// **THIS CONNECTION IS LINKED ON A VERSION THIS PRODUCER NO LONGER CALLS
    /// CURRENT** — rule 2.6's MIDDLE state, and the field that stops it being a
    /// silent accommodation.
    ///
    /// `None`, and therefore absent, on every link that asked for
    /// [`ShapeContract::version`] and on every link that stated no version at
    /// all — which is every link any producer has had, so no byte of any
    /// existing export moved. Same property as [`Self::parent_epoch`] and here
    /// for the same reason: a new fact about a minority of links must not
    /// rewrite the frames of the majority.
    ///
    /// **It is on the EPOCH and not on the subscribe reply, which is the
    /// point.** A reply is read once, by the code that asked; an epoch rides
    /// every export for the life of the link, so a mirror cannot hold a
    /// superseded contract and forget it, and a human reading one frame can see
    /// it. Rule 2.6 says *serves with warning*, and a warning delivered once to
    /// a caller free to discard it is the clamp the rule forbids, in a coat.
    ///
    /// **Per CONNECTION, so it is the one field here that is not a property of
    /// the fold.** The hub's export already builds one frame per distinct
    /// retain in the audience and groups the connections that share one; this
    /// joins that grouping key. The bound does not move — the number of groups
    /// is bounded by the number of connections, never by anything a consumer
    /// can ask for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded: Option<Superseded>,
}

impl Default for Epoch {
    /// Registered, never initialised, nothing lost: what a lazy shape publishes
    /// before anybody subscribes.
    fn default() -> Self {
        Self {
            init_at: None,
            reinit: 0,
            lost: 0,
            state: ShapeState::NotHydrated,
            dead_reason: None,
            parent_epoch: None,
            superseded: None,
        }
    }
}

/// **A LINK SERVED ON AN OLDER CONTRACT, NAMING BOTH VERSIONS** — rule 2.6's
/// `superseded: {wanted, current}`.
///
/// Both numbers, never one. *You are old* is not actionable; *you asked for 3
/// and I am on 4* is. It is the discipline rule 2.8 applies to every ceiling
/// refusal — name the number, so the consumer reports something a human can act
/// on rather than the word `mismatch`.
///
/// `Ord` exists because this rides in the export's per-connection grouping key
/// (see [`Epoch::superseded`]), and for no other reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Superseded {
    /// The version the consumer stated it was built against.
    pub wanted: u32,
    /// The version this producer calls current — [`ShapeContract::version`].
    pub current: u32,
}

impl std::fmt::Display for Superseded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "linked on v{} while this producer serves v{} as current",
            self.wanted, self.current
        )
    }
}

/// **WHAT A SUBSCRIBER ASKED TO SEE OF EACH KEY** — the `detail` parameter, and
/// the one place the pool-membership question is answered.
///
/// A separate `pool-membership` SHAPE was ruled out: it *would just admit that
/// our shapes concept is deficient*. Membership is not a different thing — it
/// is the same shape over the same keys out of the same fold, at a COARSER
/// LEVEL OF DETAIL. So it is a parameter, and under rule 2.8 a parameter has a
/// ceiling: [`Self::Rows`] is both the ceiling and the default, [`Self::Keys`]
/// is less.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    /// Which keys EXIST, and nothing about any of them: one or more membership
    /// frames ([`MembersPage`]), then `added`/`gone` for ever.
    Keys,
    /// The whole row for each key, which is what every subscribe has always
    /// meant. **The ceiling**, so a subscriber that asks for nothing gets this.
    #[default]
    Rows,
}

impl Detail {
    pub fn as_str(&self) -> &'static str {
        match self {
            Detail::Keys => "keys",
            Detail::Rows => "rows",
        }
    }

    /// Parse the wire spelling. Absent → the ceiling, which is rule 2.8's
    /// *asking nothing means the ceiling*. Anything else is `None` and the door
    /// refuses it.
    pub fn parse(s: Option<&str>) -> Option<Self> {
        match s {
            None | Some("rows") => Some(Detail::Rows),
            Some("keys") => Some(Detail::Keys),
            Some(_) => None,
        }
    }
}

/// **HOW MUCH OF A WINDOWED CELL A SUBSCRIBER ASKED TO BE SHOWN** — the
/// `retain` parameter, which is rule 2.8 applied to the *history* axis.
///
/// The rule came in two halves. First: *these sequence shapes, if they are
/// lazy, can have a parameter for how many items the subscriber wants the
/// producer to hold for them. Like it could be ALL, COUNT.* Then, on the hazard
/// of an unbounded `ALL`: *we can set a max size on the shape on the producer
/// side. so instead of ALL it would be MAX.*
///
/// # `Max` is "everything currently retained", never "everything that happened"
///
/// That is the whole reason the name is `Max` and not `All`. A windowed cell
/// holds `capacity` items inside `window`; `Max` is that, declared by the
/// producer as a `const` and published on `shapes.list` as
/// [`ShapeMax::retained_items`]. It is never a read of a durable log. A hydrate
/// that walked 2.46 M ledger events to answer `ALL` would be the same class of
/// unbounded read as the 100,000-row thread list this chassis's first producer
/// already had to repair.
///
/// # Who pays for a large ask
///
/// The obvious reading of *how many items the subscriber wants the producer to
/// hold for them* is PER-SUBSCRIBER RETENTION: two subscribers ask for 5 and
/// 500, the producer holds 500, and the small subscriber's cost is set by the
/// large one's appetite. **This chassis cannot do that and does not pretend
/// to.** A shape has ONE root shared by every subscriber, so *hold for them* is
/// not a per-subscriber quantity at all: the cell's capacity and window are
/// fixed by the producer's `const` and no subscribe moves them.
///
/// So the ceiling **bounds total exposure**, and `retain` is a PROJECTION AT
/// DELIVERY rather than a retention request:
///
/// * the producer's cost is the ceiling, always, paid whether one consumer
///   subscribes or fifty;
/// * `Count(5)` asks for LESS than it would otherwise receive — strictly fewer
///   bytes on its own frames and nothing at all on anybody else's;
/// * **no subscriber's appetite is chargeable to another**, because the only
///   dial that could charge it, the retained size, is not on the subscribe.
///
/// That is the unbounded-`ALL` repair taken to its conclusion: once the bound
/// is on the producer side there is nothing left for a subscriber to inflate.
///
/// # Scope
///
/// Per CONNECTION per SHAPE, not per key. A second subscribe on the same shape
/// from the same connection replaces it. Said rather than left to be
/// discovered: a per-`(conn, key)` retain would have no representation for a
/// keyless subscribe, which is one ask covering every key.
///
/// It bounds every `row` frame that connection receives for the shape — the
/// export that satisfies the subscribe AND the live re-exports after it. A knob
/// that held only on the first frame would be a stub that pretends to work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retain {
    /// **Everything the producer is currently retaining** — the ceiling, and
    /// the default, so a subscriber that asks for nothing gets what every
    /// subscriber got before the parameter existed.
    #[default]
    Max,
    /// **The newest `n` items of what is currently retained.** `n` may be `0`,
    /// which is the real ask *the row, without its log*. It may not exceed
    /// [`ShapeMax::retained_items`], and over it is [`ShapeError::AboveMax`]
    /// naming the ceiling — never clamped.
    Count(usize),
}

impl Retain {
    /// How many items to show, given what the cell is holding. `Max` is what is
    /// there; `Count(n)` is the newest `n` of it. The `min` is a floor and not
    /// a clamp: the ask was already checked against the declared ceiling at
    /// subscribe time, and a window holding fewer items than the ask is the
    /// world being small, not the request being refused.
    pub fn take(&self, held: usize) -> usize {
        match self {
            Retain::Max => held,
            Retain::Count(n) => (*n).min(held),
        }
    }

    /// Parse the wire spelling. Absent, `null` or `"max"` → the ceiling; a
    /// non-negative integer → [`Self::Count`]. Anything else is `None`, and the
    /// door refuses it rather than quietly handing back the ceiling.
    pub fn parse(v: Option<&Value>) -> Option<Self> {
        match v {
            None | Some(Value::Null) => Some(Retain::Max),
            Some(Value::String(s)) if s == "max" => Some(Retain::Max),
            Some(Value::Number(n)) => n.as_u64().map(|n| Retain::Count(n as usize)),
            Some(_) => None,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Retain::Max => "max".to_string(),
            Retain::Count(n) => n.to_string(),
        }
    }
}

/// **THE CHASSIS'S PAGE, WHEN NEITHER THE PRODUCER NOR THE CONSUMER NAMED ONE.**
///
/// The operator's chain, verbatim in substance: *we should make large list
/// shapes page by built-in mechanism so it's just a field you set. And I think
/// the default should be to page at some reasonable size, maybe 100. We can
/// have a global default of 100, a producer-set default informed by the size of
/// the thing being paginated, and then a consumer can set their own page size
/// since they know their context. null being no pagination.*
///
/// This constant is the first link. It is what a keyless `rows` subscribe
/// answers with when the shape declares [`Paging::Default`] and the consumer
/// sends nothing, and it is published on `shapes.list` as
/// [`PageContract::default`] already resolved — so a consumer never has to know
/// this number exists.
pub const DEFAULT_PAGE_ROWS: usize = 100;

/// **HOW MANY ROWS ONE KEYLESS SUBSCRIBE ANSWERS WITH** — the `page` parameter,
/// the producer's declaration and the chassis default, all one type because
/// they are one chain: *global default → producer default → consumer ask, null
/// being no pagination.*
///
/// # What a page IS here, and why it needs no cursor
///
/// Rule 2.5: *there is no re-emit verb, no replay, no cursor.* A page on this
/// chassis is a KEY RANGE and not a position. The producer answers *the first
/// `n` keys from `from`, in key order*, and the last frame of a short answer
/// carries [`ShapeWire::next`] — the first key it did NOT export. The consumer
/// asks for the next page by re-subscribing with `from: next`. That token is a
/// key the consumer already holds rather than a position the producer
/// remembers: the producer keeps NOTHING per subscriber (a range query over the
/// root it already has), the same ask yields the same keys, and a consumer that
/// lost a page or doubts one simply asks again — which is rule 2.5's own
/// remedy.
///
/// **The 120 s promise retention is untouched**: that is per KEY, and a page is
/// a set of ordinary keyed exports each naming the subscribe that asked.
///
/// # Rule 2.8, and the one place this chain bends it
///
/// `page` has a ceiling, [`ShapeMax::rows_per_subscribe`], and an ask above it
/// is [`ShapeError::AboveMax`] naming the ceiling — never clamped. What differs
/// from `retain` and `beat` is the second clause: *asking nothing* does not
/// mean the ceiling here, it means the DEFAULT the producer published, because
/// a default below the ceiling was ruled for exactly this parameter. The
/// ceiling is still reachable by asking for it, and *no pagination* —
/// [`Paging::Whole`], `page: null` on the wire — is the ceiling by name.
///
/// # Rule 2.6 — additive, and how
///
/// A consumer built before `page` existed sends nothing and gets the resolved
/// default, which for a pool smaller than 100 keys is what it got before. Where
/// the pool is larger, the PRODUCER declares [`Paging::Whole`] — the chain's
/// second link, *informed by the size of the thing being paginated* — so
/// nothing a face already reads moves. A shape whose keyless answer shrinks
/// under this default has had a semantic change and owes a version bump; the
/// chassis cannot see which shapes those are, so this doc says it instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Paging {
    /// The next link decides: on a consumer's ask, the producer's declaration;
    /// on the producer's declaration, [`DEFAULT_PAGE_ROWS`].
    #[default]
    Default,
    /// **No pagination** — [`ShapeMax::rows_per_subscribe`], which is what
    /// every keyless subscribe got before this parameter existed.
    Whole,
    /// This many rows per keyless subscribe. Never `0`, which [`Self::parse`]
    /// refuses: a page of nothing is not an ask, it is `detail: keys`.
    Rows(usize),
}

impl Paging {
    /// Parse the wire spelling of `page`. Absent → the chain decides; `null` →
    /// no pagination; a positive integer → that many rows. `0` and anything
    /// else are `None`, and the door refuses them rather than quietly handing
    /// back a default.
    pub fn parse(v: Option<&Value>) -> Option<Self> {
        match v {
            None => Some(Paging::Default),
            Some(Value::Null) => Some(Paging::Whole),
            Some(Value::Number(n)) => match n.as_u64() {
                Some(0) | None => None,
                Some(n) => Some(Paging::Rows(n as usize)),
            },
            Some(_) => None,
        }
    }

    /// **THE CHAIN, RESOLVED TO ONE NUMBER.** `ask` is the consumer's,
    /// `declared` the producer's, `ceiling` its `rows_per_subscribe`. The ask
    /// was already checked against the ceiling at the door, so this never
    /// clamps one; the single `min` is the CHASSIS default against a producer
    /// whose ceiling is below 100, which is nobody's ask being cut and simply
    /// the default being the ceiling.
    pub fn resolve(ask: Paging, declared: Paging, ceiling: usize) -> usize {
        match ask {
            Paging::Rows(n) => n,
            Paging::Whole => ceiling,
            Paging::Default => match declared {
                Paging::Rows(n) => n,
                Paging::Whole => ceiling,
                Paging::Default => DEFAULT_PAGE_ROWS.min(ceiling),
            },
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Paging::Default => "default".to_string(),
            Paging::Whole => "null".to_string(),
            Paging::Rows(n) => n.to_string(),
        }
    }
}

/// **WHAT A KEYLESS SUBSCRIBE ANSWERS WITH WHEN THE CONSUMER SAYS NOTHING, AND
/// THE MOST IT MAY ASK FOR** — the producer's two links of the paging chain,
/// published on `shapes.list` as [`ShapeInfo::page`].
///
/// `default: None` is *no pagination*: the producer declared [`Paging::Whole`]
/// and a silent consumer gets up to `max` rows, as it always did. `max` is
/// [`ShapeMax::rows_per_subscribe`] restated beside it, so the two numbers a
/// consumer sizes a page by sit in one block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageContract {
    pub default: Option<usize>,
    pub max: usize,
}

/// **HOW OFTEN A SUBSCRIBER ASKED TO BE BEATEN AT** — the `beat` parameter, and
/// rule 2.8 on the *rate* axis. It is the FIRST ceiling on this chassis that
/// bounds WORK PER UNIT TIME; every other one bounds a QUANTITY, and six things
/// change when the axis is time.
///
/// The rule itself is unchanged: *the producer should offer a max contract to
/// the consumer and the consumer can choose to utilize up to that max. So again
/// the answer is to set a MAX frequency.*
///
/// # 1. The ask is a PERIOD and the ceiling is a FREQUENCY, so *beyond* inverts
///
/// A bigger `retain` is more; a bigger `beat` interval is LESS. So a `beat` ask
/// beyond the contract is a SMALLER number than the ceiling, and
/// [`ShapeError::AboveMax`]'s remedy — *ask for `{max}` or less* — would be
/// advice to ask for more of the same failure. That is why the refusal is its
/// own variant, [`ShapeError::TooFast`], on exactly the reasoning that split
/// [`ShapeError::NoRetention`] out of `AboveMax`.
///
/// # 2. A quantity ceiling bounds the answer; a rate ceiling bounds no total
///
/// `retain: 32` says the whole exposure is 32 items, for ever. `beat: 1000`
/// says nothing about the total: N frames per second times an unbounded number
/// of seconds has no bound. The only thing that bounds a beating shape's output
/// is the SUBSCRIPTION ENDING, which makes a beat the one knob whose cost is
/// paid for as long as somebody listens rather than once per ask.
///
/// # 3. The producer's cost is INCURRED, not merely EXPOSED
///
/// A retain window exists whether anybody subscribes or not, so `retain` only
/// selects from something already paid for. A beat does not exist until
/// something emits it, so the ceiling is a rate of WORK actually performed. The
/// consequence: for `retain`, rule 2.8's *asking nothing means the ceiling*
/// costs the producer nothing extra; for `beat`, the default is the most
/// expensive delivery the contract allows, on every connection that never named
/// the knob.
///
/// # 4. So the projection is DECIMATION, and the gate needs STATE
///
/// The retain precedent decides who pays: the producer emits at its own `const`
/// rate whether one consumer subscribes or fifty, and a subscriber asking for
/// less gets fewer FRAMES on its own connection and changes nothing on anybody
/// else's. But where [`Retain`] is a pure function of the cell and the ask, a
/// rate projection must remember WHEN it last delivered to each connection, so
/// `beat` adds a clock reading per connection per frame. The FOLD still reads
/// no clock: the gate is in the delivery path, after the fold, where every
/// other audience decision already lives.
///
/// # 5. Dropping is only legal because a beat SUPERSEDES
///
/// Decimating a stream of real state changes would lose facts — which is why
/// a coalesce window has to COALESCE and not drop. A beat is a pure level
/// (*the link was alive as of seq N*) and the newest beat contains everything
/// every dropped beat said, so dropping is lossless here and only here. **This
/// is not a coalesce window and does not become one**: the gate is reachable
/// only on a shape that declares a minimum beat interval, where the only live
/// `row` is a beat.
///
/// # 6. A gap in the sequence is NOT loss, under decimation
///
/// The beat carries the producer's own monotone sequence number and the gate
/// does not renumber it. Renumbering would make `seq` mean *frames you were
/// sent*, and then one field would answer both *how many beats has the producer
/// made* and *how many did I see*. So a consumer that asked `beat: 5000`
/// against a 1,000 ms producer sees `seq` advance by about five each time and
/// **must not read the gap as a dropped frame** — it asked for the gap.
///
/// # Scope
///
/// Per CONNECTION per SHAPE, exactly as [`Retain`] is, and a second subscribe
/// on the same shape from the same connection replaces it. A beat is a fact
/// about the LINK and not about a key, so a per-`(conn, key)` beat would answer
/// a question nobody asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Beat {
    /// **Every beat the producer makes** — the ceiling, the fastest this shape
    /// goes, and the default.
    #[default]
    Max,
    /// **At most one beat frame per this many milliseconds.** It may not be
    /// SMALLER than [`ShapeMax::beat_interval_ms`] — smaller is faster, and
    /// faster is the direction a rate ceiling forbids — and under it is
    /// [`ShapeError::TooFast`] naming the producer's period, never clamped.
    Every(u64),
}

impl Beat {
    /// The interval this ask amounts to, given the producer's own period.
    /// [`Self::Max`] is the producer's period, which is what *the ceiling* means
    /// on this axis. `max` and not `min` because a rate ceiling is a FLOOR on
    /// the interval: no ask can make the producer beat faster.
    pub fn interval_ms(&self, producer_ms: u64) -> u64 {
        match self {
            Beat::Max => producer_ms,
            Beat::Every(ms) => (*ms).max(producer_ms),
        }
    }

    /// Parse the wire spelling. Absent, `null` or `"max"` → the ceiling; a
    /// non-negative integer of milliseconds → [`Self::Every`]. Anything else is
    /// `None` and the door refuses it.
    pub fn parse(v: Option<&Value>) -> Option<Self> {
        match v {
            None | Some(Value::Null) => Some(Beat::Max),
            Some(Value::String(s)) if s == "max" => Some(Beat::Max),
            Some(Value::Number(n)) => n.as_u64().map(Beat::Every),
            Some(_) => None,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Beat::Max => "max".to_string(),
            Beat::Every(ms) => format!("{ms}ms"),
        }
    }
}

/// **ONE PAGE OF A POOL'S MEMBERSHIP**, on a `detail: keys` export.
///
/// **Deliberately NOT the row-less export.** An export emits `row: None,
/// cells: None` for a key the root does not hold, and the same bytes meaning *I
/// do not hold this key* and *member, rows not requested* is the collapsed-event
/// class this chassis keeps paying for — four bugs of it were counted in this
/// path alone. A membership frame carries this field and a row frame never
/// does.
///
/// Three-valued like every other read here, with *empty* collapsed where it is
/// genuinely the same statement: an empty pool is `Known { keys: [], complete:
/// true }`, a COMPLETE answer of zero. `NotHydrated` is the other statement,
/// and it is the one a producer that cannot enumerate its namespace makes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MembersPage {
    /// These keys exist, in key order. `complete: false` means this is one page
    /// of more, and `next` names the first key of the page after it — so a
    /// mirror that lost a frame knows where the hole starts.
    Known {
        keys: Vec<String>,
        complete: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<String>,
    },
    /// **The producer cannot enumerate this pool**, and here is why: an opaque
    /// remote namespace, a read that failed. The three-valued state already
    /// says it, so there is no separate boolean flag for *members supported*.
    NotHydrated { reason: String },
}

/// **THE FRAME.** One export or one occurrence, for one shape, on one key.
/// Everything a mirror holds is applied out of this struct.
///
/// Field order is the wire: the golden strings in this file's tests are the
/// serialisation of this declaration, so moving a field moves bytes a face
/// already parses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapeWire {
    /// **The promises this frame resolves** (rule 2.4). Absent when empty,
    /// which is almost always. A coalesced `row` may resolve several at once,
    /// which is why it is a list and not a scalar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub promises: Vec<u64>,
    /// **THE SUBSCRIBES THIS EXPORT SATISFIES** — rule 2.7's `exported` stage,
    /// and the repair for a collapse rule 2.4 admitted in its own last
    /// paragraph: *`shapes.subscribe` answers `{queued: true}`, an
    /// acknowledgement in spirit and anonymous in practice — the export that
    /// resolves it cannot be matched to it.*
    ///
    /// A subscribe now answers `{queued: true, subscribe_id}` and the export —
    /// or exports, since a keyless subscribe yields one per key and all of them
    /// carry the same id, because all of them are what that one call asked for
    /// — name it here. So a consumer can say *the read I asked for has arrived*
    /// instead of inferring it from a counter moving, which is the
    /// timeout-and-retry policy rule 2.5 exists to keep out of clients.
    ///
    /// **A SEPARATE FIELD FROM [`Self::promises`], and not the same id space.**
    /// A command promise and a subscribe are two different acts; merged into one
    /// list, a consumer holding id 5 could not tell which of its two
    /// outstanding things it had been told about, and the producer would have to
    /// mint both from one counter it does not own — the promise book is the
    /// producer's and the subscribe id is the chassis's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subscribes: Vec<u64>,
    pub shape: String,
    /// `None` = a fact about the SHAPE and not about one key: the death export,
    /// and a `shared` move. The door delivers those to any connection holding
    /// any key of that shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub op: Op,
    /// On a `live` frame: the stamp of the input that caused it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    /// On a `read` frame: the REAL fold point, never the `at` a subscribe
    /// request asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub as_of: Option<Timestamp>,
    pub epoch: Epoch,
    /// Per-cell provenance. Rides ONLY on an export; a live frame carries one
    /// `at` and the consumer advances its own `folded_at` from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cells: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row: Option<Value>,
    /// Shape-level scalars. Present on every export, so no consumer has to
    /// hardcode a threshold the producer owns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_cells: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<Derived>,
    /// **ONE PAGE OF THE POOL'S MEMBERSHIP** — present on a `detail: keys`
    /// export and on NO other frame. See [`MembersPage`] for why it is its own
    /// field rather than a row-less export wearing a second meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<MembersPage>,
    /// **THE WHOLE ANSWER TO THIS SUBSCRIBE HAS ARRIVED** — `true` on the LAST
    /// frame a `shapes.subscribe` causes, `false` when the producer's ceiling
    /// cut the answer short, absent on every frame that is not the last of a
    /// subscribe.
    ///
    /// Without it, a keyless subscribe yielded one export per key, all carrying
    /// the subscribe id, and *a consumer knows it has the whole answer when the
    /// exports stop* — which is unobservable, because the next frame is a live
    /// one or nothing, and nothing has no arrival time. That is the
    /// deadline-and-retry policy rule 2.5 keeps out of clients, and a face built
    /// one because this field did not exist.
    ///
    /// **`false` is rule 2.8's fourth clause**: where the ceiling is smaller
    /// than the whole answer, the frame says so STRUCTURALLY. A silent
    /// truncation is the lie the rule forbids; a stated short answer is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub complete: Option<bool>,
    /// **THE FIRST KEY THIS PAGE DID NOT EXPORT** — on the last frame of a
    /// keyless `rows` answer that is short (`complete: false`), and on no other
    /// frame. The consumer's next ask is `{from: next}`; see [`Paging`] for why
    /// this is a key the consumer holds rather than a cursor the producer does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// A frame plus its routing. `to` never reaches the wire.
///
/// **DELIBERATELY NOT `Serialize`/`Deserialize` — the one type in this module
/// that is neither.** It is a producer-side envelope: what crosses the socket
/// is its [`ShapeWire`], and `shape`/`key` are copies of fields already inside
/// it so the broadcast can route without parsing. A `Deserialize` impl would be
/// a trap, because no such bytes are ever written; a `Serialize` impl would
/// offer `to`, a connection id, to a transport. It lives in `wire` because it
/// is inert data a mirror's own test harness constructs, not because a mirror
/// parses it.
#[derive(Debug, Clone)]
pub struct ShapeFrame {
    /// `Some(conn)` = this connection's own export, and the ONLY thing that
    /// puts `(shape, key)` into its subscription set.
    pub to: Option<ConnId>,
    pub shape: String,
    pub key: Option<String>,
    pub wire: ShapeWire,
}

/// **ONE SHAPE, AS `shapes.list` PUBLISHES IT.** The introspection frame: what
/// the shape is called, what it holds, who is watching it, and every ceiling
/// and contract a consumer is allowed to read before it links.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShapeInfo {
    pub name: String,
    /// **THE PARENT SHAPE'S NAME** for a nested shape; absent for a root one,
    /// so `shapes.list` is byte-identical on a producer that nests nothing.
    ///
    /// Redundant with [`Self::name`] by construction — a nested shape is named
    /// `"<parent>/<leaf>"` and registration refuses any other spelling — and
    /// carried anyway so a reader walks a declared edge instead of splitting a
    /// string and hoping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// **WHICH SHAPE EACH `Ref` IN THIS SHAPE'S MANIFEST RESOLVES TO** — the
    /// key-level nesting edge, declared.
    ///
    /// [`Self::parent`] is the SHAPE-level edge: `threads/log` nests inside
    /// `threads`. This is the KEY-level one: a cell declared `Ref("thread")` or
    /// `List(Ref("thread"))` holds ids, and this map says those ids are keys of
    /// the shape `threads` — so a consumer holding a row can walk from a
    /// reference to the thing it names by subscribing, rather than knowing by
    /// convention that `thread` means `threads`. The count of descendants is a
    /// cell the shape declares; the traversal is a `Ref` cell plus this map plus
    /// a subscribe.
    ///
    /// **ABSENT when the shape declares none**, so `shapes.list` is
    /// byte-identical for every shape written before the declaration existed. A
    /// ref name NOT in this map is an id in a space no shape serves — a
    /// resource id, say — and a consumer reading the map learns that too:
    /// absence here means *not a shape's key*, not *unknown*.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub refs: BTreeMap<String, String>,
    pub keys: Vec<String>,
    /// `"eager"` or `"lazy"`. A `String` rather than the `&'static str` it is
    /// declared with, for [`Derived::Transition`]'s reason: `&'static str` has
    /// no `Deserialize` impl, and the JSON is the same string either way.
    pub tracking: String,
    pub tracked: bool,
    /// **HOW MANY DISTINCT CONNECTIONS ARE SUBSCRIBED**, counted by the task
    /// from its own subscription set with the key dropped.
    ///
    /// **Not a count of `(conn, key)` pairs.** The code has deduped on the
    /// connection id since before this chassis was a crate, while the doc on
    /// [`Self::consumers`] described a pair count and built an argument on it.
    /// The code is the one that is right: a keyless subscribe on a keyed shape
    /// expands to one pair per key, so a pair count would report one board as
    /// four subscribers on a four-account shape. *Is anyone watching, and how
    /// many of them* is the question this number is read for, and only the
    /// connection count answers it.
    ///
    /// It is also the number production serves and the baseline the contract
    /// doc measured: `accounts: subscribers 1 (the operator's board), keys 4`.
    /// Changing the count to match the stale doc would have moved a live
    /// `shapes.list` field under a board that is already reading it.
    pub subscribers: usize,
    pub epoch: Epoch,
    pub fifo_high_water: usize,
    pub bursts: u64,
    /// **Shape-named counters, FLATTENED.** A counter called `unplaced_status`
    /// lands at the top level of `shapes.list` exactly where a face already
    /// reads it, so the flatten is load-bearing wire and not a tidiness. The
    /// chassis names none of them.
    #[serde(flatten)]
    pub counters: BTreeMap<String, u64>,
    /// **WHO IS SUBSCRIBED RIGHT NOW**, by name, declared shape version, build
    /// and pid — the passive-consumer view, and the thing that would have made
    /// two board processes running superseded code visible the day that
    /// happened.
    ///
    /// **This is not a second spelling of [`Self::subscribers`], and the two do
    /// agree on their count.** They did not always: a KEYED
    /// `shapes.unsubscribe {shape, key}` removed one `(conn, key)` pair from the
    /// task's set but closed the whole `(conn, shape)` ledger entry, so a
    /// connection holding `a` and `b` that dropped `b` was reported
    /// `subscribers: 1, consumers: 0` while `a`'s frames kept arriving. The fix
    /// is a ledger call that NARROWS an entry's key set and closes it only when
    /// the set empties; the agreement is held by that call rather than by this
    /// paragraph.
    ///
    /// **What makes both fields worth carrying** is not the count. `subscribers`
    /// is a NUMBER the task can publish with no registry at all; this is a list
    /// of IDENTITIES the hub holds beside the task. One answers *is anyone
    /// watching*, the other *who, and what did they think this shape looked
    /// like*. The length of this list is the same integer, arrived at through
    /// the ledger — and a producer that wires no ledger still publishes the
    /// count.
    pub consumers: Vec<ConsumerEntry>,
    /// **EVERY CEILING THIS SHAPE DECLARES** — rule 2.8's last clause, which is
    /// what makes a contract readable before a consumer uses it.
    pub max: ShapeMax,
    /// **WHICH VERSIONS OF THIS SHAPE THIS PRODUCER SERVES** — rule 2.6's
    /// producer declaration, published for the same reason every ceiling is.
    ///
    /// A SIBLING of [`Self::max`] and not a field inside it; see
    /// [`ShapeContract`] for why a version is not a ceiling.
    pub contract: ShapeContract,
    /// **THE PRODUCER'S TWO LINKS OF THE PAGING CHAIN** — what a silent keyless
    /// subscribe answers with, and the most a `page` ask may be.
    ///
    /// `None` is *a producer built before paging existed*, and a consumer
    /// reading `None` knows a keyless subscribe there answers up to
    /// `max.rows_per_subscribe` with no `next` to follow. Absent from the wire
    /// when `None`; a producer running this code always publishes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<PageContract>,
    /// **EVERY PER-KEY CELL THIS SHAPE CARRIES**, published so a consumer knows
    /// which cells can EVER arrive before it reads one. See [`crate::cell`] for
    /// the hole this closes: a cell the producer never declared reads, on every
    /// consumer, as *not yet* for ever.
    ///
    /// **REQUIRED, with no serde default**, on [`Self::contract`]'s terms. A
    /// defaulted `[]` would be a consumer concluding this producer carries
    /// NOTHING — a positive claim serde invented about the one field whose job
    /// is to be believed before a read is attempted. A parse failure is loud; an
    /// invented manifest is the silence this field exists to end.
    pub cells: Vec<CellInfo>,
    /// **EVERY SHAPE-LEVEL CELL** — the manifest for the frame's `shared_cells`
    /// block, on [`Self::cells`]' terms, and named as the frame names the block
    /// it describes.
    pub shared_cells: Vec<CellInfo>,
}

/// **ONE DECLARED CELL, AS `shapes.list` PUBLISHES IT** — the wire form of
/// [`crate::cell::Cell`].
///
/// `dtype` is the `Display` spelling of the declared [`crate::operation::DType`]
/// (`struct:StateLevel`, `map<opaque>`, `list<struct:MoveItem>`). The parts a
/// consumer CHECKS against are broken out beside it so that nobody has to parse
/// that string: `words` is the closed vocabulary when the dtype's leaf is an
/// enum, `fields` the one-level field list when it is a struct.
///
/// `words` and `fields` skip when empty, and that is legitimate here where it is
/// not on [`ShapeInfo::cells`]: an absent list and an empty list are the SAME
/// statement from the two ends (*no words*, *no fields*), and the manifest's own
/// compile-time check makes a struct with no fields unrepresentable — so absence
/// cannot stand in for something declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellInfo {
    pub name: String,
    pub dtype: String,
    /// `true`: never null once the cell is known. `false`: may be null or
    /// absent, and that is a statement rather than a defect.
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldInfo>,
}

/// One field of a struct-shaped cell, one level deep. See [`CellInfo`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldInfo {
    pub name: String,
    pub dtype: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<String>,
}

impl CellInfo {
    /// The declared field by name, for a struct-shaped cell.
    pub fn field(&self, name: &str) -> Option<&FieldInfo> {
        self.fields.iter().find(|f| f.name == name)
    }
}

/// **THE PRODUCER'S CEILINGS, PUBLISHED** — rule 2.8.
///
/// > Every parameter a subscriber can set has a ceiling the producer declares
/// > on the shape. A subscriber may ask for anything up to the ceiling; asking
/// > for nothing means the ceiling; asking beyond it is refused on the request
/// > id, NAMING the ceiling — never clamped. Where the ceiling is smaller than
/// > the whole answer, the frame says so structurally. Every ceiling rides
/// > `shapes.list`, so a consumer can read the contract it is allowed to use
/// > before it uses it.
///
/// **Consts on the shape, not runtime config.** A ceiling is a statement about
/// what the producer can afford to serve, and one that could change per request
/// would be a producer its consumers cannot reason about. The shape trait
/// carries the defaults; an author overrides only the ones whose default is
/// wrong for that shape.
///
/// **The rule is evidence, not novelty.** The future-`at` bound and
/// [`ShapeError::BadAt`] were already exactly this — a bound, and a refusal on
/// the request id naming it — written before the rule was. The rest of this
/// struct is the same pattern collected under one name instead of scattered as
/// unrelated constants.
///
/// Raising a ceiling is additive; LOWERING one is a semantic change under rule
/// 2.6 and bumps the shape's version. The asymmetry is right: a consumer built
/// against a 500-key page keeps working when the page becomes 1,000, and not
/// the other way round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShapeMax {
    /// The most ROWS one `shapes.subscribe` will export. A keyless subscribe at
    /// `detail: rows` stops here and the last export says `complete: false`; on
    /// a shape that hydrates per key, a keyless `detail: rows` is REFUSED naming
    /// this number, because a consumer must not be able to ask for a 20 MB dump
    /// by omission.
    pub rows_per_subscribe: usize,
    /// The most KEYS one membership frame carries. Not a consumer dial — the
    /// chassis pages, and a subscribe's last membership frame says
    /// `complete: true`. It is declared so a consumer can size its own buffer.
    pub keys_per_frame: usize,
    /// How far ahead a future `at` may be, in seconds: the ceiling
    /// [`ShapeError::BadAt`] already refuses against.
    pub future_at_secs: i64,
    /// How many future-`at` timers one connection may hold on this shape,
    /// refused by [`ShapeError::TooManyTimers`].
    pub armed_timers_per_conn: usize,
    /// **THE MOST ITEMS OF A WINDOWED CELL ONE SUBSCRIBER WILL BE SHOWN** —
    /// [`Retain::Max`] as a number, and what [`Retain::Count`] is checked
    /// against.
    ///
    /// **`0` means this shape holds no windowed cell**, and a `retain: n` ask
    /// against it is [`ShapeError::NoRetention`] rather than
    /// [`ShapeError::AboveMax`]: *this parameter selects nothing here* and *you
    /// asked for more than I serve* are two different statements.
    ///
    /// It is a number about EXPOSURE and not about storage. A subscriber cannot
    /// raise it, and nothing a subscriber asks changes what the producer keeps.
    pub retained_items: usize,
    /// **THE SHORTEST INTERVAL BETWEEN TWO BEAT FRAMES, IN MILLISECONDS** — and
    /// therefore the MAXIMUM FREQUENCY, which is the operator's own word for
    /// this ceiling. A `beat` ask below it is [`ShapeError::TooFast`].
    ///
    /// **The first ceiling here that bounds WORK PER UNIT TIME**; [`Beat`]
    /// states the six consequences. The one that matters for reading this
    /// number: every other field on this struct bounds a total, and this one
    /// bounds no total at all.
    ///
    /// **`0` means this shape does not beat**, and a `beat: n` ask against it is
    /// [`ShapeError::NoBeat`] rather than [`ShapeError::TooFast`], exactly as
    /// [`Self::retained_items`]'s `0` works.
    pub beat_interval_ms: u64,
    /// **THE MOST KEYS OF THIS SHAPE ONE CONNECTION MAY HOLD OPEN AT ONCE**,
    /// refused by [`ShapeError::TooManyKeys`].
    ///
    /// Rule 2.8 applied to the one subscriber parameter on a per-key-hydrating
    /// shape that had no ceiling at all: the KEY. Every other knob here bounds
    /// how much ONE subscribe answers with; this bounds HOW MANY subscribes a
    /// connection may have outstanding, which on a shape that hydrates a
    /// bounded window per key is the number that decides the root's size. A
    /// navigator moving through threads opens one key per thread and closes
    /// none, so without this the bound is the consumer's patience.
    ///
    /// **`None` MEANS THIS SHAPE DECLARES NO SUCH CEILING**, which is every
    /// shape written before the field existed — so it is `Option` rather than a
    /// sentinel `0`, and ABSENT from the wire rather than serialised as `null`.
    /// That is [`Epoch::superseded`]'s precedent, for the same reason: a field
    /// that exists so one shape can publish a ceiling must not move a byte of
    /// `shapes.list` for the shapes that have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_keys_per_conn: Option<usize>,
}

/// **WHICH VERSIONS OF THIS SHAPE THIS PRODUCER SERVES** — rule 2.6's producer
/// half, published where a consumer can read it before it links.
///
/// > *Version mismatch is refused at link time, loudly.* A shape declares
/// > `VERSION: u32` and `SERVES: &[u32]` — the older versions it still serves.
/// > A consumer states the version it was built against. Three states, not two:
/// > the current version SERVES, a member of `SERVES` serves WITH A WARNING on
/// > every export's epoch, and anything else is REFUSED naming the version
/// > wanted and the versions available.
///
/// **A sibling of [`ShapeMax`] and not a field inside it**, deliberately. `max`
/// is a set of CEILINGS: every entry answers *how much of this may I ask for*,
/// and asking beyond any of them fails the same way. A version answers *which
/// contract is this* — not a quantity, with no *asking nothing means the
/// ceiling* reading, and whose refusal is terminal rather than a fixable
/// request. Rule 2.7's own precedent is the model: the durable contract rides
/// `shapes.list` as a SIBLING of `shapes` rather than folded into a neighbour,
/// because a consumer reads the contract before it asks and two different
/// questions get two different blocks.
///
/// **Published as a REQUIRED field with no serde default**, on exactly
/// [`ShapeMax`]'s terms: a defaulted version would be a contract nobody
/// declared, and inventing one is worse than failing to parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShapeContract {
    /// **THE VERSION THIS PRODUCER CALLS CURRENT.**
    ///
    /// Never `0`: registration panics on it, so *unset* has no spelling and a
    /// consumer reading this number is reading a declaration somebody made
    /// rather than the absence of one.
    pub version: u32,
    /// **EVERY VERSION A LINK IS SERVED ON, CURRENT FIRST, STRICTLY
    /// DESCENDING** — the declared current version followed by the older ones
    /// the shape still serves.
    ///
    /// The whole list and not just the old ones, because this is the list the
    /// refusal quotes — *this producer serves [4, 3]* — and a consumer that had
    /// to concatenate two fields to learn what it may ask for would be doing the
    /// producer's arithmetic. A shape serving only its current version has
    /// exactly one entry here, which is the default.
    pub serves: Vec<u32>,
}

impl ShapeContract {
    /// Where a stated version stands against this contract — rule 2.6's three
    /// states, decided in one place so the producing and consuming halves
    /// cannot disagree about the answer.
    ///
    /// **A consumer that states NOTHING is [`Link::Unstated`] and is served.**
    /// Rule 2.6 is written about *a consumer that states the version it was
    /// built against*, and the `version` parameter does not exist on any
    /// consumer built before it. Refusing those would refuse the board — the
    /// same ruling the ledger's identity parse already made: a chassis that
    /// refuses its only consumer to enforce its own bookkeeping has inverted
    /// what it is for. The gap is not swallowed; it is a state with a name, so
    /// it is legible rather than silent.
    pub fn link(&self, wanted: Option<u32>) -> Link {
        let Some(wanted) = wanted else {
            return Link::Unstated;
        };
        if wanted == self.version {
            return Link::Current;
        }
        if self.serves.contains(&wanted) {
            return Link::Superseded(Superseded {
                wanted,
                current: self.version,
            });
        }
        Link::Refused {
            wanted,
            serves: self.serves.clone(),
        }
    }
}

/// **WHERE A STATED VERSION STANDS AGAINST A PRODUCER'S CONTRACT** — rule 2.6's
/// three states, and rule 2.7's *link → current/superseded/refused* as a type.
///
/// Three variants and not a `bool`, because collapsing the stages of one act
/// forces every consumer to accept whichever stage the producer chose: *these
/// are three different events and deserve 3 different events, and that's the
/// general form of this problem.*
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// The consumer stated no version. Served, and recorded as having stated
    /// none — see [`ShapeContract::link`] for why this is not a refusal.
    Unstated,
    /// The consumer asked for [`ShapeContract::version`]. Served, and no byte of
    /// any frame says anything about it.
    Current,
    /// The consumer asked for a member of the served set. Served, and every
    /// export's [`Epoch::superseded`] says so for the life of the link.
    Superseded(Superseded),
    /// The consumer asked for something this producer does not serve. Refused at
    /// link time, as [`ShapeError::VersionRefused`].
    Refused { wanted: u32, serves: Vec<u32> },
}

/// **NOT on the wire today, and the derives do not put it there.** A refused
/// subscribe reaches a consumer as its transport's own error carrying
/// [`Display`](std::fmt::Display) text — `threads`' `shapes.subscribe` answers
/// a JSON-RPC error. `Serialize`/`Deserialize` are here so a consumer can hold
/// and pass one around in its OWN transport; nothing in this crate emits this
/// representation, so nothing can have changed by its existing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShapeError {
    Unknown(String),
    /// Ruling 1: the task died. `shapes.subscribe` is REFUSED with this rather
    /// than answering `{queued: true}` and then nothing, forever.
    Dead {
        shape: String,
        reason: String,
    },
    /// Ruling 4: a future `at` needs a bound. An absurd or overflowing `at` is
    /// refused, not queued.
    ///
    /// **Rule 2.8's founding evidence.** The rule's own text says
    /// *"`MAX_FUTURE_AT`/`BadAt` predate the rule and were already obeying it,
    /// which is evidence it was discovered rather than invented."* The message
    /// derives the bound from [`crate::hub::MAX_FUTURE_AT`] rather than spelling
    /// it, so the refusal and the ceiling on `shapes.list` cannot drift.
    BadAt(String),
    /// **TOO MANY FUTURE-`at` SUBSCRIPTIONS ARMED ON ONE CONNECTION.**
    ///
    /// Rule 2.8's refusal clause — *"Asking beyond it is REFUSED, NAMING THE
    /// CEILING — never clamped"* — and this variant **did not name it** until
    /// 2026-10-02. It carried the shape alone, so a consumer was told it had
    /// asked for too many and not how many it may have: the refusal that tells
    /// you you were wrong and not what to ask instead, which is precisely what
    /// [`Self::KeyRequired`] and [`Self::AboveMax`] were both changed to stop
    /// doing.
    ///
    /// `max` is [`ShapeMax::armed_timers_per_conn`] as published on
    /// `shapes.list`, read off the same slot — not the const — so the number in
    /// the refusal is the number in the contract by construction.
    TooManyTimers {
        shape: String,
        /// **THE CEILING THE REFUSAL IS AGAINST** — rule 2.8.
        max: usize,
    },
    /// **TOO MANY KEYS OF ONE SHAPE HELD OPEN ON ONE CONNECTION.**
    ///
    /// Rule 2.8's refusal clause for the KEY parameter, and modelled on
    /// [`Self::TooManyTimers`] rather than on [`Self::AboveMax`] deliberately:
    /// `AboveMax`'s remedy is *"ask for {max} or less"*, which is not the repair
    /// here, because the ask was for ONE key and there is no smaller ask. The
    /// repair is to RELEASE a key — `shapes.unsubscribe` — which is exactly the
    /// sentence `TooManyTimers` already prints for the other per-connection
    /// capacity on this chassis.
    ///
    /// `max` is [`ShapeMax::open_keys_per_conn`] as published on `shapes.list`,
    /// read off the same slot and not off the const, so the number in the refusal
    /// is the number in the contract by construction.
    TooManyKeys {
        shape: String,
        /// **THE CEILING THE REFUSAL IS AGAINST** — rule 2.8.
        max: usize,
    },
    /// **A KEYLESS SUBSCRIBE TO A SHAPE THAT KNOWS KEYS ONLY ON DEMAND.**
    ///
    /// [`crate::hub::KeyScope::OnDemand`]: "every key I know about" is "nothing"
    /// there, so this is refused on the request id rather than acked and then
    /// answered with silence. A consumer's repair is to name the key it wants.
    KeyRequired {
        shape: String,
        /// **THE CEILING THE REFUSAL IS AGAINST** — rule 2.8: a refusal that
        /// does not name the ceiling tells a consumer it asked wrongly and not
        /// what to ask instead. `ShapeMax::rows_per_subscribe`.
        max_rows: usize,
    },
    /// **`detail: keys` IS A QUESTION ABOUT THE POOL, AND A POOL HAS NO KEY.**
    ///
    /// Refused where the request is still a request, like every other
    /// decidable-from-the-declaration refusal in [`crate::hub::Hub::subscribe`].
    /// Answering it — a one-key roster — would be a second spelling of the keyed
    /// subscribe that already exists.
    BadDetail(String),
    /// **A PARAMETER BEYOND ITS PRODUCER-DECLARED CEILING** — rule 2.8, third
    /// clause. **Never clamped**: a clamped answer is a second claim about a
    /// number the frame states outright, and the consumer must then decide which
    /// to believe. This names the ceiling, so the consumer knows exactly what to
    /// ask instead.
    AboveMax {
        shape: String,
        param: String,
        asked: String,
        max: String,
    },
    /// **`retain` ASKED OF A SHAPE THAT HOLDS NO WINDOWED CELL.**
    ///
    /// Its own variant rather than [`Self::AboveMax`] with a ceiling of zero.
    /// *"You asked for more history than I serve"* and *"there is no history
    /// here to select"* are two different facts, and `AboveMax`'s remedy — *"ask
    /// for `{max}` or less"* — reads as "ask for 0 or less", which is advice to
    /// do a thing that is not the repair. The repair is to drop the parameter.
    ///
    /// Decidable from the shape's declaration alone, so it is refused in
    /// [`crate::hub::Hub::subscribe_ask`] where the answer can still reach the
    /// caller, like every other declaration-time refusal there.
    NoRetention {
        shape: String,
    },
    /// **`beat` ASKED OF A SHAPE THAT DOES NOT BEAT.**
    ///
    /// The rate axis's [`Self::NoRetention`], and it is a separate variant for the
    /// same reason that one is: *"there is no beat here"* and *"you asked faster
    /// than I beat"* have different repairs, and the second one's remedy read
    /// against a ceiling of zero would be "ask for 0ms or more", which is advice
    /// to keep asking.
    NoBeat {
        shape: String,
    },
    /// **A `beat` ASKED FASTER THAN THE PRODUCER'S DECLARED PERIOD** — rule 2.8's
    /// third clause on the RATE axis, and NOT [`Self::AboveMax`].
    ///
    /// `AboveMax` cannot express this. Its remedy is *"ask for `{max}` or less"*,
    /// and on this axis the ask beyond the ceiling is the SMALLER number: `beat:
    /// 100` against a 1,000 ms producer is too fast, so "100 is above 1000" is
    /// false as arithmetic and "ask for 1000 or less" is advice to ask for the
    /// same thing again. The direction of *beyond* inverts when the parameter is a
    /// period, which is the sharpest single difference between a rate ceiling and
    /// a quantity ceiling — see [`Beat`].
    ///
    /// **Never clamped**, like every other ceiling here: a clamped rate would be a
    /// second claim about a cadence the contract states outright.
    TooFast {
        shape: String,
        asked_ms: u64,
        min_ms: u64,
    },
    /// **THE CONSUMER WAS BUILT AGAINST A VERSION THIS PRODUCER DOES NOT SERVE**
    /// — rule 2.6, refused AT LINK TIME and never discovered per frame.
    ///
    /// **It names what the producer SERVES, not merely that there is a
    /// mismatch**, and that is rule 2.8's discipline applied on this axis. The
    /// refusal that says *"you asked wrongly"* and not *"and here is what to ask
    /// instead"* is the one [`Self::KeyRequired`] and [`Self::TooManyTimers`] were
    /// both changed to stop being. A consumer holding this can report *"I am a v2
    /// build and this daemon serves 4 and 3"*, which names a deployment fact a
    /// human can act on; `mismatch` names nothing.
    ///
    /// `serves` is [`ShapeContract::serves`] as published on `shapes.list`, read
    /// off the same slot rather than off the const, so the refusal and the
    /// contract cannot drift — the property [`Self::TooManyTimers`] was given on
    /// 2026-10-02 for the same reason.
    ///
    /// **It is NOT [`Self::AboveMax`] and not a clamp of any kind.** There is no
    /// nearest served version to fall back to: the versions are a SET and not an
    /// interval, so "the closest one" is not a thing the producer could pick even
    /// if it were allowed to. And it is not `malformed` on the wire either — see
    /// `crate::envelope::codes::VERSION_REFUSED`, which exists because a consumer
    /// has to tell *"I wrote a bad request"* from *"I am the wrong build"* without
    /// parsing English.
    VersionRefused {
        shape: String,
        /// The version the consumer stated it was built against.
        wanted: u32,
        /// Every version this producer serves, current first — the actionable
        /// half.
        serves: Vec<u32>,
    },
}

impl std::fmt::Display for ShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShapeError::Unknown(s) => write!(f, "unknown shape: {s}"),
            ShapeError::Dead { shape, reason } => write!(
                f,
                "shape `{shape}` is dead ({reason}); it cannot be subscribed to \
                 until the producer restarts"
            ),
            ShapeError::BadAt(s) => write!(f, "invalid `at`: {s}"),
            ShapeError::TooManyTimers { shape, max } => write!(
                f,
                "shape `{shape}` arms at most {max} pending future-`at` \
                 subscriptions per connection and this one already holds {max}: \
                 wait for one to fire, or `shapes.unsubscribe` to release a slot. \
                 It is not clamped and nothing was queued"
            ),
            ShapeError::TooManyKeys { shape, max } => write!(
                f,
                "shape `{shape}` holds at most {max} keys open per connection and \
                 this one already holds {max}: `shapes.unsubscribe` a key you are \
                 no longer looking at to release a slot. It is not clamped and \
                 nothing was queued"
            ),
            ShapeError::KeyRequired { shape, max_rows } => write!(
                f,
                "shape `{shape}` hydrates keys one at a time, so a keyless \
                 `detail: rows` would be an unbounded export and its ceiling is \
                 {max_rows} rows: name the key you want, or ask `detail: keys` \
                 for the pool's membership"
            ),
            ShapeError::BadDetail(s) => write!(f, "invalid `detail`: {s}"),
            ShapeError::AboveMax {
                shape,
                param,
                asked,
                max,
            } => write!(
                f,
                "`{param}` on shape `{shape}`: asked {asked}, and the producer's \
                 maximum is {max}. Ask for {max} or less — it is not clamped"
            ),
            ShapeError::NoRetention { shape } => write!(
                f,
                "shape `{shape}` holds no windowed cell, so `retain` selects \
                 nothing here: omit it. It is not an ask that is too large — \
                 there is no history on this shape to take a count of"
            ),
            ShapeError::NoBeat { shape } => write!(
                f,
                "shape `{shape}` does not beat, so `beat` selects nothing here: \
                 omit it. It is not an ask that is too fast — there is no beat on \
                 this shape to take a cadence of"
            ),
            ShapeError::TooFast {
                shape,
                asked_ms,
                min_ms,
            } => write!(
                f,
                "`beat` on shape `{shape}`: asked one beat every {asked_ms}ms, and \
                 the producer beats at most once every {min_ms}ms. Ask for \
                 {min_ms}ms or MORE — a rate ceiling is a FLOOR on the interval, \
                 and it is not clamped"
            ),
            // **RULE 2.6's SENTENCE, VERBATIM.** The doc writes the refusal as
            // `shape accounts: built against v2; this producer serves [4, 3]`, so
            // that is what this prints — the version wanted AND the versions
            // available, because a refusal naming only the mismatch is the one
            // every other refusal in this enum was repaired to stop being.
            ShapeError::VersionRefused {
                shape,
                wanted,
                serves,
            } => write!(
                f,
                "shape {shape}: built against v{wanted}; this producer serves {}. \
                 The link is REFUSED and nothing was queued — it is not served on \
                 a nearer version, because the versions are a set and not a range",
                crate::wire::fmt_versions(serves)
            ),
        }
    }
}

/// **THE `version` SUBSCRIBE PARAMETER, PARSED** — rule 2.6's half of rule 2.1's
/// `{shape, key?, at?, consumer, version}`.
///
/// `Some(inner)` is a readable ask and `inner` is what the consumer stated:
/// `Some(n)` a version, `None` *nothing stated*. **`None` is the REFUSAL** — the
/// param was present and is not a version — which is the shape
/// [`Detail::parse`], [`Retain::parse`] and [`Beat::parse`] already use at this
/// door, so one reading serves all four.
///
/// **IT LIVES IN THE ALWAYS-COMPILED FLOOR AND IS SHARED BY EVERY DOOR**, for two
/// reasons that are really one. A CONSUMER writes this param and must spell it the
/// way the producer reads it, exactly as it does for `detail` and `retain` — *"a
/// consumer that can parse the ceiling off `shapes.list` and cannot spell the ask
/// against it has half a contract."* And `threads` has its own legacy subscribe
/// door beside this chassis's: two hand-written copies of this match is two places
/// for the rule to be reachable from one door and not the other, which is the
/// *declared-and-never-constructed* failure `ShapeError::TooManyTimers` already
/// shipped once in this crate.
///
/// **ABSENT IS LEGAL AND IS NOT "THE CEILING".** A version is not a quantity and
/// there is no largest one; absent means the consumer stated nothing, which
/// [`ShapeContract::link`] serves and names [`Link::Unstated`]. Refusing those
/// would refuse every consumer built before rule 2.1's param existed.
///
/// **`null` IS ABSENT, not a malformed ask**, because a client that fills a fixed
/// request shape writes `null` for *I have nothing to say here* — the same reading
/// `crate::ledger::ConsumerIdentity::parse` gives a `null` `consumer`.
pub fn parse_version(v: Option<&serde_json::Value>) -> Option<Option<u32>> {
    match v {
        None => Some(None),
        Some(serde_json::Value::Null) => Some(None),
        Some(v) => v
            .as_u64()
            .filter(|n| *n <= u64::from(u32::MAX))
            .map(|n| Some(n as u32)),
    }
}

/// The one sentence a door says when [`parse_version`] refuses. One copy, so two
/// doors cannot tell a consumer two different things about one param.
pub const VERSION_PARAM_REFUSAL: &str =
    "`version` must be a non-negative integer naming the shape contract you were \
     built against; omit it if you state none. It is NOT `consumer.shape_version`, \
     which is free text for a human and is never compared — read `contract.serves` \
     off `shapes.list` for the versions this producer serves";

/// `[4, 3]` — rule 2.6's own spelling for the served set.
///
/// A free function and not a `Display` impl on a newtype, because the ONE place
/// the list is rendered for a human is this refusal and
/// [`crate::mirror::VersionMismatch`]'s twin of it, and both must render it
/// identically or a consumer's own report and the producer's refusal would
/// disagree about the same list.
pub(crate) fn fmt_versions(v: &[u32]) -> String {
    let inner: Vec<String> = v.iter().map(|n| n.to_string()).collect();
    format!("[{}]", inner.join(", "))
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. One subscribed consumer
// ─────────────────────────────────────────────────────────────────────────────
//
// The DATA half of the subscriber ledger. It rides inside [`ShapeInfo`] on
// `shapes.list`, so a consumer parsing that frame parses this — which is why it
// is here and not in [`crate::ledger`], where its machinery (the append-only
// file, the registry, the sweep) stays. `crate::ledger::ConsumerEntry` still
// resolves, by re-export.
//
// This is what keeps `wire` free of a `ledger` dependency. `ShapeInfo` carries
// a `Vec<ConsumerEntry>` unconditionally; had the type stayed in `ledger`, the
// always-compiled floor would have had to pull the ledger in, and a consumer
// build would link a std-fs append path it never calls.

/// **ONE CURRENTLY-SUBSCRIBED CONSUMER**, as `shapes.list` reports it.
///
/// Flattened rather than nesting [`crate::ledger::ConsumerIdentity`] so that an UNIDENTIFIED
/// subscriber is one row with null names rather than a row with a null object —
/// a reader scanning the column sees the hole.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsumerEntry {
    /// The daemon-assigned connection id. Not a consumer's claim about itself:
    /// it is the one field here the chassis KNOWS, and it is what ties a row to
    /// the ledger's connect line.
    pub conn: ConnId,
    /// **EVERY KEY THIS CONNECTION NAMED ON THIS SHAPE**, sorted.
    ///
    /// **It was `key: Option<String>` — one scalar — and that was a collapse with
    /// two halves.** A connection may subscribe twice on one shape naming two
    /// keys, which is one subscription holding two keys; the scalar could hold one,
    /// so the second subscribe OVERWROTE the first and `shapes.list` reported a
    /// consumer watching `b` when it was watching `a` and `b`. Worse, the ledger
    /// recorded that widening as `resubscribe` — rule 2.5's RECOVERY act — so
    /// *"this consumer lost its state and repaired it"* and *"this consumer took an
    /// interest in another key"* were one word. See [`crate::ledger::EVENT_WIDEN`].
    ///
    /// A keyless subscribe is NOT a key called `null` in this list. It is
    /// [`Self::keyless`], because *"every key you know about"* is a different ask
    /// from any particular key and a sentinel in a key list is a key a producer
    /// could mint — the same argument [`Resolving`] makes for keeping its keyless
    /// slot unspellable.
    pub keys: Vec<String>,
    /// This connection subscribed without naming a key — *"every key you know
    /// about"*. It may ALSO hold named keys ([`Self::keys`]); the two are
    /// independent asks, not two spellings of one.
    pub keyless: bool,
    /// When this subscription opened, RFC3339. The operator's *"three days
    /// old"* is read off this field.
    pub since: String,
    /// `null` for a consumer that did not identify itself.
    pub name: Option<String>,
    pub shape_version: Option<String>,
    pub pid: Option<u32>,
    pub build: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. The ack — rule 2.3's vocabulary, rule 2.7's form
// ─────────────────────────────────────────────────────────────────────────────

/// **WHY A CONSUMER COULD NOT APPLY A FRAME IT RECEIVED.**
///
/// A closed set, because the point of a rejection is that a producer can COUNT it
/// and a human can act on the count. Prose alone is what the mirror already
/// throws away.
pub mod reject {
    /// The frame's `event.kind` is a word this consumer's code does not know. A
    /// consumer running older code than the producer — **the one detector rule
    /// 2.6's soft spot 2 admits it does not have** for a semantic change it cannot
    /// refuse at link time.
    pub const UNKNOWN_KIND: &str = "unknown_kind";
    /// A cell's `state` is a word this consumer does not know. Same class as
    /// [`UNKNOWN_KIND`], one level down.
    pub const UNKNOWN_CELL_STATE: &str = "unknown_cell_state";
    /// A derived event arrived for a key this connection never had an export for.
    /// The consumer rule is *hold the export, apply everything after it*, and this
    /// is the frame that has no export to be after.
    pub const NO_EXPORT: &str = "no_export";
    /// The frame parsed and named a shape this consumer is not mirroring.
    pub const NOT_MINE: &str = "not_mine";
    /// **The shape's `epoch.state` is a word this consumer does not know.** The
    /// same class as [`UNKNOWN_CELL_STATE`], one level UP — [`super::ShapeState`]
    /// is `#[serde(rename_all)]` over a closed enum, so an unheard-of word is a
    /// hard parse failure that would take the whole frame with it. A consumer
    /// keeps the state it had, ignores the word, and counts it here; guessing
    /// that an unknown word means "fine" is how a board shows a confident number
    /// it has no right to.
    pub const UNKNOWN_SHAPE_STATE: &str = "unknown_shape_state";
    /// **The frame's `members.state` is a word this consumer does not know.**
    ///
    /// [`super::MembersPage`] is `serde(tag = "state")` over a CLOSED enum, so an
    /// unheard-of third word is a hard parse failure that takes the whole frame
    /// — and a membership frame is an `op: read`, so what goes with it is the
    /// frame's `subscribes` and `promises`. A `detail: keys` subscribe answered
    /// by a page this consumer cannot read used to produce a subscribe that NEVER
    /// SETTLES: no `complete`, no refusal, nothing to wait on. The roster this
    /// consumer already holds STANDS; the word is counted here.
    pub const UNKNOWN_MEMBERS_STATE: &str = "unknown_members_state";
    /// **The frame's `op` is a word this consumer does not know.**
    ///
    /// [`super::Op`] is two words today and a third is exactly the semantic
    /// change rule 2.6 admits it cannot refuse at link time. NO BODY is applied —
    /// *replace one key* and *append an occurrence* are the two things an `op`
    /// chooses between and there is no safe third guess — but the frame's own
    /// bookkeeping lands: the epoch is absorbed, rule 2.7's stages are recorded
    /// and the promises resolve.
    ///
    /// **Why those land rather than being dropped with the body.** Rule 2.5 has
    /// no re-emit verb: a promise id and a subscribe id a frame loses are not
    /// re-offered except by a re-subscribe the CONSUMER decides to issue, and
    /// deciding that means noticing a promise did not resolve and concluding it
    /// has been too long — the timeout-and-retry policy rule 2.5 exists to keep
    /// out of clients. A consumer wedged by one word it does not recognise is
    /// strictly worse than a consumer that knows (because this code is counted)
    /// that it missed a body.
    pub const UNKNOWN_OP: &str = "unknown_op";
    /// **The frame did not parse at all**, and no single unknown word explains
    /// it. The last resort, so that "received and not applied" is always
    /// sayable: a consumer that silently drops a frame it could not read is a
    /// consumer whose disagreement with the producer is invisible to both.
    pub const UNREADABLE: &str = "unreadable";

    pub const ALL: &[&str] = &[
        UNKNOWN_KIND,
        UNKNOWN_CELL_STATE,
        NO_EXPORT,
        NOT_MINE,
        UNKNOWN_SHAPE_STATE,
        UNKNOWN_MEMBERS_STATE,
        UNKNOWN_OP,
        UNREADABLE,
    ];
}

/// One frame received and NOT applied, with a reason from [`reject`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejected {
    /// The frame's serial, as [`Ack::received_through`] counts them.
    pub serial: u64,
    /// One of [`reject::ALL`]. A code, so it can be tallied.
    pub reason: String,
    /// The word the consumer did not know, when there was one — `event.kind`, a
    /// cell state. Absent rather than empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// **AN ACK SAYS WHAT RECEIPT DID** — three things, where rule 2.3 named one.
///
/// 2.3 fixes an ack at RECEIVED INTO THE MIRROR and that is deliberately weak:
/// *"it can only ever hurt itself"* is the property, and any stronger definition
/// exports the slowest screen's speed to every other consumer. The collapse is
/// INSIDE "received": three things happen at receipt and the reference mirror
/// (`shapes.py::on_frame`) already distinguishes them — a frame applied, a frame
/// of another shape, an unknown `op`, a `live` with no event, a transition for a
/// key never exported, an unknown event `kind`, an unknown cell-state word — and
/// then returns a `bool`, spending the distinction.
///
/// * **received** — [`Self::received_through`], the highest CONTIGUOUS serial. The
///   delivery guarantee, and the only thing a producer's retention bound waits on.
/// * **applied** — the default, and it costs nothing to say because it is what the
///   absence of a rejection means.
/// * **rejected** — [`Self::rejected`]. A fact about the SEAM: this consumer's code
///   and this producer's frames disagree. The producer records it beside the
///   consumer's declared version in the ledger, which is the one place a mismatch
///   rule 2.6 cannot refuse can still be seen.
/// * **processed** stays outside an ack forever, as 2.3 rules.
///
/// ## NOTHING PRODUCES OR CONSUMES ONE OF THESE YET, AND THAT IS STATED RATHER
/// THAN IMPLIED
///
/// This is the VOCABULARY, defined where every other frame type is defined, so the
/// two halves of 2.2's ack path (a per-connection serial on transition frames; a
/// producer-side retention bound) are built against a settled shape instead of
/// inventing one each. **No frame carries a serial today** — `docs/shapes.md` 2.2's
/// *"soft"* paragraph is the whole design of that path and it has no code — so
/// [`Self::received_through`] has nothing to count and no producer reads an ack.
/// A consumer chassis is where `rejected` gets FILLED IN, and there is no consumer
/// chassis in Rust yet (`docs/shapes.md` 6.5). Declaring the type is not claiming
/// the path works.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub shape: String,
    /// The highest serial such that this consumer received every serial up to and
    /// including it. **Contiguous, not highest-seen**: a producer retaining
    /// un-acked transitions can only release what nothing behind it is missing.
    pub received_through: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<Rejected>,
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. A RESOURCE SUMMARY — the only place a resource's existence is stated
// ─────────────────────────────────────────────────────────────────────────────

/// **WHAT A SHAPE SAYS ABOUT A RESOURCE IT OWNS: FACTS, AND NOTHING ELSE.**
///
/// `docs/chassis-resources.md` 1.2's one sentence: *a shape says what exists, a
/// resource fetch says what it is.* This is the "what exists" half, and it rides
/// in a ROW FIELD — one summary per cell, never a list. A thread with 5,000
/// messages carries ONE of these and names ONE id.
///
/// ## Why this is in the always-compiled floor
///
/// Because the CONSUMING half reads it. [`crate::mirror::Mirror::plan`] decides
/// a transport out of exactly these two numbers, and that decision is a pure
/// function of mirrored state — so the type has to be where the mirror is, not
/// behind `fold` with the producer's cells.
///
/// ## FACTS, never a recommendation — and the operator's own reason
///
/// > *"the consumer side of the chassis decides, right? Because how can you say
/// > on the producer side push a payload over REST if the consumer didn't ask
/// > for it? … The producer could explicitly emit the resource metadata with a
/// > flag to remove ambiguity in the consumer, about which is supported for the
/// > resource. So yes the chassis decides, not the consumer explicitly. The
/// > consumer can be thought of as the agent and the chassis as the
/// > environment, in an RL analogy."*
///
/// So [`Self::transports`] is spelled SUPPORTED and is never spelled
/// *recommended*, *preferred* or *inline*. A producer that ranked the two would
/// be making the policy call the agent/environment test assigns to the
/// consuming side, and a consuming chassis reading a rank would be obeying a
/// policy it is supposed to own. The producer answers *"a by-id read of this
/// resource exists over each of these doors"*; which door gets used is
/// [`crate::mirror`]'s, out of [`Self::bytes`].
///
/// **The producer therefore does NOT prune this list by size.** A large payload
/// still lists `ws` if a by-id websocket read exists for it; pruning by size
/// would be the producer's threshold wearing a fact's clothes, and would make
/// the consuming chassis's choice a rubber stamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceSummary {
    /// **THE OPAQUE, SHAPE-ISSUED ID.** The only thing either door accepts, and
    /// the only way a consumer can name this resource. Never parsed, split or
    /// constructed by a consumer — see `crate::resource::ResourceId`, whose
    /// field and constructor are both private for the same reason.
    pub id: String,
    /// **THE WHOLE RESOURCE'S LENGTH.** The one place it is stated — which is
    /// what `crate::resource`'s 416-rather-than-clamp argument rests on, and
    /// what makes the transport decision cost no round trip: the size is
    /// already mirrored by the time anybody wants the bytes.
    pub bytes: u64,
    /// **WHICH DOORS A BY-ID READ OF THIS RESOURCE EXISTS ON.** `"ws"` = inline
    /// over the open websocket; `"get"` = `GET /resource/<id>`.
    ///
    /// Held as raw strings, deliberately. Rule 2.6's obligation on every
    /// consumer is to ignore what it does not understand, and a `#[serde(tag)]`
    /// enum here would make a transport word this build has never heard of a
    /// hard parse failure that took the whole frame with it. [`Self::supports`]
    /// is the only reader.
    #[serde(default)]
    pub transports: Vec<String>,
}

/// The two doors that exist today. A [`ResourceSummary`] names them as strings;
/// this is what the chassis's own code matches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Inline over the websocket that is already open: `resource/{id}/read`.
    Ws,
    /// `GET /resource/<id>` over HTTP.
    Get,
}

impl Transport {
    /// The word a summary spells it with.
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::Ws => "ws",
            Transport::Get => "get",
        }
    }
}

impl std::fmt::Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl ResourceSummary {
    /// A summary for `id` of `bytes` bytes, served on `transports`.
    pub fn new(id: impl Into<String>, bytes: u64, transports: &[Transport]) -> Self {
        Self {
            id: id.into(),
            bytes,
            transports: transports.iter().map(|t| t.as_str().to_string()).collect(),
        }
    }

    /// Whether the producer said this door exists for this resource. An
    /// unrecognised word in the list answers `false` for every door and is NOT
    /// an error — rule 2.6's ignore-what-you-do-not-know, applied here.
    pub fn supports(&self, t: Transport) -> bool {
        self.transports.iter().any(|w| w == t.as_str())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. The round trip — the capability this module exists for
// ─────────────────────────────────────────────────────────────────────────────

/// **A CONSUMER CAN PARSE A FRAME, AND THE BYTES A PRODUCER WRITES DID NOT
/// MOVE.** Two claims, and they are tested together on purpose, because the
/// cheap way to get the first one is to perturb the second.
///
/// Before the re-seam there was no `Deserialize` anywhere in this crate: every
/// wire type was `Serialize` only, so the `--no-default-features` build handed a
/// mirror the producer's WRITE cells and no way to read a single frame. These
/// tests are that missing capability, exercised.
///
/// **Where the golden strings come from.** Every `GOLD_*` constant below was
/// printed by the crate AS IT WAS AT `99e998b` — the commit before this re-seam,
/// `Serialize`-only, `ShapeInfo::tracking: &'static str` and
/// `Derived::Transition::cell: &'static str` — through a throwaway copy of that
/// tree built out of this workspace. They are not a photograph of the new code
/// congratulating itself. If any of them goes red, a producer's wire changed and
/// every consumer of it, the board included, is parsing something else.
///
/// **`GOLD_INFO` HAS NOW BEEN DELIBERATELY MOVED TWICE, AND THIS IS THE SECOND
/// (2026-09-28).** `keyed` is GONE and `max` is new. `keyed` was
/// `Shape::KEYED`, a const read by no code at all — `do_subscribe` iterates
/// `Root::keys()`, so a `KEYED = false` shape would have been acked and handed
/// nothing — and the field on this frame was its only effect. A consumer that
/// reads `keyed` today reads `true` and always did, so its removal takes away a
/// constant and no information. `max` is rule 2.8's contract block: every
/// ceiling a subscriber can be refused against, published where it can be read
/// before it is hit. See [`ShapeMax`].
///
/// **AND A STANDING CAVEAT ON THIS GOLDEN, WHICH IS NOT THIS DISPATCH'S TO
/// FIX.** These bytes are STRUCT-ORDERED, because they come from
/// `serde_json::to_string(&an_info())`. The LIVE `shapes.list` is KEY-SORTED:
/// both doors answer `json!({"shapes": producer.hub().list()})`, and `json!`
/// round-trips through `serde_json::Value`, whose `Map` is a `BTreeMap` without
/// the `preserve_order` feature. So this golden pins the SERIALISATION of
/// `ShapeInfo` and does not describe the bytes a board receives. It still
/// catches every field added, removed or renamed, which is what it is read for;
/// it would not catch a field REORDERED. Left as found, and recorded here
/// because this dispatch moved the value and therefore had to look at it.
///
/// **THE FIRST DELIBERATE MOVE, 2026-09-26.**
/// `ShapeInfo.consumers[].key` — one nullable string — became `keys` (a sorted
/// list) plus `keyless` (a bool). The old field could not hold what a consumer can
/// actually be: a connection may subscribe twice on one shape naming two keys, so
/// the scalar overwrote and `shapes.list` reported a consumer watching the LAST
/// key it named rather than all of them. It is a NON-ADDITIVE change to a field
/// that landed on 2026-09-22, four days before this, and that nothing outside
/// these tests reads (the board draws `subscribers`, a count, and never this
/// list). Everything else in every golden is unchanged, `GOLD_LOADED` included —
/// which matters, because the promise-recovery repair populates `promises` on an
/// EXPORT and this golden already froze an export carrying `[7, 9]`, so that
/// repair moves no byte at all.
#[cfg(test)]
mod round_trip {
    use super::*;
    use serde_json::json;

    // ── Frozen at 99e998b ───────────────────────────────────────────────────
    const GOLD_LOADED: &str = r#"{"promises":[7,9],"shape":"accounts","key":"anthropic:sam","op":"read","as_of":"2026-09-25T12:00:00Z","epoch":{"init_at":"2026-09-25T11:59:59Z","reinit":2,"lost":41,"state":"known"},"cells":{"share":{"complete":true,"folded_at":"2026-09-25T12:00:00Z","state":"known"}},"row":{"account":"anthropic:sam","share":0.25},"shared":{"warn_below_days":3},"shared_cells":{"warn_below_days":{"state":"known"}}}"#;
    const GOLD_LIVE: &str = r#"{"shape":"accounts","op":"live","at":"2026-09-25T12:00:00.123Z","epoch":{"reinit":0,"lost":0,"state":"not_hydrated"},"event":{"kind":"transition","cell":"moves","item":{"from":"idle","to":"busy"}}}"#;
    const GOLD_DEAD: &str = r#"{"shape":"accounts","op":"read","as_of":"2026-09-25T12:00:00Z","epoch":{"init_at":"2026-09-25T11:00:00Z","reinit":0,"lost":0,"state":"dead","dead_reason":"the shape task panicked"}}"#;
    const GOLD_CELL_KNOWN: &str =
        r#"{"state":"known","complete":true,"folded_at":"2026-09-25T12:00:00Z"}"#;
    const GOLD_CELL_WINDOWED: &str = r#"{"state":"known","complete":false,"folded_at":"2026-09-25T12:00:00Z","since_init":true,"covers_since":"2026-09-25T11:00:00Z"}"#;
    const GOLD_CELL_NOT_HYDRATED: &str = r#"{"state":"not_hydrated"}"#;
    const GOLD_CELL_DEAD: &str = r#"{"state":"dead","reason":"gone"}"#;
    /// **`GOLD_INFO` MOVED ON 2026-09-28, AND THAT IS THE RECORD OF WHY.**
    ///
    /// `max` gained `retained_items` — rule 2.8's ceiling for the `retain`
    /// parameter, read off `Shape::MAX_RETAINED_ITEMS` at `register`. It is
    /// ADDITIVE, which is the only kind of movement this field may make: raising
    /// or adding a ceiling is compatible, LOWERING one is a semantic change under
    /// rule 2.6, and a consumer parsing `shapes.list` gains a key it can ignore.
    ///
    /// **These two tests are what made the change visible at all**, which is
    /// exactly what a frozen-bytes test is for: nothing else in the suite would
    /// have noticed a new key on a method the board polls. The byte string is
    /// updated rather than the assertion relaxed — a golden that tolerates
    /// additions stops being a golden.
    ///
    /// ── `GOLD_INFO` MOVED A THIRD TIME, 2026-10-05: RULE 2.6's `contract` ────
    ///
    /// **THIS IS A WIRE-COMPATIBILITY EVENT ON `shapes.list`, SAID OUT LOUD.**
    /// `ShapeInfo` gained ONE key, `contract`, and it is REQUIRED with no serde
    /// default, so a consumer that parses `shapes.list` into a typed `ShapeInfo`
    /// built before today will fail to parse it. Before and after, in full:
    ///
    /// ```text
    /// before:  …,"max":{…,"beat_interval_ms":0}}
    /// after:   …,"max":{…,"beat_interval_ms":0},"contract":{"version":1,"serves":[1]}}
    /// ```
    ///
    /// **Nothing else moved, and that was the design constraint rather than the
    /// outcome.** `GOLD_LOADED`, `GOLD_LIVE`, `GOLD_DEAD` and all four
    /// `GOLD_CELL_*` are byte-identical: rule 2.6's other new field,
    /// `Epoch::superseded`, is `skip_serializing_if = "Option::is_none"` and is
    /// `None` on every link that asked for the current version or stated none —
    /// which is every link that exists — so no EXPORT, no LIVE frame and no DEATH
    /// frame changed by one byte. The only frame that moved is the one that
    /// publishes the contract, which is the frame whose job that is.
    ///
    /// **Why `contract` is REQUIRED and not defaulted, which is what makes this a
    /// break rather than a free addition.** It is `max`'s own argument, and `max`
    /// is the precedent this follows: *"a defaulted zero would be a ceiling nobody
    /// declared, which is worse than a parse failure."* A defaulted `version: 0`
    /// or `serves: []` would be a consumer concluding this producer serves NOTHING
    /// — a positive claim, invented by serde, about the one field whose whole
    /// purpose is to be believed before a link is attempted. A parse failure is
    /// loud; an invented contract is the silence rule 2.6 exists to end.
    ///
    /// **Who this actually breaks, measured rather than assumed.** No consumer in
    /// this workspace parses `shapes.list` into a typed `ShapeInfo`: the board
    /// reads `subscribers` and the flattened counters by key out of untyped JSON,
    /// and both still read. The strict parse lives in `check-features.sh`'s mirror
    /// probe and in these tests. An outside consumer holding a typed
    /// `shapewire` `ShapeInfo` is affected, and that is exactly what rule 2.6
    /// would have the producer BUMP `VERSION` for — which it cannot do for its own
    /// introspection frame, because `shapes.list` is not a shape and has no
    /// version of its own. That gap is reported, not papered over.
    ///
    /// ── `GOLD_INFO` MOVED A FOURTH TIME, 2026-10-08: THE CELL MANIFEST ──────
    ///
    /// **A SECOND WIRE-COMPATIBILITY EVENT ON `shapes.list`, ON THE SAME TERMS AS
    /// THE THIRD.** `ShapeInfo` gained TWO keys, `cells` and `shared_cells`,
    /// both REQUIRED with no serde default:
    ///
    /// ```text
    /// before:  …,"contract":{"version":1,"serves":[1]}}
    /// after:   …,"contract":{"version":1,"serves":[1]},"cells":[{"name":"share","dtype":"scalar:Float","required":true}],"shared_cells":[]}
    /// ```
    ///
    /// Required for `contract`'s reason, which is `max`'s reason: a defaulted
    /// empty manifest would be a consumer concluding this producer carries NO
    /// cells — a positive claim invented by serde about the field whose whole
    /// purpose is to say which cells can ever arrive. That claim is precisely the
    /// collapse the field exists to end (see `crate::cell`): a cell that is not
    /// declared must read as *there is no such cell*, and a consumer whose
    /// manifest serde emptied would read EVERY cell that way.
    ///
    /// **No export, live or death frame moved a byte** — the manifest rides the
    /// introspection frame and nothing else, exactly as `contract` does. The
    /// consumers this breaks are the same set the third move broke: anything
    /// parsing `shapes.list` into a typed `ShapeInfo`, which in this workspace is
    /// `check-features.sh`'s mirror probe, these tests, and the seam reference
    /// kit — all three updated in the same change.
    const GOLD_INFO: &str = r#"{"name":"accounts","keys":["anthropic:sam"],"tracking":"lazy","tracked":true,"subscribers":2,"epoch":{"reinit":0,"lost":0,"state":"not_hydrated"},"fifo_high_water":7,"bursts":0,"unplaced_status":3,"consumers":[{"conn":4,"keys":[],"keyless":true,"since":"2026-09-25T11:00:00.000Z","name":"threads-tui","shape_version":"rev4","pid":1234,"build":null}],"max":{"rows_per_subscribe":500,"keys_per_frame":500,"future_at_secs":86400,"armed_timers_per_conn":4,"retained_items":0,"beat_interval_ms":0},"contract":{"version":1,"serves":[1]},"cells":[{"name":"share","dtype":"scalar:Float","required":true}],"shared_cells":[]}"#;

    fn at(s: &str) -> Timestamp {
        s.parse().expect("stamp")
    }

    /// An export carrying everything at once: the promises a command's fact
    /// resolves, per-cell provenance, and a row.
    fn loaded() -> ShapeWire {
        ShapeWire {
            promises: vec![7, 9],
            subscribes: Vec::new(),
            shape: "accounts".to_string(),
            key: Some("anthropic:sam".to_string()),
            op: Op::Read,
            at: None,
            as_of: Some(at("2026-09-25T12:00:00Z")),
            epoch: Epoch {
                init_at: Some(at("2026-09-25T11:59:59Z")),
                reinit: 2,
                lost: 41,
                state: ShapeState::Known,
                dead_reason: None,
                // A ROOT shape, which every shape on this producer is — so the
                // field is absent from the golden bytes and `GOLD_LOADED` is
                // unchanged. The nested case is proved on a live hub, in
                // `threads/tests/nesting.rs`, where a real parent has a real era.
                parent_epoch: None,
                // Rule 2.6: a link on the CURRENT version, which is every link
                // this producer has ever had, so the field is absent from the
                // golden bytes and not one of them moved.
                superseded: None,
            },
            cells: Some(json!({
                "share": { "state": "known", "complete": true, "folded_at": "2026-09-25T12:00:00Z" },
            })),
            row: Some(json!({ "account": "anthropic:sam", "share": 0.25 })),
            shared: Some(json!({ "warn_below_days": 3 })),
            shared_cells: Some(json!({ "warn_below_days": { "state": "known" } })),
            event: None,
            members: None,
            next: None,
            complete: None,
        }
    }

    /// A live frame: one `at`, no cells, no row, an occurrence.
    fn live() -> ShapeWire {
        ShapeWire {
            promises: Vec::new(),
            subscribes: Vec::new(),
            shape: "accounts".to_string(),
            key: None,
            op: Op::Live,
            at: Some(at("2026-09-25T12:00:00.123Z")),
            as_of: None,
            epoch: Epoch::default(),
            cells: None,
            row: None,
            shared: None,
            shared_cells: None,
            event: Some(Derived::Transition {
                cell: "moves".to_string(),
                item: json!({"from": "idle", "to": "busy"}),
            }),
            members: None,
            next: None,
            complete: None,
        }
    }

    /// The supervisor's final export (ruling 1): `key: None`, nothing but an
    /// epoch that says `dead`.
    fn dead() -> ShapeWire {
        ShapeWire {
            promises: Vec::new(),
            subscribes: Vec::new(),
            shape: "accounts".to_string(),
            key: None,
            op: Op::Read,
            at: None,
            as_of: Some(at("2026-09-25T12:00:00Z")),
            epoch: Epoch {
                init_at: Some(at("2026-09-25T11:00:00Z")),
                reinit: 0,
                lost: 0,
                state: ShapeState::Dead,
                dead_reason: Some("the shape task panicked".to_string()),
                parent_epoch: None,
                // Rule 2.6: a link on the CURRENT version, which is every link
                // this producer has ever had, so the field is absent from the
                // golden bytes and not one of them moved.
                superseded: None,
            },
            cells: None,
            row: None,
            shared: None,
            shared_cells: None,
            event: None,
            members: None,
            next: None,
            complete: None,
        }
    }

    fn an_info() -> ShapeInfo {
        let mut counters = BTreeMap::new();
        counters.insert("unplaced_status".to_string(), 3);
        ShapeInfo {
            name: "accounts".to_string(),
            // A ROOT shape: absent from `GOLD_INFO`, which is why the board's wire
            // is unchanged by nesting existing.
            parent: None,
            // Declares no referent: absent from the wire on the same terms.
            refs: BTreeMap::new(),
            // Built before paging existed: absent from the wire on the same terms.
            page: None,
            keys: vec!["anthropic:sam".to_string()],
            tracking: "lazy".to_string(),
            tracked: true,
            subscribers: 2,
            epoch: Epoch::default(),
            fifo_high_water: 7,
            bursts: 0,
            counters,
            consumers: vec![ConsumerEntry {
                conn: 4,
                keys: Vec::new(),
                keyless: true,
                since: "2026-09-25T11:00:00.000Z".to_string(),
                name: Some("threads-tui".to_string()),
                shape_version: Some("rev4".to_string()),
                pid: Some(1234),
                build: None,
            }],
            max: ShapeMax {
                rows_per_subscribe: 500,
                keys_per_frame: 500,
                future_at_secs: 86400,
                armed_timers_per_conn: 4,
                retained_items: 0,
                beat_interval_ms: 0,
                // **ABSENT FROM THE WIRE, WHICH IS WHY `GOLD_INFO` DID NOT MOVE.**
                // A shape that declares no key ceiling publishes none — see
                // `ShapeMax::open_keys_per_conn`.
                open_keys_per_conn: None,
            },
            // Rule 2.6's contract as `Shape`'s DEFAULTS produce it — version 1,
            // serving only itself. This is what a shape whose author has not
            // thought about versions publishes, which is every shape that exists
            // the day the const lands.
            contract: ShapeContract {
                version: 1,
                serves: vec![1],
            },
            // The cell manifest, as `Shape::CELLS` publishes it: the one cell
            // `loaded()` carries, declared. A shape-level manifest that is empty
            // is a STATEMENT (`Cells::NONE`), serialised as `[]` and never
            // skipped, so a consumer reading it reads a declaration.
            cells: vec![CellInfo {
                name: "share".to_string(),
                dtype: "scalar:Float".to_string(),
                required: true,
                words: Vec::new(),
                fields: Vec::new(),
            }],
            shared_cells: Vec::new(),
        }
    }

    /// **THE ONE THAT DID NOT EXIST.** A frame with `promises`, with `cells` and
    /// with a `row` goes out as JSON, comes back as a `ShapeWire`, and is the
    /// same value. This is the whole job of a mirror's parser; before the
    /// re-seam it could not be written, because no type in this crate derived
    /// `Deserialize`.
    #[test]
    fn a_consumer_round_trips_a_frame_with_promises_cells_and_a_row() {
        let out = loaded();
        let json = serde_json::to_string(&out).expect("serialize");
        let back: ShapeWire =
            serde_json::from_str(&json).expect("a consumer must be able to parse this");

        assert_eq!(back, out, "round trip changed the frame");
        assert_eq!(back.promises, vec![7, 9], "the promises a command resolves");
        assert_eq!(
            back.row.as_ref().and_then(|r| r.get("share")),
            Some(&json!(0.25)),
            "the row survived"
        );
        assert!(back.cells.is_some(), "per-cell provenance survived");
    }

    /// The other two frame shapes a mirror actually receives. Both are mostly
    /// ABSENT fields, which is the case a naive `Deserialize` gets wrong: every
    /// `skip_serializing_if` field has to parse from nothing.
    #[test]
    fn a_live_frame_and_a_death_frame_round_trip_too() {
        for f in [live(), dead()] {
            let json = serde_json::to_string(&f).expect("serialize");
            let back: ShapeWire = serde_json::from_str(&json).expect("parse");
            assert_eq!(back, f, "round trip changed {json}");
        }
    }

    /// **A consumer parses the bytes a PRODUCER wrote, not the bytes this test
    /// wrote.** The goldens are the pre-re-seam crate's own output, so this is
    /// the frozen wire going into the new parser.
    #[test]
    fn the_frozen_bytes_parse_into_the_frame_they_came_from() {
        assert_eq!(
            serde_json::from_str::<ShapeWire>(GOLD_LOADED).expect("parse the export"),
            loaded()
        );
        assert_eq!(
            serde_json::from_str::<ShapeWire>(GOLD_LIVE).expect("parse the live frame"),
            live()
        );
        assert_eq!(
            serde_json::from_str::<ShapeWire>(GOLD_DEAD).expect("parse the death frame"),
            dead()
        );
    }

    /// **NO BEHAVIOUR CHANGE ON THE WIRE.** `Deserialize` cannot alter
    /// serialization, but `#[serde(default)]`, a renamed field or a type swapped
    /// to make a field parseable all can. Two of this module's fields DID change
    /// type to become parseable (`ShapeInfo::tracking` and
    /// `Derived::Transition::cell`, both `&'static str` -> `String`); these bytes
    /// are the proof that neither moved a byte.
    #[test]
    fn the_bytes_a_producer_emits_are_byte_identical_to_99e998b() {
        assert_eq!(serde_json::to_string(&loaded()).unwrap(), GOLD_LOADED);
        assert_eq!(serde_json::to_string(&live()).unwrap(), GOLD_LIVE);
        assert_eq!(serde_json::to_string(&dead()).unwrap(), GOLD_DEAD);
        assert_eq!(serde_json::to_string(&an_info()).unwrap(), GOLD_INFO);
    }

    /// **AN EXPORT NAMES THE SUBSCRIBE IT SATISFIES, AND A FRAME THAT SATISFIES
    /// NONE SAYS NOTHING.**
    ///
    /// Rule 2.7's `exported` stage for `shapes.subscribe`. The field is skipped
    /// when empty, which is every derived frame — so `GOLD_LIVE` and `GOLD_LOADED`
    /// are untouched by its existing, and this asserts both halves.
    #[test]
    fn an_export_can_name_the_subscribe_it_satisfies() {
        let mut e = loaded();
        e.subscribes = vec![12];
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""subscribes":[12]"#), "{json}");
        assert_eq!(serde_json::from_str::<ShapeWire>(&json).unwrap(), e);

        // And absent — not `[]` — when a frame satisfies no subscribe.
        let v = serde_json::to_value(live()).unwrap();
        assert!(v.get("subscribes").is_none(), "{v}");
        assert_eq!(serde_json::to_string(&loaded()).unwrap(), GOLD_LOADED);
    }

    /// **THE ACK VOCABULARY PARSES — AND NOTHING EMITS ONE YET.**
    ///
    /// Rule 2.3 amended: an ack says what receipt DID. This pins the shape so the
    /// two unbuilt halves of 2.2's ack path (a per-connection serial, a producer
    /// retention bound) are built against a settled type. The test asserts the
    /// round trip and the skip; it does NOT assert that any producer reads one,
    /// because none does.
    #[test]
    fn an_ack_round_trips_with_and_without_rejections() {
        let clean = Ack {
            shape: "accounts".to_string(),
            received_through: 41,
            rejected: Vec::new(),
        };
        let json = serde_json::to_string(&clean).unwrap();
        assert_eq!(
            json, r#"{"shape":"accounts","received_through":41}"#,
            "applied is the default and costs nothing to say"
        );
        assert_eq!(serde_json::from_str::<Ack>(&json).unwrap(), clean);

        let dirty = Ack {
            shape: "accounts".to_string(),
            received_through: 41,
            rejected: vec![
                Rejected {
                    serial: 39,
                    reason: reject::UNKNOWN_KIND.to_string(),
                    detail: Some("gauge_swept".to_string()),
                },
                Rejected {
                    serial: 40,
                    reason: reject::NO_EXPORT.to_string(),
                    detail: None,
                },
            ],
        };
        let json = serde_json::to_string(&dirty).unwrap();
        assert_eq!(serde_json::from_str::<Ack>(&json).unwrap(), dirty, "{json}");
        assert!(
            json.contains(r#"{"serial":40,"reason":"no_export"}"#),
            "a reason with no detail carries no null: {json}"
        );
        for r in &dirty.rejected {
            assert!(
                reject::ALL.contains(&r.reason.as_str()),
                "a rejection reason is a CODE from the closed set, not prose: {}",
                r.reason
            );
        }
    }

    /// The `skip_serializing_if` fields are still skipped — asserted as the
    /// ABSENCE of keys rather than only as a whole string, so the reason a
    /// failure happened is legible.
    #[test]
    fn the_skipped_fields_are_still_absent_not_null() {
        let v = serde_json::to_value(live()).unwrap();
        for k in [
            "promises",
            "subscribes",
            "key",
            "as_of",
            "cells",
            "row",
            "shared",
            "shared_cells",
        ] {
            assert!(
                v.get(k).is_none(),
                "`{k}` must be absent on a live frame, not null: {v}"
            );
        }
        let v = serde_json::to_value(loaded()).unwrap();
        assert!(
            v.get("at").is_none(),
            "an export carries `as_of`, never `at`: {v}"
        );
        assert!(
            v.get("event").is_none(),
            "an export carries no occurrence: {v}"
        );
    }

    /// [`CellState`] is the type a mirror renders `?` from, so all three values
    /// have to survive the trip — including `since_init`, which is skipped when
    /// false and therefore has to DEFAULT on the way in rather than fail.
    #[test]
    fn every_cell_state_round_trips_and_keeps_its_bytes() {
        let cases = [
            (
                CellState::Known {
                    complete: true,
                    folded_at: at("2026-09-25T12:00:00Z"),
                    since_init: false,
                    covers_since: None,
                },
                GOLD_CELL_KNOWN,
            ),
            (
                CellState::Known {
                    complete: false,
                    folded_at: at("2026-09-25T12:00:00Z"),
                    since_init: true,
                    covers_since: Some(at("2026-09-25T11:00:00Z")),
                },
                GOLD_CELL_WINDOWED,
            ),
            (
                CellState::NotHydrated { reason: None },
                GOLD_CELL_NOT_HYDRATED,
            ),
            (
                CellState::Dead {
                    reason: "gone".to_string(),
                },
                GOLD_CELL_DEAD,
            ),
        ];
        for (state, gold) in cases {
            assert_eq!(serde_json::to_string(&state).unwrap(), gold, "bytes moved");
            assert_eq!(
                serde_json::from_str::<CellState>(gold).expect("parse"),
                state,
                "a mirror could not read {gold} back"
            );
        }
        // The whole point of `#[serde(default)]` on `since_init`: a producer
        // that skipped the field must not be unparseable.
        assert!(matches!(
            serde_json::from_str::<CellState>(GOLD_CELL_KNOWN).unwrap(),
            CellState::Known {
                since_init: false,
                ..
            }
        ));
    }

    /// All four derived events, which is what a mirror applies between exports.
    #[test]
    fn every_derived_event_round_trips() {
        let cases = [
            (
                Derived::Row {
                    row: json!({"a": 1}),
                },
                r#"{"kind":"row","row":{"a":1}}"#,
            ),
            (Derived::Gone, r#"{"kind":"gone"}"#),
            (
                Derived::Transition {
                    cell: "moves".to_string(),
                    item: json!({"from": "idle", "to": "busy"}),
                },
                r#"{"kind":"transition","cell":"moves","item":{"from":"idle","to":"busy"}}"#,
            ),
            (
                Derived::Shared {
                    shared: json!({"warn_below_days": 3}),
                },
                r#"{"kind":"shared","shared":{"warn_below_days":3}}"#,
            ),
        ];
        for (event, gold) in cases {
            assert_eq!(serde_json::to_string(&event).unwrap(), gold);
            assert_eq!(serde_json::from_str::<Derived>(gold).expect("parse"), event);
        }
    }

    /// **`shapes.list` survives the trip, flattened counter and all.** The
    /// flatten is the delicate one: on the way out it puts `unplaced_status` at
    /// the top level where `threads_tui/render.py:168` reads it, and on the way
    /// in it has to collect that key back into `counters` without swallowing a
    /// named field.
    #[test]
    fn shape_info_round_trips_with_its_flattened_counter() {
        let info = an_info();
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(json, GOLD_INFO, "the board's wire moved");

        let back: ShapeInfo = serde_json::from_str(&json).expect("a reader must parse shapes/list");
        assert_eq!(back, info);
        assert_eq!(
            back.counters.get("unplaced_status").copied(),
            Some(3),
            "the flattened counter came back INTO the map, not as a lost key"
        );
        assert_eq!(back.tracking, "lazy");
        assert_eq!(
            back.consumers.first().and_then(|c| c.name.clone()),
            Some("threads-tui".to_string()),
            "who is subscribed came back too"
        );
        assert!(
            !json.contains("\"counters\""),
            "still flattened, never a second spelling: {json}"
        );
    }

    /// [`ShapeError`] is the refusal a consumer has to act on. It is not on any
    /// wire this crate writes (see its own docs), so this asserts only that it
    /// survives a trip through one a consumer might.
    #[test]
    fn a_refusal_round_trips() {
        let errors = [
            ShapeError::Unknown("nope".to_string()),
            ShapeError::Dead {
                shape: "accounts".to_string(),
                reason: "the shape task panicked".to_string(),
            },
            ShapeError::BadAt("3026-01-01T00:00:00Z is more than 24h ahead".to_string()),
            ShapeError::TooManyTimers {
                shape: "accounts".to_string(),
                max: 4,
            },
        ];
        for e in errors {
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<ShapeError>(&json).expect("parse"), e);
        }
    }

    /// The stamp carrier a mirror holds a windowed item in.
    #[test]
    fn a_stamped_value_round_trips() {
        let s = Stamped::resolving(
            at("2026-09-25T12:00:00Z"),
            json!({"n": 1}),
            Resolving::keyed("anthropic:sam", vec![7]),
        );
        let json = serde_json::to_string(&s).unwrap();
        let back: Stamped<Value> = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, s);
        // The ordinary input skips the field entirely, so a stamp carrier costs
        // nothing to say "this resolves nothing".
        let plain = Stamped::new(at("2026-09-25T12:00:00Z"), json!({"n": 1}));
        let json = serde_json::to_string(&plain).unwrap();
        assert!(!json.contains("promises"), "{json}");
        assert_eq!(
            serde_json::from_str::<Stamped<Value>>(&json).expect("parse"),
            plain
        );
    }

    /// **THE ROUTING, AT THE LEVEL WHERE IT IS DECIDABLE.** Which frame an id
    /// rides is the whole of the promise defect, and it is decided here.
    #[test]
    fn ids_are_routed_to_the_subject_they_are_about() {
        let mut r = Resolving::keyed("aa", vec![1, 2]);
        r.merge(Resolving::keyed("bb", vec![3]));
        r.merge(Resolving::shape_level(vec![9]));

        assert_eq!(r.for_key("aa"), vec![1, 2]);
        assert_eq!(r.for_key("bb"), vec![3]);
        assert!(r.for_key("cc").is_empty(), "no ids for a key nobody named");
        assert_eq!(r.ids(), vec![1, 2, 3, 9], "keys in order, then keyless");

        // A take REMOVES, so one id cannot ride two keys' frames.
        assert_eq!(r.take_key("aa"), vec![1, 2]);
        assert!(r.for_key("aa").is_empty());
        assert_eq!(r.take_key("aa"), Vec::<u64>::new(), "and it is gone");

        // Put-back is what keeps an id whose frame never went out visible.
        r.put_back("aa".to_string(), vec![1, 2]);
        assert_eq!(r.for_key("aa"), vec![1, 2]);
        r.put_back("aa".to_string(), Vec::new());
        assert_eq!(r.for_key("aa"), vec![1, 2], "putting back nothing is nothing");

        assert_eq!(r.take_keyless(), vec![9]);
        assert!(r.take_keyless().is_empty());
        assert!(!r.is_empty());
        let _ = r.take_key("aa");
        let _ = r.take_key("bb");
        assert!(r.is_empty());
        assert!(Resolving::none().is_empty());
        // An empty id list mints no key at all, so `keys()` never reports a
        // subject with nothing to deliver.
        assert_eq!(Resolving::keyed("aa", Vec::new()).keys().count(), 0);
    }
}
