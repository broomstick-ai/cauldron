//! # Fold — the cells a PRODUCER folds its inputs into
//!
//! `wire` is what a consumer parses. This is the other half: the small set of
//! converging cells a shape author assembles a [`Root`] out of, plus the
//! [`Outbox`] that one fold's worth of change leaves behind for the chassis to
//! page onto the wire.
//!
//! **There is no tokio in here and there is no `hub` in here.** The flag line
//! this module sits on is not "runtime or not", it is "write or read": a mirror
//! links the floor and nothing else, a producer authoring a shape links this,
//! and only a producer that wants the loop *driven* links `hub` and pays for an
//! async runtime. The first cut of this crate drew the line at tokio instead,
//! which put three write-only entries (`Register::emit`, `Map::take_dirty_keys`,
//! `Root::drain`) in the always-compiled floor that every face vendors, and put
//! the frame types a mirror has to parse behind an async runtime it does not
//! want. Both halves were wrong about the same thing and neither was wrong
//! about tokio.
//!
//! ## The two entries, and why every cell has exactly two
//!
//! | entry | who calls it | what the cell records |
//! |---|---|---|
//! | `emit` / `add` / `touch` / `append` | the live fold, per input | the value AND its consequence |
//! | `read` / `mark_*` | the hydrate, once | the value, and nothing else |
//!
//! Which one a handler calls is decided by the CELL TYPE and never by the
//! handler, so routing an input is a lookup and not a judgment. The setter is
//! spelled `emit` because the name is the warning: a fold that moves a cell has
//! published, every subscriber will see it, and nobody should be able to do that
//! by reaching for something that reads like a field assignment.
//!
//! ## THE STAMP RULE, which is the one thing a shape author must hold
//!
//! Every cell carries `folded_at`. **An input stamped at or before `folded_at`
//! sets the level and records nothing else** — no [`Edge`] transition, no
//! [`Sequence`] append, no [`Recency`] reorder.
//!
//! It exists for one window of time. A hydrate read answers "as of T", and the
//! events that were already in flight when it ran arrive afterwards carrying
//! stamps at or before T. For a level ([`Register`], [`Semilattice`], [`Map`])
//! re-folding them is harmless — the same value lands twice. For anything that
//! records a CHANGE it is not harmless: the cell would manufacture a transition
//! out of an event the read had already accounted for, and hand every mirror a
//! state change that never happened.
//!
//! [`Sequence::append`] is the one documented way out of it, and the reason is
//! in its own doc: a log whose order is a producer's own monotone key has a
//! better guard available than time, and the caller owes it.
//!
//! Nothing here knows what a thread, an account or a push is. The contract is
//! `docs/shapes.md` rules 2.1–2.9 in substrate-interface-layer, §6.2 for the
//! generic-over-the-producer property.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::Duration as ChronoDuration;
use serde::Serialize;
use serde_json::Value;

use crate::wire::{CellState, Stamped, Timestamp};

// ─────────────────────────────────────────────────────────────────────────────
// The shared state arithmetic
//
// Six cells answer the same four questions about their own provenance, and in
// the first implementation each of them answered all four inline. The repeated
// block was not merely long: the "carry both flags" rule below was written out
// three times, in three cells, and a fix to one of the three is a fix to one of
// the three. So the arithmetic lives here once and the cells call it.
// ─────────────────────────────────────────────────────────────────────────────

/// The state a hydrate leaves: a real value, the producer's own claim about
/// whether its read was whole, and **`since_init: false` unconditionally**,
/// because a cell that has been read is standing on history rather than on
/// whatever it happened to witness after its own birth. There is no hydrate in
/// this module that leaves a cell `since_init`, so the flag is not a parameter.
fn hydrated(as_of: Timestamp, complete: bool, covers_since: Option<Timestamp>) -> CellState {
    CellState::Known {
        complete,
        folded_at: as_of,
        since_init: false,
        covers_since,
    }
}

/// The state a cell with NO hydrate entry is in once something has happened in
/// it: whole, because nothing was missed, and `since_init` because what it
/// holds starts at the shape's own birth rather than at any statement about the
/// world.
fn from_init(at: Timestamp, covers_since: Option<Timestamp>) -> CellState {
    CellState::Known {
        complete: true,
        folded_at: at,
        since_init: true,
        covers_since,
    }
}

/// Nothing ever reached this cell, and here is the reason. Distinct from empty
/// and distinct from dead — the whole point of a three-valued state.
fn unreached(reason: impl Into<String>) -> CellState {
    CellState::NotHydrated {
        reason: Some(reason.into()),
    }
}

/// The fold behind this cell is gone; everything under it is last-known.
fn expired(reason: &str) -> CellState {
    CellState::Dead {
        reason: reason.to_string(),
    }
}

/// **Is `at` on the far side of the hydration edge?** The stamp rule, as one
/// predicate, called by every cell that records a change rather than a level.
///
/// `None` from `folded_at()` is a cell that has never folded anything, and
/// there is no edge to be behind: the first input through an un-hydrated cell is
/// always live.
fn already_folded(state: &CellState, at: Timestamp) -> bool {
    state.folded_at().is_some_and(|edge| at <= edge)
}

/// **WHAT A LIVE WRITE CARRIES OUT OF THE STATE IT REPLACES** — `(complete,
/// hydrated)`.
///
/// Both halves are carried and NEITHER is recomputed, and the reason is the same
/// for both: a live input is evidence about the world, not evidence about the
/// read that preceded it. An append tells you nothing about whether that read
/// was short, and a cell that was properly hydrated does not stop being
/// hydrated because something then happened in it.
///
/// The default for a cell that is not `Known` — never hydrated, or dead — is
/// `(true, false)`: nothing is known to be missing, and nothing was read. That
/// is what makes a cell with no `read` entry report `since_init` for ever, which
/// is the honest answer and the one [`Counter`] is built on.
fn carried(state: &CellState) -> (bool, bool) {
    match state {
        CellState::Known {
            complete,
            since_init,
            ..
        } => (*complete, !*since_init),
        _ => (true, false),
    }
}

/// The state a live write leaves on a windowed cell, with [`carried`] applied.
fn advanced(prev: &CellState, at: Timestamp, covers_since: Option<Timestamp>) -> CellState {
    let (complete, was_hydrated) = carried(prev);
    CellState::Known {
        complete,
        folded_at: at,
        since_init: !was_hydrated,
        covers_since,
    }
}

/// **ONE ITEM, ONE RENDERING.** Serialise `value`, then graft `extra` onto it —
/// and only if it serialised to an OBJECT.
///
/// The grafting is conditional for a reason that is a contract and not a
/// convenience. An item of a windowed cell reaches a consumer twice in its life:
/// inside the array an export carries, and inside the frame that announced it.
/// Those are one item, so they get one rendering here and not two expressions in
/// two methods — the version of this module that had two expressions shipped a
/// real defect, where a consumer assembling a window from an export plus the
/// transitions after it ended up holding items that carried `at` beside items
/// that did not, in one list.
///
/// A `Sequence<u64>` has nowhere to put a key. Inventing a wrapper for the
/// scalar case would change every byte every existing scalar window puts on the
/// wire, so the scalar is left exactly as it serialised.
fn grafted<T: Serialize>(value: &T, extra: &[(&str, String)]) -> Value {
    let mut out = serde_json::to_value(value).unwrap_or(Value::Null);
    if let Value::Object(fields) = &mut out {
        for (k, v) in extra {
            fields.insert((*k).to_string(), Value::String(v.clone()));
        }
    }
    out
}

/// A windowed item as both halves of the wire spell it: the value, plus the
/// stamp of when the thing happened.
fn rendered_item<T: Serialize>(value: &T, at: Timestamp) -> Value {
    grafted(value, &[("at", at.to_rfc3339())])
}

// ─────────────────────────────────────────────────────────────────────────────
// Converging values: the `Join` vocabulary
// ─────────────────────────────────────────────────────────────────────────────

/// **A value that only ever moves one way.**
///
/// The three laws an implementation owes, and the reason each one is load
/// bearing here rather than a textbook ornament:
///
/// * **idempotent** — `join(a, a)` is `a`. A duplicate delivery is free.
/// * **commutative** — `join(a, b)` is `join(b, a)`. Arrival order cannot be
///   relied on across a reconnect, so it must not matter.
/// * **associative** — the grouping cannot change the answer. A mirror that
///   folded two inputs before a third must agree with one that folded the third
///   first.
///
/// Together they are what makes a replay, a duplicate and a re-hydrate converge
/// on the same held value instead of three different ones — which is rule 2.5's
/// claim, stated as an algebraic property rather than as a hope about delivery.
pub trait Join: Clone {
    fn join(&mut self, other: &Self);
}

/// **First-seen wins.** The join keeps whichever operand is lower, so the cell
/// records the earliest value it was ever told about and never moves forward
/// again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Min<T: Ord + Clone>(pub T);

impl<T: Ord + Clone> Join for Min<T> {
    fn join(&mut self, other: &Self) {
        if other.0 < self.0 {
            self.0 = other.0.clone();
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The level cells
// ─────────────────────────────────────────────────────────────────────────────

/// **Last writer wins, by input stamp.** The cell nearly every field is.
#[derive(Debug, Clone)]
pub struct Register<T> {
    value: Option<T>,
    state: CellState,
    /// The ordering key of the held value, when this register is ordered by a
    /// FIELD of its value rather than by the stamp of the input that carried it
    /// (`stamped_by(observed_at)`, §2.6). `None` until something arrives
    /// through [`Self::emit_ordered`].
    order: Option<Timestamp>,
    dirty: bool,
}

impl<T> Default for Register<T> {
    fn default() -> Self {
        Self {
            value: None,
            state: CellState::default(),
            order: None,
            dirty: false,
        }
    }
}

impl<T> Register<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// **THE LIVE ENTRY.** Takes the value and stamps the cell with the input's
    /// own stamp.
    ///
    /// For a last-writer-wins cell the live entry and the hydrate entry are the
    /// SAME operation — take the value, record when — and they differ only in
    /// which clock the stamp came off. That is why both route through one place
    /// here, and it is emphatically NOT true of the cells that record a change:
    /// an [`Edge`] or a [`Sequence`] does different work on the two paths, which
    /// is what the stamp rule is about.
    pub fn emit(&mut self, at: Timestamp, v: T) {
        self.land(at, v);
    }

    fn land(&mut self, stamped_at: Timestamp, v: T) {
        self.value = Some(v);
        self.state = CellState::known(stamped_at);
        self.dirty = true;
    }

    /// **Live set, ordered by a field of the VALUE** — the `stamped_by` variant.
    ///
    /// A reading observed at 10:00 that reaches the producer at 10:05, after one
    /// observed at 10:02 has already landed, is not news: it is the older fact,
    /// delivered late. Ordering by the input stamp would apply it and move the
    /// gauge backwards; ordering by the value's own key DISCARDS it, which is
    /// what makes a replayed or re-ordered observation harmless rather than
    /// destructive.
    ///
    /// The discard needs both halves of its condition. An ordering key that is
    /// not newer is only grounds to refuse when there is something to refuse it
    /// in favour of, so a register that is not yet `Known` takes the value
    /// whatever its key says.
    pub fn emit_ordered(&mut self, at: Timestamp, order: Timestamp, v: T) {
        if self.outranked_by_held(order) {
            return;
        }
        self.value = Some(v);
        self.order = Some(order);
        self.state = CellState::known(at);
        self.dirty = true;
    }

    fn outranked_by_held(&self, order: Timestamp) -> bool {
        match self.order {
            Some(held) => order <= held && self.state.is_known(),
            None => false,
        }
    }

    /// **THE HYDRATE ENTRY.** The read is the authority, and the cell's
    /// `folded_at` becomes the instant the read speaks for — which is what every
    /// subsequent live input is compared against.
    pub fn read(&mut self, as_of: Timestamp, v: T) {
        self.land(as_of, v);
    }

    /// Hydrate set from a partial read — the value is a fact, the picture behind
    /// it is admittedly short, and the export says so.
    pub fn read_partial(&mut self, as_of: Timestamp, v: T) {
        self.value = Some(v);
        self.state = hydrated(as_of, false, None);
        self.dirty = true;
    }

    /// **The hydrate never reached this cell**, with the reason. Not the same
    /// claim as empty, and not the same claim as dead.
    pub fn not_hydrated(&mut self, reason: impl Into<String>) {
        self.state = unreached(reason);
        self.dirty = true;
    }

    /// Read what is held. Publishes nothing.
    pub fn now(&self) -> Option<&T> {
        self.value.as_ref()
    }

    pub fn state(&self) -> &CellState {
        &self.state
    }

    pub fn mark_dead(&mut self, reason: &str) {
        self.state = expired(reason);
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
}

/// **A cell that JOINS its inputs instead of setting them** — `Min<Timestamp>`
/// for a first-seen instant, a max for a high-water mark, an or for a latch.
///
/// Order-insensitivity is the whole product: see [`Join`] for the three laws the
/// inner value owes, and note that this cell cannot check them. A `Join` that
/// does nothing at all satisfies every one of the three and holds the first
/// value it was given for ever; a `Join` that overwrites satisfies idempotence
/// alone. Both compile, both pass a law suite in part, and only a test that
/// pins the SEMANTICS of the specific lattice can tell either of them from a
/// correct one.
#[derive(Debug, Clone)]
pub struct Semilattice<T: Join> {
    value: Option<T>,
    state: CellState,
    dirty: bool,
}

impl<T: Join> Default for Semilattice<T> {
    fn default() -> Self {
        Self {
            value: None,
            state: CellState::default(),
            dirty: false,
        }
    }
}

impl<T: Join> Semilattice<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// **Live JOIN, never a set** — which is exactly why the order the inputs
    /// arrive in cannot matter. An empty cell takes the value; a cell with a
    /// value folds the new one into it.
    pub fn emit(&mut self, at: Timestamp, v: T) {
        match &mut self.value {
            Some(held) => held.join(&v),
            empty => *empty = Some(v),
        }
        self.state = CellState::known(at);
        self.dirty = true;
    }

    /// **Hydrate SET, not a join.** The read is an authority making a statement
    /// about the whole of history, not one more contribution to be merged with
    /// whatever the cell happened to accumulate first.
    pub fn read(&mut self, as_of: Timestamp, v: T) {
        self.value = Some(v);
        self.state = CellState::known(as_of);
        self.dirty = true;
    }

    pub fn now(&self) -> Option<&T> {
        self.value.as_ref()
    }

    pub fn state(&self) -> &CellState {
        &self.state
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn mark_dead(&mut self, reason: &str) {
        self.state = expired(reason);
    }
}

/// **A true counter: `add` only, and there is deliberately NO hydrate entry.**
///
/// Having no `read` is what makes it `since_init` by construction, which in turn
/// forces eager tracking at `register` — a counter that only counts what it
/// witnessed cannot be allowed to start witnessing at the first subscribe.
///
/// It is also the cell to reach for LAST. A total is only correctly a counter
/// where the producer does not retain the underlying facts: where it does, the
/// honest spellings are a difference of two durable monotone positions (a
/// [`Register`], as `unread = last_seq - last_seen_seq` is) or a count the
/// consumer derives over the rows it holds. `threads` keeps its whole ledger, so
/// the first shape ported to this chassis has no instance of this type at all.
///
/// **`add` is not idempotent, and that is the contract rather than a defect.**
/// Two deliveries of one event are two increments. Every other cell here
/// converges under duplication; this one accumulates, which is why it is the
/// only one that cannot be re-derived by replaying inputs into a fresh shape.
#[derive(Debug, Clone, Default)]
pub struct Counter {
    n: u64,
    state: CellState,
    dirty: bool,
}

impl Counter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, at: Timestamp, n: u64) {
        self.n += n;
        self.state = from_init(at, None);
        self.dirty = true;
    }

    pub fn now(&self) -> u64 {
        self.n
    }

    pub fn state(&self) -> &CellState {
        &self.state
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The keyed cell
// ─────────────────────────────────────────────────────────────────────────────

/// **The root cell of a keyed shape**, and, nested inside one, any per-key
/// sub-map.
///
/// # Two sets, not one
///
/// A key can EXIST without being HELD. The map keeps the rows it holds and,
/// separately, every key it knows to be in the pool:
///
/// * a `Whole` root reads its key set at init and mints a row for each, so the
///   two sets are equal by construction and nobody has to think about it;
/// * a `PerKey` root marks a key's existence without minting a row, and the two
///   sets come apart on purpose. That is what lets a shape over a 2.46 M-event
///   ledger answer *what exists* without its root growing to hold the whole
///   fleet: one set of ids costs about 200 KB at 5,417 threads, against roughly
///   20 MB for the rows.
///
/// Each set carries its own [`CellState`], because one state word cannot say
/// both things: a `PerKey` root holds zero rows *completely* while its
/// membership is `NotHydrated` because the producer cannot enumerate its
/// namespace at all.
///
/// # Dirt is marked on REACH, not on write
///
/// [`Self::key`] dirties the key it hands back, before the caller has changed
/// anything. That is deliberate and it is the departure from the consumed-guard
/// machinery the design originally called for: a fold reaching for a key is a
/// fold about to change it, and the cost of being wrong is one extra `row`
/// frame, which is an idempotent upsert and never a wrong value. The burst
/// counter (§2.7) is the instrument that catches a fold reaching for keys it
/// does not touch — not the type system.
#[derive(Debug, Clone)]
pub struct Map<K: Ord + Clone, C> {
    entries: BTreeMap<K, C>,
    members: BTreeSet<K>,
    /// Keys that joined the pool since the last drain — the `added` events.
    added: Vec<K>,
    /// Provenance of the MEMBERSHIP set, independent of `state`.
    members_state: CellState,
    /// Provenance of the ROW set.
    state: CellState,
    dirty: BTreeSet<K>,
    removed: Vec<K>,
}

impl<K: Ord + Clone, C> Default for Map<K, C> {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            members: BTreeSet::new(),
            added: Vec::new(),
            members_state: CellState::default(),
            state: CellState::default(),
            dirty: BTreeSet::new(),
            removed: Vec::new(),
        }
    }
}

impl<K: Ord + Clone, C: Default> Map<K, C> {
    pub fn new() -> Self {
        Self::default()
    }

    /// The cell block for `k`, MINTING it if this root does not hold one yet,
    /// and marking the key dirty on the way through.
    ///
    /// **Holding implies membership.** A root with a row for a key is a root
    /// asserting that key exists, so this marks membership too and there is no
    /// second call a shape author can forget to make. It is why a `PerKey`
    /// hydrate of one key needs no membership statement of its own.
    pub fn key(&mut self, k: K) -> &mut C {
        self.dirty.insert(k.clone());
        self.note_member(k.clone());
        self.entries.entry(k).or_default()
    }

    /// **THIS KEY EXISTS — WITHOUT MINTING A ROW FOR IT.** The one call a
    /// `PerKey` fold makes about a key it is not going to hold.
    ///
    /// The first statement about a key yields an `added` event; every repeat is
    /// silent. The silence is load bearing at the only scale that matters: a
    /// ledger folding 50 events a second would otherwise publish 50 `added`
    /// frames a second for keys every consumer has known about since init.
    pub fn member(&mut self, k: K) {
        self.note_member(k);
    }

    fn note_member(&mut self, k: K) {
        if self.members.insert(k.clone()) {
            self.added.push(k);
        }
    }

    /// Is this key in the pool — held or not?
    pub fn is_member(&self, k: &K) -> bool {
        self.members.contains(k)
    }

    /// Every key that EXISTS, in key order. For every key HELD, see
    /// [`Self::keys`]; on a `PerKey` root the two answers differ.
    pub fn members(&self) -> impl Iterator<Item = &K> {
        self.members.iter()
    }

    pub fn members_len(&self) -> usize {
        self.members.len()
    }

    pub fn members_state(&self) -> &CellState {
        &self.members_state
    }

    /// The producer enumerated the pool at `as_of`. `complete: false` is a short
    /// read, so a consumer counting the roster knows it has a floor rather than
    /// the set.
    pub fn mark_members_read(&mut self, as_of: Timestamp, complete: bool) {
        self.members_state = hydrated(as_of, complete, None);
    }

    /// **THE PRODUCER CANNOT ENUMERATE THIS POOL**, with the reason — the
    /// three-valued state earning its keep, rather than a `MEMBERS: bool` flag
    /// that could not carry one.
    pub fn mark_members_not_hydrated(&mut self, reason: impl Into<String>) {
        self.members_state = unreached(reason);
    }

    pub fn take_added(&mut self) -> Vec<K> {
        std::mem::take(&mut self.added)
    }

    /// Reach a key for mutation and DO NOT dirty it — the drain path, which is
    /// collecting what a fold already published, not publishing anything.
    pub fn get_mut(&mut self, k: &K) -> Option<&mut C> {
        self.entries.get_mut(k)
    }

    /// Read a key without dirtying it — for a fold that must look before it
    /// decides, and for the export path.
    pub fn get(&self, k: &K) -> Option<&C> {
        self.entries.get(k)
    }

    pub fn contains(&self, k: &K) -> bool {
        self.entries.contains_key(k)
    }

    /// **THE KEY LEFT THE POOL.** The row goes, the membership goes with it, and
    /// a `gone` event falls out at Tx close — which is what makes `gone` the
    /// honest twin of `added` rather than a vaguer "I stopped holding this".
    ///
    /// Exactly one `gone` is owed however much was dropped, so a `PerKey` key
    /// that was a member with no row yields one and a held key yields one. A
    /// key that was neither yields nothing, because nothing happened.
    ///
    /// An `added` still waiting to be drained is RETRACTED rather than published
    /// beside its own `gone`: a key that arrived and left inside one Tx was
    /// never news.
    ///
    /// **This is not how a key leaves one subscriber's held set.** A
    /// per-subscription scope exit has to be built as un-holding, never as this
    /// call: a key one connection stopped watching has not left the pool, and
    /// spelling the two the same way is the collision `docs/access-patterns.md`
    /// §6.8 names.
    pub fn remove(&mut self, k: &K) -> Option<C> {
        let row = self.entries.remove(k);
        let was_member = self.members.remove(k);
        if row.is_some() {
            self.dirty.remove(k);
        }
        if was_member {
            self.added.retain(|pending| pending != k);
        }
        if row.is_some() || was_member {
            self.removed.push(k.clone());
        }
        row
    }

    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.keys()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&K, &C)> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn state(&self) -> &CellState {
        &self.state
    }

    /// The ROW set is hydrated as of `as_of`. It touches no row: the rows were
    /// written through [`Self::key`] by the shape's own hydrate, and this is the
    /// statement about them.
    pub fn mark_read(&mut self, as_of: Timestamp, complete: bool) {
        self.state = hydrated(as_of, complete, None);
    }

    pub fn mark_not_hydrated(&mut self, reason: impl Into<String>) {
        self.state = unreached(reason);
    }

    pub fn mark_since_init(&mut self, at: Timestamp) {
        self.state = from_init(at, None);
    }

    /// The fold is gone. **Both state words go**, because the task that was
    /// maintaining the membership is the task that has died: a roster read off
    /// this root afterwards is history and has to say so.
    ///
    /// Sub-cells are not reached from here. A map owns two state words and
    /// nothing below them; each held cell is marked by whoever owns it.
    pub fn mark_dead(&mut self, reason: &str) {
        self.state = expired(reason);
        self.members_state = expired(reason);
    }

    pub fn take_dirty_keys(&mut self) -> BTreeSet<K> {
        std::mem::take(&mut self.dirty)
    }

    pub fn take_removed(&mut self) -> Vec<K> {
        std::mem::take(&mut self.removed)
    }

    pub fn entries_mut(&mut self) -> impl Iterator<Item = (&K, &mut C)> {
        self.entries.iter_mut()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The windowed cells
// ─────────────────────────────────────────────────────────────────────────────

/// **A windowed LOG, bounded twice: `capacity: N` and `window: D`.**
///
/// Every append appends; two items in the other order are two different
/// stories; the same subject twice is two occurrences. (For the cell where the
/// same subject twice is one entry that MOVED, see [`Recency`].)
///
/// Eviction runs before every append, and the cell publishes `covers_since` —
/// the oldest stamp it still holds — so that a count a consumer derives from it
/// is qualified by what the window can still see. "Three failovers" is only
/// honest as "three since a stated instant", and this is the instant.
#[derive(Debug, Clone)]
pub struct Sequence<T> {
    items: VecDeque<Stamped<T>>,
    capacity: usize,
    /// **`None` means COUNT ONLY, NO CLOCK** — see [`Self::new_capacity_only`].
    window: Option<ChronoDuration>,
    state: CellState,
    /// Appends made in the current Tx, drained to `transition` events at close.
    pending: Vec<Value>,
}

impl<T: Serialize + Clone> Sequence<T> {
    pub fn new(capacity: usize, window: ChronoDuration) -> Self {
        Self {
            items: VecDeque::new(),
            capacity,
            window: Some(window),
            state: CellState::default(),
            pending: Vec::new(),
        }
    }

    /// **A LOG BOUNDED BY COUNT AND BY NOTHING ELSE.**
    ///
    /// A dual-bounded log answers *"the failovers of the last 24 hours"*, and
    /// for that question an empty answer after a quiet day is the TRUE answer.
    /// *"The transcript I have open"* is a different question with the same
    /// shape: the thread a consumer is looking at may have been silent for a
    /// week, and a clock that emptied the window overnight would hand that
    /// consumer nothing in precisely the state where it most wants the last
    /// thing that was said. [`Recency`] carries the argument in full; it does not
    /// hold whether the cell counting them is a log or an order.
    ///
    /// It is a separate constructor rather than a very large `window`, and that
    /// is not a style choice: `now - Duration::MAX` is not a representable
    /// instant, so "no clock" spelled as an enormous clock is an overflow
    /// waiting for an idle daemon. The absence is spelled as an absence, once,
    /// and [`Self::evict`] is the only reader of it.
    pub fn new_capacity_only(capacity: usize) -> Self {
        Self {
            items: VecDeque::new(),
            capacity,
            window: None,
            state: CellState::default(),
            pending: Vec::new(),
        }
    }

    /// Drop what `now` has carried out of the window. No-op on a cell with no
    /// clock.
    fn evict(&mut self, now: Timestamp) {
        let Some(window) = self.window else {
            return;
        };
        let floor = now - window;
        while self.items.front().is_some_and(|held| held.at < floor) {
            self.items.pop_front();
        }
    }

    /// Drop from the old end until the count fits. Separate from [`Self::evict`]
    /// because the two bounds are reached at different moments: the clock sweep
    /// is decided by the arriving stamp and can run before the push, the count
    /// can only be decided after it.
    fn trim(&mut self) {
        while self.items.len() > self.capacity {
            self.items.pop_front();
        }
    }

    /// Live append, under the stamp rule: an input at or before `folded_at`
    /// appends nothing and answers `false`.
    pub fn emit(&mut self, at: Timestamp, item: T) -> bool {
        if already_folded(&self.state, at) {
            return false;
        }
        self.append(at, item)
    }

    /// **APPEND WITH THE STAMP RULE SET ASIDE — for a log ordered by a
    /// producer's OWN MONOTONE KEY, which does the rule's job and does it
    /// strictly better.**
    ///
    /// The stamp rule stops a cell manufacturing entries out of events that were
    /// in flight when the hydrate read ran, and it does it by TIME. On a log
    /// that hydrates, time is the wrong proxy in both directions at once:
    ///
    /// * **It loses real items.** The items of a hydrated log carry the
    ///   PRODUCER'S stamps — each event's own `ts` — because those are the only
    ///   stamps the read path and the live path can both produce, and a mirror
    ///   rebuilt by re-subscribing has to hold the same stamp for the same item
    ///   as one that never dropped, or the two are not the same mirror and rule
    ///   2.5 is false. Once the item stamp is a producer's clock it is no longer
    ///   comparable with the hydrate's `as_of`: a genuinely new event whose `ts`
    ///   happens to land a millisecond before the read is refused and lost for
    ///   good. On a level that is a missed re-assertion. On a log it is a
    ///   permanent hole.
    /// * **It admits duplicates it cannot see.** Two appends of the same item
    ///   under different stamps both satisfy the stamp rule. Nothing but an
    ///   IDENTITY can refuse that pair, and a log holding one — a `seq`, a
    ///   revision, an offset — refuses it exactly.
    ///
    /// So a caller of this method owes the guard the rule is not providing: it
    /// compares the arriving item against [`Self::newest`] on its own key and
    /// appends only what genuinely follows. That is a STRONGER contract than the
    /// stamp rule, not a weaker one — identity rather than time — and it is the
    /// move `threads`' fold already makes for its own newest cell, where the key
    /// is `seq` because a wall clock can move backwards.
    ///
    /// Eviction, `pending` and the carried flags are [`Self::emit`]'s unchanged:
    /// `emit` is this function plus one stamp check, so there is one append in
    /// this cell and not two.
    pub fn append(&mut self, at: Timestamp, item: T) -> bool {
        self.evict(at);
        // A windowed item carries its stamp and nothing else. It is state the
        // shape HOLDS, not an input that resolves a promise.
        self.items.push_back(Stamped::new(at, item.clone()));
        self.trim();
        self.pending.push(rendered_item(&item, at));
        self.state = advanced(&self.state, at, self.oldest_stamp());
        true
    }

    fn oldest_stamp(&self) -> Option<Timestamp> {
        self.items.front().map(|held| held.at)
    }

    /// **THE HYDRATE ENTRY — the absence four documents were written around.**
    ///
    /// `threads`' module doc, `body.rs`'s, `recent.rs`'s and `docs/shapes.md` all
    /// recorded the same measured fact, that this cell had no `read` and a tail
    /// would therefore be `since_init` from the instant of subscription — and all
    /// four drew the same conclusion, that a thread's log could not be a shape
    /// and had to stay a cursor read. This method is that absence closed, and it
    /// is the only reason a log CAN be a shape: a cold subscriber is handed the
    /// window as it stands rather than an empty cell that fills from now on.
    ///
    /// `entries` is the producer's window **OLDEST FIRST** — the opposite of
    /// [`Recency::read`]'s order, and the opposite for the same underlying
    /// reason. This cell EXPORTS oldest-first, so holding the items that way
    /// round means nothing has to be reversed on the way out.
    ///
    /// **It keeps the newest `capacity` and drops the rest off the FRONT.** A
    /// producer that hands over more than the cell declares is not silently
    /// clamped into some other window — it is given the recent end, which is the
    /// end anyone asking for less of a window wants and the end eviction would
    /// have reached last anyway. The drop is then stated twice, structurally:
    /// `covers_since` moves, and `complete` goes `false` whatever the producer
    /// claimed about its own read.
    ///
    /// **It does not evict by time, even on a dual-bounded cell.** These items
    /// came out of a durable log rather than off the wire, so a producer handing
    /// them over is asserting that they are what the log holds; a clock sweep
    /// here would silently delete the answer to *"what was the last thing said"*
    /// for every subject quiet longer than the window. Time eviction is a LIVE
    /// policy and belongs where an arriving stamp can be compared with a `now`
    /// that is actually now.
    ///
    /// **It appends nothing to `pending`.** A hydrate publishes nothing; the
    /// export is the publication. A `read` that filled `pending` would hand
    /// every already-subscribed consumer N `transition` frames for items it
    /// either already holds or has never been offered a key for — the phantom
    /// transition, at the scale of a whole window.
    pub fn read(&mut self, as_of: Timestamp, entries: Vec<(Timestamp, T)>, complete: bool) {
        let over = entries.len().saturating_sub(self.capacity);
        self.items = entries
            .into_iter()
            .skip(over)
            .map(|(at, value)| Stamped::new(at, value))
            .collect();
        self.state = hydrated(as_of, complete && over == 0, self.oldest_stamp());
    }

    /// The newest item held, for a fold that must compare an arriving item
    /// against the head before deciding to append — see [`Self::append`], which
    /// is the method that makes this one necessary. This cell orders by ARRIVAL
    /// and cannot know a producer's own key.
    pub fn newest(&self) -> Option<&T> {
        self.items.back().map(|held| &held.value)
    }

    /// A log with no hydrate entry covers only what the shape has seen, from the
    /// instant it initialised — and the export says so.
    pub fn mark_since_init(&mut self, at: Timestamp) {
        self.state = from_init(at, Some(at));
    }

    pub fn now(&self) -> impl Iterator<Item = &T> {
        self.items.iter().map(|held| &held.value)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn state(&self) -> &CellState {
        &self.state
    }

    pub fn to_json(&self) -> Value {
        self.to_json_retained(crate::wire::Retain::Max)
    }

    /// **THE WINDOW, PROJECTED TO WHAT THIS SUBSCRIBER ASKED FOR.**
    ///
    /// The newest `n`, because the recent end is the end anyone asking for less
    /// of a window wants and the oldest items are the ones eviction takes first
    /// regardless. Order inside the answer is unchanged — oldest first, as this
    /// cell has always exported — so a consumer that asked for fewer items reads
    /// the same array shape, shorter at the front.
    ///
    /// **It evicts nothing.** What one subscriber asks for cannot change what
    /// the cell holds, and every other subscriber's frame is built from the same
    /// untouched items. That is the whole of the who-pays answer, in one method.
    pub fn to_json_retained(&self, retain: crate::wire::Retain) -> Value {
        let shown = retain.take(self.items.len());
        let from = self.items.len() - shown;
        let rendered: Vec<Value> = self
            .items
            .iter()
            .skip(from)
            .map(|held| rendered_item(&held.value, held.at))
            .collect();
        Value::Array(rendered)
    }

    pub fn take_pending(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.pending)
    }

    pub fn mark_dead(&mut self, reason: &str) {
        self.state = expired(reason);
    }

    /// Module-private: an [`Edge`] whose level could not be read has to put its
    /// transition log into the same state, and reaching into the field from
    /// outside the impl would make the invariant somebody else's to maintain.
    /// Deliberately NOT public — a log that hydrates has `read`, and a log that
    /// does not is `since_init`; neither needs this.
    fn set_not_hydrated(&mut self, reason: String) {
        self.state = unreached(reason);
    }
}

/// **A bounded, deduped, newest-first ORDER over named members** — the cell an
/// INDEX is made of, and the windowed cell here that HYDRATES.
///
/// # Why this is not a [`Sequence`]
///
/// A log and an order are different things, and the operator named the
/// difference when he asked for *"a recency sequence across fleet, maybe that
/// AGGREGATED BY THREAD_ID so it's just a REORDER of recently active threads"*.
/// The aggregation is the point: a hot member appended once per event would fill
/// a 32-slot window with three distinct ids and evict every cold member the
/// index exists to surface.
///
/// Three differences, each load bearing:
///
/// 1. **A touch MOVES; it does not append.** A member already held is lifted to
///    the front with its new value and the length does not grow.
/// 2. **It hydrates.** [`Self::read`] takes the producer's whole order as of an
///    instant, which is what lets a COLD subscriber receive history instead of
///    only what happens next.
/// 3. **It publishes no transitions.** A reorder is not an appended item.
///    Spelling it as one would hand every mirror a `transition` to append to a
///    list whose meaning IS its order: the consumer would accumulate moves and
///    never hold the order. The cell moves wholesale on the row instead.
///
/// # What bounds it
///
/// `capacity` only, and **there is no clock, deliberately.** "The most recently
/// updated N" must have an answer on an idle fleet; a 24-hour window would empty
/// the index after a quiet day, which is the one state where a consumer most
/// wants to know where it left off. The oldest member is evicted when the
/// `capacity + 1`-th distinct member is touched, and `covers_since` says how far
/// back the order still reaches.
#[derive(Debug, Clone)]
pub struct Recency<T> {
    /// **FRONT IS NEWEST** — the opposite of [`Sequence`]'s, and again because
    /// of what this cell exports. "Most recent first" IS the answer, so holding
    /// the members that way round means nothing has to be reversed to serve
    /// one.
    items: VecDeque<(String, Stamped<T>)>,
    capacity: usize,
    state: CellState,
}

impl<T: Serialize + Clone> Recency<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::new(),
            capacity,
            state: CellState::default(),
        }
    }

    /// **THE HYDRATE ENTRY.** One statement from the producer, holding its whole
    /// order with the MOST RECENT MEMBER FIRST. Anything past `capacity` is
    /// dropped on the way in, so there is no path by which this cell comes to
    /// hold more than it declares.
    ///
    /// `complete` is the producer's claim about its own read — `false` when the
    /// read hit its own limit — and it lands on the cell state verbatim, so a
    /// consumer counting entries knows whether it has the order or a floor of
    /// it. Note the asymmetry with [`Sequence::read`], which DOWNGRADES a
    /// `complete` claim when it had to drop items: there, dropping the front of
    /// a log makes the window demonstrably short whatever the producer thought.
    /// Here the ask was for the newest N and the newest N is what was kept, so
    /// an over-long answer is the producer being generous rather than the cell
    /// being short, and `complete` is left as it was stated.
    pub fn read(&mut self, as_of: Timestamp, entries: Vec<(String, Timestamp, T)>, complete: bool) {
        self.items = entries
            .into_iter()
            .take(self.capacity)
            .map(|(id, at, value)| (id, Stamped::new(at, value)))
            .collect();
        self.state = hydrated(as_of, complete, self.oldest_stamp());
    }

    fn oldest_stamp(&self) -> Option<Timestamp> {
        self.items.back().map(|(_, held)| held.at)
    }

    /// **A MEMBER DID SOMETHING.** `id` goes to the front carrying `value`; a
    /// member that was not held is inserted there, and if that takes the cell
    /// past `capacity` the member at the far end leaves.
    ///
    /// Under the stamp rule, like every other live entry: an input at or before
    /// `folded_at` is one the hydrate read already accounted for, so it moves
    /// nothing and answers `false`.
    ///
    /// The answer is whether anything moved, so that a fold can decide whether
    /// the row is worth a frame — a touch that changed neither the order nor the
    /// value is not news for anybody.
    pub fn touch(&mut self, at: Timestamp, id: String, value: T) -> bool {
        if already_folded(&self.state, at) {
            return false;
        }
        if let Some(held) = self.items.iter().position(|(k, _)| *k == id) {
            self.items.remove(held);
        }
        self.items.push_front((id, Stamped::new(at, value)));
        while self.items.len() > self.capacity {
            self.items.pop_back();
        }
        self.state = advanced(&self.state, at, self.oldest_stamp());
        true
    }

    /// Read a member's value WITHOUT moving it — for a fold that must look
    /// before it decides, the same reason [`Map::get`] exists.
    pub fn get(&self, id: &str) -> Option<&T> {
        self.items
            .iter()
            .find(|(k, _)| k == id)
            .map(|(_, held)| &held.value)
    }

    /// **CHANGE WHAT IS HELD ABOUT A MEMBER, WITHOUT MOVING IT.**
    ///
    /// The other half of [`Self::touch`], and the two are separate because the
    /// difference is a ruling rather than an ergonomic. `touch` says *this
    /// member did something*, which is what an order is made of. This says *what
    /// I hold about this member changed*, which is not. The standing instance is
    /// a CONSUMER'S own act: a thread marked seen changes the roll-up and must
    /// not reorder the index, because a visit is a consumer-side fact and a
    /// producer that let it move the order would be recording one.
    ///
    /// It answers `false` for a member this window does not hold, so a fold can
    /// make its decision THROUGH the call and dirty nothing when there was
    /// nothing to change; and `false` for an input the stamp rule refuses, on
    /// exactly `touch`'s terms.
    ///
    /// **THE MEMBER'S OWN `at` DOES NOT MOVE AND THE CELL'S `folded_at` DOES.**
    /// Two stamps, two questions, and this is the one method where they come
    /// apart:
    ///
    /// * the member's own stamp is its POSITION — the reason it sits where it
    ///   sits. A roll-up changing is not the member doing anything, so moving
    ///   that stamp would assert a reorder that never happened, and each exported
    ///   `id`/`at` pair would stop agreeing with the position it is printed at.
    /// * the CELL's stamp is when this cell last folded an input, and it has just
    ///   folded one. Leaving it behind would publish a changed row beside an
    ///   unmoved cell state, and a consumer holding both would conclude its row
    ///   is older than it is — the quiet disagreement every three-valued state
    ///   in this module exists to prevent.
    pub fn set(&mut self, at: Timestamp, id: &str, value: T) -> bool {
        if already_folded(&self.state, at) {
            return false;
        }
        let Some((_, held)) = self.items.iter_mut().find(|(k, _)| k == id) else {
            return false;
        };
        held.value = value;
        self.state = advanced(&self.state, at, self.oldest_stamp());
        true
    }

    /// The whole order, most recent member first.
    pub fn now(&self) -> impl Iterator<Item = (&str, &T)> {
        self.items
            .iter()
            .map(|(k, held)| (k.as_str(), &held.value))
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn state(&self) -> &CellState {
        &self.state
    }

    pub fn to_json(&self) -> Value {
        self.to_json_retained(crate::wire::Retain::Max)
    }

    /// **THE ORDER, PROJECTED TO WHAT THIS SUBSCRIBER ASKED FOR** — the newest
    /// `n`, which for this cell is simply the first `n`, because storage order
    /// is export order. `id` and `at` ride on every entry, so the order is
    /// checkable by a consumer rather than merely asserted by the producer.
    ///
    /// **It evicts nothing**, exactly as [`Sequence::to_json_retained`] does
    /// not: every other subscriber's frame is built from the same untouched
    /// items.
    pub fn to_json_retained(&self, retain: crate::wire::Retain) -> Value {
        let shown = retain.take(self.items.len());
        let rendered: Vec<Value> = self
            .items
            .iter()
            .take(shown)
            .map(|(id, held)| {
                grafted(
                    &held.value,
                    &[("id", id.clone()), ("at", held.at.to_rfc3339())],
                )
            })
            .collect();
        Value::Array(rendered)
    }

    pub fn mark_not_hydrated(&mut self, reason: impl Into<String>) {
        self.state = unreached(reason);
    }

    pub fn mark_dead(&mut self, reason: &str) {
        self.state = expired(reason);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The edge cell
// ─────────────────────────────────────────────────────────────────────────────

/// **A level with a log of its changes.** The level hydrates; the transitions
/// never do, and the export says `since_init` so that nobody counts them as
/// history.
#[derive(Debug, Clone)]
pub struct Edge<L: PartialEq + Clone + Serialize> {
    level: Option<L>,
    transitions: Sequence<Transition<L>>,
    state: CellState,
    dirty: bool,
}

/// One change of level: what it was, and what it became.
#[derive(Debug, Clone, Serialize)]
pub struct Transition<L> {
    pub from: Option<L>,
    pub to: L,
}

impl<L: PartialEq + Clone + Serialize> Edge<L> {
    pub fn new(capacity: usize, window: ChronoDuration) -> Self {
        Self {
            level: None,
            transitions: Sequence::new(capacity, window),
            state: CellState::default(),
            dirty: false,
        }
    }

    /// Set the level. A transition is recorded only when all three hold:
    ///
    /// 1. the level actually CHANGED — a re-assertion of the same level is a
    ///    re-assertion, not an event;
    /// 2. a level was already held — the first level a shape ever learns is not
    ///    a change from anything, and `from: null` to mean "it began" would be
    ///    a transition every cold subscriber would have to learn to ignore;
    /// 3. the stamp is strictly after `folded_at` — the hydration edge. An
    ///    input that was in flight when the read ran cannot have changed a level
    ///    the read already observed.
    ///
    /// The level is set in every case, including the refused ones. A refused
    /// input is refused as EVIDENCE OF CHANGE, never as evidence of the value.
    pub fn emit(&mut self, at: Timestamp, level: L) {
        let behind_edge = already_folded(&self.state, at);
        let changed = self.level.as_ref() != Some(&level);
        if changed && self.level.is_some() && !behind_edge {
            self.transitions.emit(
                at,
                Transition {
                    from: self.level.clone(),
                    to: level.clone(),
                },
            );
        }
        self.level = Some(level);
        self.state = CellState::known(at);
        self.dirty = true;
    }

    /// Hydrate the LEVEL. The transition log is marked `since_init` rather than
    /// filled: a producer can say what the level is as of an instant, and the
    /// changes behind it are not something a read can hand over honestly.
    pub fn read(&mut self, as_of: Timestamp, level: L) {
        self.level = Some(level);
        self.state = CellState::known(as_of);
        self.transitions.mark_since_init(as_of);
        self.dirty = true;
    }

    /// **The hydrate did not reach this cell** — the three-valued state's middle
    /// value, which this cell could not express until a shape needed it.
    ///
    /// [`Register`] has had it since the first shape. An `Edge` had only `read`
    /// and `mark_dead`, so a shape whose level read FAILED had to choose between
    /// `Known` with no value, which is indistinguishable from a real level, and
    /// `Dead`, which claims the whole task died. Both are lies, about different
    /// things. It arrived with this cell's first instance, `threads`' status,
    /// whose derivation can fail on a store error.
    ///
    /// The log of changes goes the same way, because transitions under a level
    /// nobody could read are not transitions anybody should believe.
    pub fn not_hydrated(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.transitions.set_not_hydrated(reason.clone());
        self.state = unreached(reason);
        self.dirty = true;
    }

    pub fn now(&self) -> Option<&L> {
        self.level.as_ref()
    }

    pub fn transitions(&self) -> &Sequence<Transition<L>> {
        &self.transitions
    }

    pub fn transitions_mut(&mut self) -> &mut Sequence<Transition<L>> {
        &mut self.transitions
    }

    pub fn state(&self) -> &CellState {
        &self.state
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// The fold is gone, and the log of changes goes with the level — the log is
    /// the level's own history and cannot be current when the level is not.
    pub fn mark_dead(&mut self, reason: &str) {
        self.state = expired(reason);
        self.transitions.mark_dead(reason);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// What a fold produced, and what a shape is
// ─────────────────────────────────────────────────────────────────────────────

/// **What one input's fold produced, after coalescing.**
///
/// Five kinds of consequence. Many cell writes under one key coalesce into one
/// `row`, which loses nothing because a row is an idempotent upsert; `added`
/// and `gone` are the membership pair; `transitions` are the appends that cannot
/// be coalesced because each one is its own occurrence; `shared` is one bit
/// because the shape-level block moves wholesale or not at all.
#[derive(Debug, Default)]
pub struct Outbox {
    pub rows: Vec<String>,
    pub gone: Vec<String>,
    /// **KEYS THAT JOINED THE POOL** — the twin of `gone`. A key minted as a row
    /// is here too, because holding implies membership; a key a `PerKey` fold
    /// merely recorded the existence of is here and NOT in `rows`, and that gap
    /// is the whole of the mechanism.
    pub added: Vec<String>,
    pub transitions: Vec<(String, &'static str, Value)>,
    pub shared: bool,
}

impl Outbox {
    /// How many consequences this fold had. The `shared` bit counts as one,
    /// because a drain that produced only a shape-level change still produced
    /// something and an outbox that called itself empty would have it dropped.
    pub fn len(&self) -> usize {
        self.rows.len()
            + self.gone.len()
            + self.added.len()
            + self.transitions.len()
            + usize::from(self.shared)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// **WHAT A ROOT SAYS ABOUT ITS POOL'S MEMBERSHIP**, before the chassis pages it
/// into `MembersPage` frames.
///
/// Two values and not three. `Empty` is deliberately absent: an empty pool is
/// `Known { keys: [], complete: true }`, a COMPLETE answer of zero, exactly as a
/// complete hydration of no rows is. The statement that is genuinely different
/// is *"I cannot enumerate"*, and that is the second value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Membership {
    Known { keys: Vec<String>, complete: bool },
    NotHydrated { reason: String },
}

/// **EVERYTHING ONE SHAPE HOLDS.**
///
/// The design called for `type Root: Cell` — a map of key to row for a keyed
/// shape — and that does not survive contact with ruling 6: three of the fields
/// the board draws are SHAPE-LEVEL rather than per-key, and a bare map has
/// nowhere to put them. So a root is a TRAIT over whatever a shape holds —
/// typically a struct carrying the keyed [`Map`] beside its shape-level cells —
/// and [`Self::shared`] is where the latter come out. That block rides on every
/// export and moves as its own `shared` event.
pub trait Root: Send + 'static {
    /// Keys currently HELD.
    fn keys(&self) -> Vec<String>;

    /// **EVERY KEY THE POOL CONTAINS**, in key order. It is what a
    /// `detail: keys` subscribe answers with, and what `added` and `gone` move
    /// between frames.
    ///
    /// **The default is the held keys, complete.** That is exactly right for a
    /// `Whole` root, which reads its key set at init and holds everything it
    /// knows about, so *what exists* and *what I hold* are one answer — and it
    /// is why no shape written before this method existed changes behaviour by
    /// its arrival. A `PerKey` root overrides it with its [`Map::members`] set,
    /// because there the two answers come apart.
    ///
    /// A producer that genuinely cannot enumerate its own namespace answers
    /// [`Membership::NotHydrated`] and says why. No flag is involved: the
    /// three-valued state already carries that statement.
    fn members(&self) -> Membership {
        Membership::Known {
            keys: self.keys(),
            complete: true,
        }
    }

    /// One key's whole row, as both a `row` event and an export carry it.
    fn row(&self, key: &str) -> Option<Value>;

    /// **ONE SUBSCRIBER'S ROW, WITH ITS OWN `retain` APPLIED TO EVERY WINDOWED
    /// CELL UNDERNEATH.**
    ///
    /// **The default ignores `retain` and answers [`Self::row`]**, and that is
    /// not the stub-pretending-to-work it reads like, because a second default
    /// it cannot disagree with stands behind it: a shape's declared ceiling on
    /// retained items is `0` unless its author says otherwise, and a `0` ceiling
    /// refuses every `Retain::Count` at subscribe time. So a root that has not
    /// overridden this can only ever be reached with `Retain::Max`, where the
    /// whole row IS the right answer rather than an omission.
    ///
    /// **THE CEILING AND THIS METHOD MOVE TOGETHER OR THE SHAPE IS WRONG**, and
    /// one direction is worse than the other: declare the ceiling, leave this
    /// method alone, and every `retain` a subscriber sends is accepted and then
    /// ignored in silence. No compile check can catch it, because this trait
    /// exposes no cell types at all — the same reason nothing can check that a
    /// counter forced eager tracking — so the pairing is owed by whoever writes
    /// the shape, and proved by whoever tests it.
    fn row_at(&self, key: &str, retain: crate::wire::Retain) -> Option<Value> {
        let _ = retain;
        self.row(key)
    }

    /// One key's provenance: a [`CellState`] per cell under it.
    fn cells(&self, key: &str) -> Option<Value>;

    /// The values that belong to the shape rather than to any one key.
    fn shared(&self) -> Value;

    /// Provenance for those same shape-level values.
    fn shared_cells(&self) -> Value;

    /// **Collect everything the fold emitted since the last drain, AND CLEAR
    /// IT.** The clearing is half the contract: a drain that collected without
    /// clearing would re-publish the same row on every input for the life of the
    /// shape.
    fn drain(&mut self) -> Outbox;

    /// The task died: every cell this root owns becomes [`CellState::Dead`].
    fn mark_dead(&mut self, reason: &str);
}

#[cfg(test)]
mod tests;
