//! # `mirror` — the chassis facing the other way
//!
//! One crate, one chassis, two directions. `hub` (behind the `hub` flag) is the
//! producing half: it stamps inputs, folds them in arrival order and emits
//! frames. This module is what RECEIVES those frames, and it is deliberately
//! not a client library — the operator's ruling on substrate's design, carried
//! into this tree with it, was that *"the chassis is capable of producing and
//! consuming… It's bidirectional. And it handles all interop between processes
//! in the cauldron."* A process that both serves a shape and reads somebody
//! else's links one crate, not two.
//!
//! The contract is `docs/shapes.md` 6.5 in substrate-interface-layer. The only
//! implementation that predates the Rust one is `threads_tui/shapes.py`'s
//! `ShapeMirror` — 427 lines of Python over a single shape — and substrate used
//! it as an ORACLE to differ against rather than as something to port. Every
//! place the two disagree is written down at the site, with which one is right
//! and why; those notes are kept here because the argument is the evidence for
//! the behaviour, and the Python file is not in this tree to be re-read.
//!
//! ## The delivery contract, which is one sentence
//!
//! > **Hold the export. Apply everything after it.**
//!
//! There is no version number on a frame, no serial, no cursor and no replay
//! (`docs/shapes.md` 2.5). That works because the ordering is structural rather
//! than negotiated: one producer task emits a shape's exports and its derived
//! events, and one receiver per connection hands them over in the order they
//! arrived. Nothing here has to reconcile two streams, so nothing here has a
//! sequence number to reconcile them with.
//!
//! ## What is NOT in here
//!
//! **No socket, no connection, no runtime, and no clock.** [`Mirror::apply`]
//! takes a frame and answers what receiving it did. Every stamp this module
//! holds arrived ON a frame; *"how stale is this"* is a question with a `now`
//! in it, and `now` belongs to whoever is rendering. That purity is why a whole
//! connection's life — hydrate, burst, a key joining, a key leaving, the task
//! dying — can be driven through it with no daemon and nothing sleeping, and it
//! is the evidence behind this module sitting in the unflagged floor rather
//! than behind `hub`.
//!
//! **It does choose a transport, and that does not contradict the paragraph
//! above** (`docs/shapes.md` 2.9; operator ruling 2026-10-02). Which door a
//! resource's bytes come through is a decision about SIZE, and the size is
//! already on a mirrored row — so the decision is a pure function of state in
//! hand and belongs beside the state. CARRYING IT OUT needs the socket, which
//! belongs to the caller. So this module stores no door, has no field of a
//! transport type, and answers a PLAN: [`Mirror::plan`] is the decision and
//! [`Mirror::read`] is the one call an application makes.
//!
//! ## What a connection owes this module — exactly seven things
//!
//! The count and the list are asserted against each other by
//! `transport_tests::the_seam_list_matches_the_count_above_it`, because this
//! sentence said *five* over a six-item list in substrate for as long as the
//! list had six items.
//!
//! 1. Hand it every `ShapeWire` that arrives, in arrival order —
//!    [`Mirror::accept`] for JSON off the wire, [`Mirror::apply`] when it is
//!    already typed.
//! 2. Call [`Mirror::reset`] the moment the socket dies, before the first frame
//!    of its replacement. A mirror is only ever valid for the connection that
//!    was exported to it.
//! 3. Hand it each `ShapeInfo` off `shapes.list` through [`Mirror::learn`], so
//!    that rule 2.8's ceilings, rule 2.6's version contract and the cell
//!    manifest are known BEFORE anything is asked for.
//! 4. Read [`Mirror::ack`] when it wants to tell the producer what receipt did,
//!    and mint the serial itself — [`Mirror::received`] says why the count this
//!    module keeps is a placeholder.
//! 5. Call [`Mirror::refused`] or [`Mirror::refused_version`] when the LINK is
//!    refused. A version mismatch at handshake, or an unknown or dead shape on
//!    a subscribe, reaches a consumer as an `envelope::Outcome` and never as a
//!    frame — so this module cannot see one, and without being told it would
//!    sit healthy for ever.
//! 6. Decide recovery. This module reports: [`Mirror::dead`],
//!    [`SubscribeStatus::Short`], [`Roster::resume_from`]. It never
//!    re-subscribes, because it holds nothing to re-subscribe ON.
//! 7. Execute a [`Fetch`] — or hand it a [`ResourceDoor`] and let
//!    [`Mirror::read`] do it. The choice of door is this module's (see
//!    [`Mirror::plan`]); the socket is not.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::envelope::Refusal;
use crate::wire::{
    reject, Ack, CellInfo, CellState, Derived, Epoch, Link, MembersPage, Op, PageContract,
    Rejected, ResourceSummary, ShapeContract, ShapeInfo, ShapeMax, ShapeState, ShapeWire,
    Superseded, Timestamp, Transport,
};

// ═════════════════════════════════════════════════════════════════════════════
// 1. The vocabulary of absence
//
// Four types in this section and all four exist for one reason: an absence is
// not a statement, and every place this chassis could have collapsed "nobody
// told me" into "there is nothing" it refuses to. `CellState` does it for a
// cell, `Enumeration` for a pool, `KeyState` for one key's existence and
// `Declaration` for whether a name is a cell at all.
// ═════════════════════════════════════════════════════════════════════════════

/// The reason a cell that no export has carried gives for being unhydrated.
///
/// Not a guess and not an error: it is `CellState::NotHydrated` with the cause
/// named, which is the whole point of the three-valued unknown.
pub const UNSEEN: &str = "no export has carried this cell";

/// What a dead shape says when the producer named no reason of its own.
pub const DIED: &str = "the shape task died";

/// **WHICH KEYS DOES THE PRODUCER SAY ARE IN THIS POOL?** The `detail: keys`
/// answer, kept strictly apart from what this connection HOLDS.
///
/// Three values, for the reason every read in this crate has three: *nobody has
/// told me*, *here they are* and *I cannot enumerate this namespace* are three
/// different sentences, and an empty list is none of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enumeration {
    /// No membership frame has ever arrived. Nothing at all is claimed.
    NotAsked,
    /// A walk arrived. `complete` is the claim the LAST page made.
    Known { complete: bool },
    /// The producer says this namespace cannot be enumerated.
    NotHydrated,
}

/// **WHAT IS KNOWN ABOUT ONE KEY'S EXISTENCE** — four values, and the two that
/// are neither *held* nor *unknown* are the reason this type exists.
///
/// Before it, `Derived::Gone` dropped the row and that was all; afterwards
/// [`Mirror::cell`] answered `NotHydrated { reason: UNSEEN }` — *nobody has told
/// me* — about a key the producer had just explicitly unsaid. That is this
/// repo's house defect (the fourth clause of `docs/shapes.md` 2.8,
/// `CellState`'s three values, [`Enumeration`]'s three, `MembersPage`'s three)
/// showing up one level down, and it bites hardest exactly where it was found:
/// across a re-init, where a key disappearing is the only way a consumer ever
/// learns the row it is drawing belongs to nobody. A board that cannot tell
/// *removed* from *not loaded yet* either shows a stale row for ever or blanks
/// one that is merely slow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyState {
    /// A row is held for this key — an export placed it, or a `row` event did.
    Held,
    /// **THE PRODUCER SAID THIS KEY IS GONE.** A positive fact with a frame
    /// behind it, not an absence. The row has been dropped and this is the
    /// record of WHY it was dropped.
    Gone,
    /// Nothing has been said about this key in either direction. Note that this
    /// is not *absent from the pool* — [`Mirror::members`] answers that, and it
    /// has its own three values.
    Unknown,
    /// **THE PRODUCER ANSWERED THIS KEY WITH NO ROW.** A keyed export carrying
    /// neither `row` nor `cells` is the producer's one spelling for *I do not
    /// hold this key*; substrate's `hub::export` emits exactly that frame for a
    /// key its root does not hold, and `wire::Derived::Added`'s doc says so.
    ///
    /// A positive fact like [`Self::Gone`], and deliberately NOT `Gone`: a key
    /// that was never held did not leave. Before this variant existed the
    /// mirror folded such an export into a bare row and answered `Held` — *I
    /// hold a row for this key* — while every cell beneath it answered *no
    /// export has carried this cell*, for the life of the connection. That
    /// contradiction was LOCKED into substrate's seam kit as
    /// `conformance/absent_key.jsonl` rather than fixed; this variant is the
    /// fix, and re-blessing that transcript was the record of it.
    Absent,
}

/// **DOES THE PRODUCER DECLARE A CELL BY THIS NAME AT ALL?** — read off the
/// manifest `shapes.list` publishes (`wire::ShapeInfo::cells`).
///
/// `CellState` is honest about DELIVERY; this is honest about EXISTENCE. The
/// hole it closes: `NotHydrated { reason: UNSEEN }` was the answer both for a
/// declared cell that had not arrived and for a cell no producer will ever
/// emit — *not yet* and *never*, one spelling — and substrate's `threads` face
/// read a `pending` cell on that answer for months. A reader asks this first,
/// exactly as it asks [`Mirror::key_state`] before believing a row-level
/// `NotHydrated`.
///
/// Three values rather than two, because *the producer says there is no such
/// cell* and *the producer has not told me its cells* are different facts and
/// collapsing them would rebuild the same hole one storey up. A mirror that has
/// learned no manifest answers [`Self::Unstated`] and judges nothing, on the
/// same grounds that [`Mirror::check`] refuses nothing without a `ShapeMax`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declaration {
    /// Declared, and here is what the producer says it carries.
    Declared(CellInfo),
    /// **THE PRODUCER LISTS ITS CELLS AND THIS IS NOT ONE.** No export will
    /// ever carry it, so a read of it is a defect in the READER, and
    /// `declares` is the list to correct that read against.
    Undeclared { declares: Vec<String> },
    /// No manifest has been learned, so nothing can be said either way.
    Unstated,
}

/// Which of the two cell namespaces a name is being asked about. A `bool`
/// parameter would read as `self.absence(name, true)` at the call site, which
/// says nothing to anyone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    /// `cells` — per key.
    Key,
    /// `shared_cells` — per shape.
    Shape,
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. Reading a `cells` block, by hand and not by serde
// ═════════════════════════════════════════════════════════════════════════════

// **WHY `serde_json::from_value::<CellState>` IS NOT USED HERE.**
//
// `CellState` is `#[serde(tag = "state")]` over a closed enum, so a state word
// this build has never heard of is a hard deserialization failure — and the
// failure would take the whole frame with it. Rule 2.6 obliges a consumer to
// IGNORE what it does not understand, and the rule's own soft spot 2 concedes
// it has no detector for a semantic change it could not refuse at link time.
// `reject::UNKNOWN_CELL_STATE` is that detector, and it only exists if the
// unknown word is caught HERE instead of throwing the frame away.
//
// An unrecognised word is never read as `Known`. A board that guessed `Known`
// would be showing a confident number it has no right to.

/// The `known` arm, whose four fields all have a defined answer when the
/// producer omits them. `folded_at` falls back to the epoch rather than to
/// *now*, because this module reads no clock.
fn known_cell(obj: &serde_json::Map<String, Value>) -> CellState {
    let flag = |k: &str, default: bool| obj.get(k).and_then(Value::as_bool).unwrap_or(default);
    let stamp = |k: &str| obj.get(k).and_then(Value::as_str).and_then(parse_ts);
    CellState::Known {
        complete: flag("complete", true),
        folded_at: stamp("folded_at").unwrap_or(Timestamp::UNIX_EPOCH),
        since_init: flag("since_init", false),
        covers_since: stamp("covers_since"),
    }
}

/// One `cells.<name>` block. A word this build does not know is pushed onto
/// `unknown` and the cell is HELD as unhydrated with the word quoted, so the
/// hole is legible on a screen as well as countable in an ack.
///
/// A block that is not an object at all, and a block with no `state` key, are
/// both the empty word: there is nothing to quote, and inventing a name for the
/// malformation would be describing it rather than reporting it.
fn read_cell(block: &Value, unknown: &mut Vec<String>) -> CellState {
    let word = match block.as_object() {
        None => String::new(),
        Some(obj) => match obj.get("state").and_then(Value::as_str) {
            Some("known") => return known_cell(obj),
            Some("not_hydrated") => {
                return CellState::NotHydrated {
                    reason: obj.get("reason").and_then(Value::as_str).map(str::to_string),
                }
            }
            Some("dead") => {
                return CellState::Dead {
                    reason: obj
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or(DIED)
                        .to_string(),
                }
            }
            Some(other) => other.to_string(),
            None => String::new(),
        },
    };
    let held = CellState::NotHydrated {
        reason: Some(format!("unrecognised cell state {word:?}")),
    };
    unknown.push(word);
    held
}

/// Every block of one `cells` or `shared_cells` object, plus the words that
/// were not understood, in the object's own key order.
fn read_cells(raw: Option<&Value>) -> (BTreeMap<String, CellState>, Vec<String>) {
    let mut states = BTreeMap::new();
    let mut unknown = Vec::new();
    if let Some(obj) = raw.and_then(Value::as_object) {
        for (name, block) in obj {
            states.insert(name.clone(), read_cell(block, &mut unknown));
        }
    }
    (states, unknown)
}

fn parse_ts(s: &str) -> Option<Timestamp> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. One key, held
// ═════════════════════════════════════════════════════════════════════════════

/// What this connection holds for a single key: the row as the producer stated
/// it, the provenance of each cell, the items appended on top of the export,
/// and the two stamps that say when each of those happened.
#[derive(Debug, Clone, PartialEq)]
pub struct Held {
    key: String,
    row: Value,
    cells: BTreeMap<String, CellState>,
    /// **APPENDED ITEMS, KEPT OUT OF THE ROW.** The one place this module
    /// deliberately refuses to do what substrate's `shapes.py` oracle does.
    ///
    /// The Python mirror appends the item to `row[cell]`, where `cell` is
    /// `Derived::Transition::cell`. For two of the four shapes in substrate's
    /// workspace that is right. For the third it destroys data: `threads`'
    /// `Outbox` names the cell `"status"` while the ROW spells the level as
    /// `status` and the log as `status_transitions`, so the Python rule
    /// replaces a held level OBJECT with a one-item LIST on the first
    /// transition frame. `docs/shape-surface.md` 8, finding 15, verified it; it
    /// is latent in Python only because that mirror covers `accounts` alone.
    ///
    /// **The chassis contract is silent on how a transition's cell name maps
    /// onto a row field, and the mapping is each shape's own choice**
    /// (`wire::Derived`'s doc). A chassis that does not know the mapping must
    /// not invent one. So appended items live in their own store, a transition
    /// never writes the row, and [`Self::items`] merges the two for a reader —
    /// merging only where the row field is absent or is already a list, which
    /// is precisely the case in which the Python rule was correct.
    transitions: BTreeMap<String, Vec<Value>>,
    as_of: Option<Timestamp>,
    at: Option<Timestamp>,
    dead_reason: Option<String>,
}

impl Held {
    /// A row-shaped hole: the key, an empty object, and no provenance for
    /// anything. What a `row` event for a never-exported key gets.
    fn empty(key: String) -> Self {
        Self {
            key,
            row: Value::Object(Default::default()),
            cells: BTreeMap::new(),
            transitions: BTreeMap::new(),
            as_of: None,
            at: None,
            dead_reason: None,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// The row exactly as the producer last stated it. An export replaced it
    /// wholesale; a live `row` event replaced it again. **Never appended to.**
    pub fn row(&self) -> &Value {
        &self.row
    }

    /// One field off the row, or `Value::Null` when the row has no such field.
    pub fn field(&self, name: &str) -> &Value {
        self.row.get(name).unwrap_or(&Value::Null)
    }

    /// The export's REAL fold point. Never the `at` that a subscribe asked for.
    pub fn as_of(&self) -> Option<Timestamp> {
        self.as_of
    }

    /// The stamp on the newest frame applied on top of the export.
    pub fn at(&self) -> Option<Timestamp> {
        self.at
    }

    /// **A DEAD ROW ANSWERS `Dead` FOR EVERY CELL NAME, INCLUDING ONES NO
    /// EXPORT EVER CARRIED.** A fold that died did not leave some of its
    /// fields trustworthy, so there is no name under it that can answer
    /// anything else.
    pub fn cell(&self, name: &str) -> CellState {
        match &self.dead_reason {
            Some(reason) => CellState::Dead {
                reason: reason.clone(),
            },
            None => self
                .cells
                .get(name)
                .cloned()
                .unwrap_or_else(|| CellState::NotHydrated {
                    reason: Some(UNSEEN.to_string()),
                }),
        }
    }

    pub fn cells(&self) -> &BTreeMap<String, CellState> {
        &self.cells
    }

    /// The items this cell has had appended SINCE the export, in arrival order.
    pub fn transitions(&self, cell: &str) -> &[Value] {
        self.transitions.get(cell).map_or(&[], Vec::as_slice)
    }

    /// Every cell name a transition frame has named for this key, in name
    /// order.
    pub fn transitioned(&self) -> Vec<String> {
        self.transitions.keys().cloned().collect()
    }

    /// Set by the death notice. Held at the ROW level rather than written into
    /// each cell, so that a cell no export carried is dead too — see
    /// [`Self::cell`].
    pub fn dead_reason(&self) -> Option<&str> {
        self.dead_reason.as_deref()
    }

    /// **THE MERGED VIEW OF AN EDGE-SHAPED CELL**: what the export retained,
    /// then what has arrived since.
    ///
    /// The row field contributes only when it is absent or is already a list.
    /// Where a shape spells the level and the log apart — `threads` does — the
    /// level stays in [`Self::row`] untouched and this answers the appended
    /// items alone. A consumer that wants its own shape's merge writes it,
    /// knowing its own spelling; the chassis does not guess at one.
    pub fn items(&self, cell: &str) -> Vec<Value> {
        let mut merged = match self.row.get(cell) {
            Some(Value::Array(exported)) => exported.clone(),
            _ => Vec::new(),
        };
        merged.extend(self.transitions(cell).iter().cloned());
        merged
    }

    fn mark_dead(&mut self, reason: &str) {
        self.dead_reason = Some(reason.to_string());
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 4. The pool's membership
// ═════════════════════════════════════════════════════════════════════════════

/// The keys the producer says are in the pool, and what it claims about how
/// complete that answer is.
///
/// **PAGES UNION; THEY DO NOT REPLACE.** A membership walk leaves the producer
/// as N frames in one burst and only the last of them can claim `complete`, so
/// a page that replaced would leave a consumer holding page N and nothing else.
/// Arrivals come as `Derived::Added` and removals as `Derived::Gone`, which
/// keeps the union correct for as long as the connection lives;
/// [`Mirror::reset`] is what a reconnect uses, and re-enumerating a live
/// connection is idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Roster {
    keys: BTreeSet<String>,
    state: Option<Enumeration>,
    reason: Option<String>,
    next: Option<String>,
}

impl Roster {
    pub fn enumeration(&self) -> Enumeration {
        self.state.unwrap_or(Enumeration::NotAsked)
    }

    /// Every key the producer has named, in key order.
    pub fn keys(&self) -> Vec<String> {
        self.keys.iter().cloned().collect()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    /// The producer's own reason for being unable to enumerate, when it gave
    /// one.
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// **WHERE THE HOLE STARTS** — the first key of the page after the last one
    /// that arrived, while the walk is unfinished. The chassis pages on its
    /// own, so this is a diagnostic and never a cursor an application drives.
    pub fn resume_from(&self) -> Option<&str> {
        self.next.as_deref()
    }

    /// The producer has stated the WHOLE membership of this pool.
    pub fn complete(&self) -> bool {
        matches!(self.state, Some(Enumeration::Known { complete: true }))
    }

    /// Fold one page in. Answers whether a reader can see any difference.
    fn page(&mut self, page: &MembersPage) -> bool {
        match page {
            MembersPage::Known {
                keys,
                complete,
                next,
            } => {
                let claim = Enumeration::Known {
                    complete: *complete,
                };
                let was = self.keys.len();
                self.keys.extend(keys.iter().cloned());
                let moved = self.keys.len() != was || self.state != Some(claim);
                self.state = Some(claim);
                self.reason = None;
                // A finished walk has no hole left to resume from.
                self.next = if *complete { None } else { next.clone() };
                moved
            }
            MembersPage::NotHydrated { reason } => {
                // **THE KEYS SURVIVE.** *"I cannot enumerate"* does not unsay
                // an `added` the producer has already sent; it says the WALK is
                // unavailable, which is what `state` now records.
                let moved = self.state != Some(Enumeration::NotHydrated)
                    || self.reason.as_deref() != Some(reason.as_str());
                self.state = Some(Enumeration::NotHydrated);
                self.reason = Some(reason.clone());
                self.next = None;
                moved
            }
        }
    }

    /// One key arriving. Into a roster nobody has enumerated this produces a
    /// roster that claims nothing about completeness — *"this key exists"* is
    /// not *"and these are all of them"*.
    fn added(&mut self, key: &str) -> bool {
        let moved = self.keys.insert(key.to_string());
        self.state
            .get_or_insert(Enumeration::Known { complete: false });
        moved
    }

    fn gone(&mut self, key: &str) -> bool {
        self.keys.remove(key)
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 5. What receiving one frame did
// ═════════════════════════════════════════════════════════════════════════════

/// **RECEIVED, APPLIED, REJECTED** — rule 2.3's three words, kept as three
/// values instead of the `bool` that substrate's Python oracle collapses them
/// into.
///
/// `shapes.py::on_frame` knows the difference between a frame applied, a frame
/// belonging to another shape, an unknown `op`, a `live` carrying no event, a
/// transition for a key never exported, an unknown event `kind` and an
/// unrecognised cell-state word — and then returns `True` or `False`, spending
/// every one of those distinctions at the door. `wire::Rejected` exists so they
/// survive the door, and its own doc says *"a consumer chassis is where
/// `rejected` gets FILLED IN"*. This is where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// Understood in full, and the effect is in the mirror.
    Applied,
    /// Applied, and PART of it was a word this build does not know. The part
    /// that was not understood is in [`Receipt::rejected`] and is held as
    /// unhydrated rather than guessed at. The only instance today is a cell
    /// whose `state` word is unrecognised.
    AppliedInPart,
    /// NOT applied. [`Receipt::rejected`] says why.
    NotApplied,
}

/// What receiving one frame did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Receipt {
    disposition: Option<Disposition>,
    /// Whether anything a reader can see changed. Not the same as applied: a
    /// `gone` for a key this connection never held is applied and may move
    /// nothing.
    pub moved: bool,
    /// The seam facts this frame carried, each with a code out of
    /// `reject::ALL`.
    pub rejected: Vec<Rejected>,
    /// **PROMISE IDS THIS FRAME RESOLVED THAT WERE NOT RESOLVED ALREADY.** An
    /// export re-offers ids a lost frame already carried, so resolving one
    /// twice is the ordinary case and this list is empty the second time.
    pub promises: Vec<u64>,
    /// Subscribe ids this frame answers — rule 2.7's `exported` stage.
    pub subscribes: Vec<u64>,
    /// `ShapeWire::complete`, passed through. `Some(true)` is *the read you
    /// asked for has all arrived*; `Some(false)` is rule 2.8's stated short
    /// answer; `None` is a frame that is not the last of a subscribe.
    pub complete: Option<bool>,
    /// `ShapeWire::next`, passed through: on the last frame of a short keyless
    /// answer, the first key the page did not carry — which is the consumer's
    /// `from` for the next ask. See `wire::Paging`.
    pub next: Option<String>,
}

impl Receipt {
    /// Applied unless something said otherwise, and applied-in-part whenever a
    /// seam fact rode along. Derived rather than stored so that the two can
    /// never disagree.
    pub fn disposition(&self) -> Disposition {
        self.disposition.unwrap_or(if self.rejected.is_empty() {
            Disposition::Applied
        } else {
            Disposition::AppliedInPart
        })
    }

    /// The frame's effect is in the mirror, whole or in part.
    pub fn applied(&self) -> bool {
        self.disposition() != Disposition::NotApplied
    }

    /// One frame received and not applied, carrying one coded reason.
    fn rejecting(serial: u64, reason: &str, detail: Option<String>) -> Self {
        Self {
            disposition: Some(Disposition::NotApplied),
            rejected: vec![Rejected {
                serial,
                reason: reason.to_string(),
                detail,
            }],
            ..Self::default()
        }
    }

    /// Nothing happened that a reader can see, and nothing went wrong.
    fn still(moved: bool) -> Self {
        Self {
            moved,
            ..Self::default()
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 6. Rule 2.8 — the published ceilings, read before anything is asked for
// ═════════════════════════════════════════════════════════════════════════════

/// What a subscriber is about to ask for. Every field is optional, because
/// rule 2.8's *"asking for nothing means the ceiling"* has to be sayable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ask {
    pub rows: Option<usize>,
    pub keys_per_frame: Option<usize>,
    pub future_at_secs: Option<i64>,
    pub armed_timers: Option<usize>,
    /// A `page` ask, judged against `rows_per_subscribe` — the consumer's end
    /// of the paging chain, rule 2.8. See `wire::Paging`.
    pub page: Option<usize>,
}

/// An ask beyond a published ceiling, NAMING the ceiling. Never clamped: the
/// discipline rule 2.8 asks for is that a refusal reports a number a human can
/// act on rather than the word *mismatch*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverCeiling {
    Rows { asked: usize, ceiling: usize },
    KeysPerFrame { asked: usize, ceiling: usize },
    FutureAt { asked: i64, ceiling: i64 },
    ArmedTimers { asked: usize, ceiling: usize },
    Page { asked: usize, ceiling: usize },
}

impl std::fmt::Display for OverCeiling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (what, asked, ceiling) = match self {
            OverCeiling::Rows { asked, ceiling } => ("rows", asked.to_string(), *ceiling),
            OverCeiling::KeysPerFrame { asked, ceiling } => {
                ("keys per frame", asked.to_string(), *ceiling)
            }
            OverCeiling::ArmedTimers { asked, ceiling } => {
                ("armed timers", asked.to_string(), *ceiling)
            }
            OverCeiling::Page { asked, ceiling } => {
                ("rows in one page", asked.to_string(), *ceiling)
            }
            // Seconds carry a unit and an `i64`, so this arm writes its own
            // sentence rather than being bent into the shape of the other four.
            OverCeiling::FutureAt { asked, ceiling } => {
                return write!(
                    f,
                    "asked for an `at` {asked}s ahead; the ceiling is {ceiling}s"
                )
            }
        };
        write!(f, "asked for {asked} {what}; the ceiling is {ceiling}")
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 7. Subscribes, eras, and a link the producer will not serve
// ═════════════════════════════════════════════════════════════════════════════

/// Where one `shapes.subscribe` stands — rule 2.7's stages, seen from the
/// consuming side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscribeStatus {
    /// No frame has named this id. Either it has not been asked, or its answer
    /// has not begun to arrive.
    Unknown,
    /// Frames naming this id have arrived and none has claimed the answer is
    /// whole.
    Open,
    /// The whole answer arrived — `complete: true` on its last frame.
    Complete,
    /// **THE PRODUCER'S CEILING CUT THE ANSWER SHORT**, and it said so
    /// structurally rather than by stopping: `complete: false`, rule 2.8's
    /// fourth clause. Not a failure, and not a thing to retry blindly.
    Short,
}

/// Whether a child's state and a parent's state were built under the same
/// parent era. The one comparison `Epoch::parent_epoch` exists to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    /// This shape has no parent, so the question does not arise.
    NoParent,
    /// Built under the parent's current era. A consumer may read the parent's
    /// summary and the child's rows together.
    Same,
    /// **THE PARENT HAS RE-INITED SINCE THIS CHILD LAST DID.** The child is
    /// still valid on its own terms — that is the nesting ruling — but anything
    /// the consumer inferred FROM the parent ABOUT this child is from before.
    /// The repair is to re-read the PARENT, never the child.
    ParentMovedOn { child_saw: u64, parent_now: u64 },
}

/// **A LINK THIS PRODUCER WILL NOT SERVE, FROM THE CONSUMING SIDE** — rule
/// 2.6's refused state, typed so that a consumer reports numbers instead of the
/// word *mismatch*.
///
/// Built to the same pattern as [`OverCeiling`] and for the same reason: a
/// refusal that names WHAT THE PRODUCER WILL SERVE lets the consumer say
/// something actionable. *"`accounts` refused my subscribe"* sends a human to
/// read logs. *"I am a v2 build of `accounts` and this daemon serves 4 and 3"*
/// sends them to a deployment.
///
/// It renders the served list through the SAME function the producer's own
/// refusal renders it with (`wire::ShapeError::VersionRefused`'s `Display`), so
/// the consumer's report and the producer's refusal cannot drift into
/// describing one list two ways.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionMismatch {
    /// The shape whose link was refused.
    pub shape: String,
    /// The version this consumer was built against and stated.
    pub wanted: u32,
    /// Every version the producer serves, current first.
    pub serves: Vec<u32>,
}

impl VersionMismatch {
    /// Read a refusal off a `Link`, which is how a consumer that asked
    /// [`Mirror::link`] FIRST reaches this type — rule 2.8's *read the contract
    /// before you use it*, applied to rule 2.6. The other path is being
    /// refused and converting the producer's error. Both must read the same, or
    /// one contract would have two spellings depending on how the consumer
    /// found out about it.
    pub fn from_link(shape: &str, link: &Link) -> Option<Self> {
        match link {
            Link::Refused { wanted, serves } => Some(Self {
                shape: shape.to_string(),
                wanted: *wanted,
                serves: serves.clone(),
            }),
            _ => None,
        }
    }
}

impl std::fmt::Display for VersionMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Delegated on purpose, so the crate holds exactly ONE sentence for
        // this fact. A second hand-written copy here is how a producer's
        // refusal and a consumer's report begin describing the same list
        // differently.
        crate::wire::ShapeError::VersionRefused {
            shape: self.shape.clone(),
            wanted: self.wanted,
            serves: self.serves.clone(),
        }
        .fmt(f)
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 8. Rule 2.9 — a door per payload
// ═════════════════════════════════════════════════════════════════════════════

/// **THE THRESHOLD. IT IS PRIVATE, AND THAT IS A RULING RATHER THAN A STYLE.**
///
/// At or below this many bytes the consuming chassis asks for a resource inline
/// over the websocket it already has open; above it, it fetches by id over
/// HTTP. The number is 64 KiB because that is substrate's
/// `resource::RESOURCE_CHUNK_BYTES`, the unit the HTTP door already streams in
/// — so the fallback begins exactly where the other door starts earning its
/// keep by streaming instead of buffering.
///
/// The operator's reason for keeping it private: a published number becomes a
/// thing consumers depend on, and then it can never be changed. So it is absent
/// from `ShapeMax`, from `shapes.list`, from every frame, and from every method
/// in this module — no accessor answers it and no refusal text quotes it. It is
/// also NOT the producer's bound on the same question (substrate's
/// `resource::MAX_WS_INLINE_BYTES`): two halves of one chassis each bound their
/// own frame, and neither tells the other its number.
///
/// This is a departure from `docs/access-patterns.md` 1.2 in substrate, which
/// put a `MAX_WS_PAYLOAD_BYTES` on the producer's published ceiling table;
/// substrate's `docs/consumer-migration-ledger.md` records it.
const WS_INLINE_CEILING_BYTES: u64 = 64 * 1024;

/// **WHICH DOOR THE CHASSIS CHOSE FOR THIS PAYLOAD.** A plan, not an act — see
/// [`Mirror::plan`] for why that is the shape.
///
/// It goes to the half of the consuming chassis that owns a connection. It
/// never reaches an application, because the call an application makes
/// ([`Mirror::read`]) answers bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use = "a fetch plan nobody executes is a resource nobody read"]
pub enum Fetch {
    /// Ask inline over the open websocket: the `resource/{id}/read` method, one
    /// opaque id, no query.
    Inline { id: String },
    /// Fetch by id over HTTP: `GET /resource/<id>`.
    ById { id: String },
}

impl Fetch {
    /// The id either arm carries. Opaque: read off a summary, never built.
    pub fn id(&self) -> &str {
        match self {
            Fetch::Inline { id } | Fetch::ById { id } => id,
        }
    }
}

/// **THE TWO DOORS, AS THE CALLER IMPLEMENTS THEM.**
///
/// Handed to [`Mirror::read`] for the length of one call. The mirror holds no
/// instance, stores none, and has no field of this type — which is what keeps
/// *"no socket, no connection, no runtime"* literally true of the struct while
/// the DECISION still lives beside the state it is made out of.
///
/// Both methods answer bytes or a `Refusal`, and they answer the same refusal
/// vocabulary: substrate's HTTP door serialises *"the same bytes the websocket
/// refusal carries"*, so an application reading a failure cannot tell from it
/// which door produced it either.
pub trait ResourceDoor {
    /// Ask the producer for `id`'s bytes inline, over the open websocket.
    fn inline(&self, id: &str) -> Result<Vec<u8>, Refusal>;
    /// Fetch `id`'s bytes by id, over HTTP.
    fn by_id(&self, id: &str) -> Result<Vec<u8>, Refusal>;
}

/// **WHY THERE ARE NO BYTES — AND NOT ONE VARIANT NAMES A PROTOCOL.**
///
/// That is the design and not an accident. These are what an application sees
/// when [`Mirror::read`] fails, so a variant called `WebsocketTooBig`, or a
/// message mentioning `GET`, would hand the application the one fact rule 2.9
/// says it must not have. [`Self::NoTransport`] comes nearest and still says
/// only what the PRODUCER declared, echoing the producer's own words back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentError {
    /// This cell names no resource — or names something that is not one. A
    /// shape says what exists, and this one did not say this exists.
    NoSummary,
    /// The cell holding the summary is not `Known`. A summary in an unhydrated
    /// or dead cell is the ABSENCE of a statement, not a statement.
    NotKnown { reason: String },
    /// No door the producer declared can carry a payload this size to this
    /// chassis. `offered` is the producer's own list, verbatim.
    NoTransport { bytes: u64, offered: Vec<String> },
    /// The door answered a different number of bytes than the summary
    /// declared. The bytes are dropped: deliver-or-refuse.
    Integrity { declared: u64, got: u64 },
    /// The producer refused, typed, and the refusal is passed through
    /// untouched. The code is what the consumer branches on
    /// (`envelope::codes`), exactly as it is on either door.
    Door(Refusal),
}

impl std::fmt::Display for ContentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContentError::NoSummary => f.write_str(
                "this cell names no resource; a shape says what exists and this one did not",
            ),
            ContentError::NotKnown { reason } => {
                write!(f, "the cell naming this resource is not known: {reason}")
            }
            ContentError::NoTransport { bytes, offered } => {
                let doors = if offered.is_empty() {
                    "nothing".to_string()
                } else {
                    offered.join(", ")
                };
                write!(
                    f,
                    "a {bytes}-byte resource cannot be carried by what the producer \
                     offers ({doors})"
                )
            }
            ContentError::Integrity { declared, got } => write!(
                f,
                "the summary declared {declared} bytes and {got} arrived; refused \
                 rather than delivered with a discrepancy"
            ),
            ContentError::Door(r) => write!(f, "{}: {}", r.code, r.message),
        }
    }
}

/// What [`Mirror::read`] answers: **bytes, or why not, and nothing else.**
pub type ContentResult = Result<Vec<u8>, ContentError>;

/// **THE INLINE DOOR'S PAYLOAD, DECODED HERE SO THAT EVERY CONSUMER DOES NOT
/// DECODE IT ITSELF.**
///
/// `resource/{id}/read` answers `{bytes, content_type, base64}`. It is base64
/// because the bytes behind an id are arbitrary and a JSON string cannot hold a
/// byte that is not UTF-8; substrate's `resource::read_resource_inline` is the
/// encoder. That makes it a WIRE FORMAT, which makes reading it the chassis's
/// job, which puts it in the floor beside everything else this module parses.
/// A `ResourceDoor::inline` implementation is then two lines, and the encoding
/// is not re-derived once per consumer the way the mirror itself was.
///
/// The declared `bytes` is checked against the decoded length here as well, and
/// that is NOT redundant with [`Mirror::read`]'s check. This one compares the
/// inline reply against ITSELF, which catches a transport-level corruption;
/// that one compares what arrived against what the SUMMARY said, which is the
/// claim the application actually depends on.
pub fn inline_bytes(payload: &Value) -> Result<Vec<u8>, Refusal> {
    let Some(encoded) = payload.get("base64").and_then(Value::as_str) else {
        return Err(Refusal::malformed(
            "a resource reply carries `base64`; this one did not",
        ));
    };
    let bytes = decode_b64(encoded).ok_or_else(|| {
        Refusal::new(
            crate::envelope::codes::INTEGRITY,
            "a resource reply's `base64` is not base64",
        )
    })?;
    if let Some(declared) = payload.get("bytes").and_then(Value::as_u64) {
        if declared != bytes.len() as u64 {
            return Err(Refusal::new(
                crate::envelope::codes::INTEGRITY,
                format!(
                    "a resource reply declared {declared} bytes and decoded to {}",
                    bytes.len()
                ),
            ));
        }
    }
    Ok(bytes)
}

/// One standard-alphabet base64 character's six bits. `None` for anything else,
/// `=` included — which is what makes padding in the middle of a string a
/// refusal rather than something to skip over.
fn sextet(c: u8) -> Option<u32> {
    Some(match c {
        b'A'..=b'Z' => u32::from(c - b'A'),
        b'a'..=b'z' => u32::from(c - b'a') + 26,
        b'0'..=b'9' => u32::from(c - b'0') + 52,
        b'+' => 62,
        b'/' => 63,
        _ => return None,
    })
}

/// Standard-alphabet base64, padded, STRICT. `None` on anything that is not
/// exactly that.
///
/// Strictness is the point rather than pedantry: a lenient decoder turns a
/// corrupted payload into short bytes, which is deliver-and-warn one layer
/// down, and the whole resource path is deliver-or-refuse. So the length must
/// be a multiple of four, `=` is legal only as the last one or two bytes of the
/// whole string, and every other byte must be in the alphabet.
fn decode_b64(encoded: &str) -> Option<Vec<u8>> {
    let raw = encoded.as_bytes();
    if raw.len() % 4 != 0 {
        return None;
    }
    let pad = raw.iter().rev().take_while(|c| **c == b'=').count();
    if pad > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(raw.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    // `=` has no sextet, so any padding that is not at the very end refuses
    // here without needing its own check.
    for c in &raw[..raw.len() - pad] {
        acc = (acc << 6) | sextet(*c)?;
        bits += 6;
        if bits == 24 {
            out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]);
            acc = 0;
            bits = 0;
        }
    }
    // The length check above leaves exactly three possibilities for the tail:
    // nothing, one byte of payload in twelve bits, or two bytes in eighteen.
    if bits == 18 {
        out.extend_from_slice(&[(acc >> 10) as u8, (acc >> 2) as u8]);
    } else if bits == 12 {
        out.push((acc >> 4) as u8);
    }
    Some(out)
}

// ═════════════════════════════════════════════════════════════════════════════
// 9. The repair pass — rule 2.6's leniency, made concrete
// ═════════════════════════════════════════════════════════════════════════════

// Rule 2.6's obligation on a consumer: **a word this code does not know is
// IGNORED and COUNTED, and is never allowed to destroy the frame around it.**
//
// Four fields on `ShapeWire` are closed, tagged enums, so each of them can
// destroy a frame on its own: `op`, `event.kind`, `members.state` and
// `epoch.state`. Two of the four were covered in substrate and two were not
// until 2026-10-02: an unknown `members.state` or `op` was a hard serde failure
// on the whole `ShapeWire`, so the frame fell through to `reject::UNREADABLE`
// and was lost — **and with it the frame's `subscribes` and `promises`**, which
// are the chassis's own bookkeeping and have nothing to do with the unknown
// word. Rule 2.5 has no re-emit verb, so neither id is ever offered again
// except by a re-subscribe the consumer has to decide to issue. The observable
// defect was a subscribe that never settled and a promise that never resolved,
// with nothing for the consumer to wait on and no refusal to render.
//
// Three of the four repairs REMOVE something and carry a flag into the apply,
// because what they change is which part of the frame may be believed. The
// fourth, `epoch.state`, is a SUBSTITUTION: the state the mirror already holds
// is written into the patched frame, which then parses and applies normally.

/// The closed vocabularies, each beside the field it closes. A word outside its
/// list is what the repair pass is for.
const KNOWN_OPS: &[&str] = &["read", "live"];
const KNOWN_KINDS: &[&str] = &["row", "gone", "added", "transition", "shared"];
const KNOWN_MEMBER_STATES: &[&str] = &["known", "not_hydrated"];
const KNOWN_SHAPE_STATES: &[&str] = &["known", "not_hydrated", "dead"];

/// What the repair pass had to do to a frame to make it parse at all. Every
/// field holds the word that was not understood, and every one of them is
/// countable.
#[derive(Debug, Default, Clone)]
struct Repair {
    /// An `op` word this build does not know. The patch substitutes `read`
    /// purely so serde hands over the envelope; nothing reads the substituted
    /// value, because this field being `Some` is what stops any body from being
    /// applied at all. See `reject::UNKNOWN_OP` for why *replace one key* and
    /// *append an occurrence* have no safe third guess.
    op: Option<String>,
    /// An `event.kind` word this build does not know. The `event` is removed,
    /// and the `live` path then says *this frame's event is a word I do not
    /// know* rather than *this frame carries no event*, which are two different
    /// malformations.
    kind: Option<String>,
    /// A `members.state` word this build does not know. `members` is removed
    /// and the roster this consumer already holds STANDS — guessing that an
    /// unheard-of word meant *here are the keys* or *I cannot enumerate* would
    /// be inventing a claim about somebody's pool.
    members: Option<String>,
    /// An `epoch.state` word this build does not know. The state the mirror
    /// already holds is substituted, so the previous state stands; guessing
    /// that an unknown word means *fine* is how a board shows a confident
    /// number it has no right to.
    shape_state: Option<String>,
}

impl Repair {
    /// Did anything need repairing? A frame that needed any of the four is
    /// applied IN PART, whichever position the word was in — including one
    /// whose body was skipped entirely, which is still a frame whose epoch,
    /// stages and promises landed.
    fn any(&self) -> bool {
        self.op.is_some()
            || self.kind.is_some()
            || self.members.is_some()
            || self.shape_state.is_some()
    }
}

/// A closed-enum word at `path`, when the frame has one there and it is a
/// string. One walker, so the four sites below are one line each rather than
/// four hand-rolled `and_then` chains.
fn word_at<'a>(raw: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut at = raw;
    for step in path {
        at = at.get(step)?;
    }
    at.as_str()
}

/// The word at `path` IF it is outside that field's vocabulary. `None` means
/// either *absent* or *understood*, and neither needs repairing.
fn unknown_word(raw: &Value, path: &[&str], vocabulary: &[&str]) -> Option<String> {
    let word = word_at(raw, path)?;
    (!vocabulary.contains(&word)).then(|| word.to_string())
}

// ═════════════════════════════════════════════════════════════════════════════
// 10. The mirror
// ═════════════════════════════════════════════════════════════════════════════

/// **THE SHAPE-LEVEL SCALARS AND THEIR PROVENANCE**, which move together or
/// not at all: an export refreshes both, a live `shared` moves the value and
/// leaves the provenance where it was, and death marks every cell without
/// touching a value. Grouping them is what makes that asymmetry one line each
/// at the three sites instead of a pair of fields to remember.
#[derive(Debug, Clone)]
struct Scalars {
    value: Value,
    cells: BTreeMap<String, CellState>,
}

impl Default for Scalars {
    /// An empty OBJECT, not `Value::Null`: a shape with no shape-level scalars
    /// has a `shared` block with nothing in it, which is a different statement
    /// from having no block.
    fn default() -> Self {
        Self {
            value: Value::Object(Default::default()),
            cells: BTreeMap::new(),
        }
    }
}

/// **EVERYTHING ONE `shapes.list` ENTRY TEACHES A MIRROR**, in one field.
///
/// Grouped rather than six fields on [`Mirror`], and the reason is
/// [`Mirror::reset`]. Every one of these is a fact about the PRODUCER, so all
/// six survive a reconnect — and substrate's `reset` restores them one named
/// line at a time, which is a list that has to be extended by hand every time
/// `learn` grows. Here a seventh field added to this struct survives a reset
/// by construction, because the reset moves the STRUCT.
#[derive(Debug, Clone, Default)]
struct Learned {
    /// Rule 2.8's ceilings. `None` makes [`Mirror::check`] refuse nothing: it
    /// has no contract to judge against, and inventing one would be worse than
    /// asking.
    max: Option<ShapeMax>,
    /// Rule 2.6's version contract. `None` makes [`Mirror::link`] judge
    /// nothing, on exactly the same grounds.
    contract: Option<ShapeContract>,
    /// The parent shape's name, for a nested shape.
    parent: Option<String>,
    /// **WHICH SHAPE EACH `Ref` CELL'S IDS ARE KEYS OF** — the key-level
    /// nesting edge. A consumer holding a row with a `ref:node` cell asks
    /// which shape to subscribe to for those ids.
    refs: BTreeMap<String, String>,
    /// The paging contract. `None` is *not handed a `ShapeInfo`* OR *a
    /// producer built before paging existed*; `max` tells those two apart.
    page: Option<PageContract>,
    /// **THE CELL MANIFEST**, as `(cells, shared_cells)`. `None` is *no
    /// `ShapeInfo` has been handed over*, and [`Mirror::declared`] then
    /// answers [`Declaration::Unstated`].
    manifest: Option<(Vec<CellInfo>, Vec<CellInfo>)>,
}

/// **WHAT THIS CONNECTION HOLDS OF ONE SHAPE.**
///
/// A pure state machine over frames: no socket, no clock, no I/O. The module
/// docs list the seven things a connection owes it.
#[derive(Debug, Clone)]
pub struct Mirror {
    name: String,
    held: BTreeMap<String, Held>,
    scalars: Scalars,
    members: Roster,
    /// **KEYS THE PRODUCER HAS POSITIVELY UNSAID** — see [`KeyState::Gone`].
    /// Kept apart from `held` and from `members` because a key that was removed
    /// and a key that was never mentioned are different facts, and neither of
    /// those two can hold the difference: both answer with an absence.
    ///
    /// A key that COMES BACK leaves this set. An export or a `row` for it is
    /// the producer unsaying the removal, and a mirror that kept the `Gone`
    /// would then be the stale claim in the other direction.
    vanished: BTreeSet<String>,
    /// **KEYS THE PRODUCER ANSWERED WITH NO ROW** — see [`KeyState::Absent`].
    /// Apart from `vanished` because *left* and *never held* differ, and apart
    /// from `held` because an absent key has no row to hold. A key that later
    /// arrives with a row leaves this set.
    absent: BTreeSet<String>,
    epoch: Epoch,
    /// Everything `shapes.list` has taught this mirror — see [`Learned`], and
    /// [`Self::reset`] for why it is one field and not six.
    learned: Learned,
    /// True once at least one KEY export has landed. *Am I holding any of
    /// this* — never *does this daemon have shapes*. A lazy shape takes real
    /// time to hydrate on its first subscribe and the two questions differ for
    /// that whole window.
    exported: bool,
    exports: u64,
    last_frame_at: Option<Timestamp>,
    resolved: BTreeSet<u64>,
    subscribes: BTreeMap<u64, SubscribeStatus>,
    received: u64,
    rejected: Vec<Rejected>,
    /// Set by [`Self::refused`]. Not an epoch state: the producer's fold is
    /// fine, it is the LINK that is not.
    refused: Option<String>,
    /// **THE VERSION REFUSAL THIS MIRROR WENT TERMINAL ON** — rule 2.6, set by
    /// [`Self::refused_version`].
    ///
    /// Beside `refused` rather than inside it, because the prose and the
    /// numbers answer different questions. A renderer draws the string; a
    /// consumer saying *I am the wrong build and here is what this daemon
    /// serves* needs the list. Deriving the second from the first would mean
    /// parsing the producer's English, which is exactly what a typed refusal
    /// exists to stop.
    version_mismatch: Option<VersionMismatch>,
}

impl Mirror {
    /// A mirror of one shape, holding nothing.
    pub fn new(shape: impl Into<String>) -> Self {
        Self {
            name: shape.into(),
            epoch: Epoch::default(),
            scalars: Scalars::default(),
            learned: Learned::default(),
            members: Roster::default(),
            held: BTreeMap::new(),
            vanished: BTreeSet::new(),
            absent: BTreeSet::new(),
            subscribes: BTreeMap::new(),
            resolved: BTreeSet::new(),
            rejected: Vec::new(),
            last_frame_at: None,
            refused: None,
            version_mismatch: None,
            exported: false,
            exports: 0,
            received: 0,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    // ── The two doors ────────────────────────────────────────────────────────

    /// **ONE FRAME, AS JSON. THE DOOR A SOCKET USES.**
    ///
    /// Everything [`Self::apply`] does, plus the repair pass rule 2.6 obliges
    /// every consumer to run. Prefer this over `apply` on anything that came
    /// off a wire: a closed-enum word this build predates is a hard serde
    /// failure, and losing a whole frame to one word is rule 2.6's soft spot 2
    /// arriving as data loss instead of as a countable rejection.
    pub fn accept(&mut self, raw: &Value) -> Receipt {
        match serde_json::from_value::<ShapeWire>(raw.clone()) {
            Ok(wire) => self.apply(&wire),
            Err(_) => self.repair(raw),
        }
    }

    /// **ONE FRAME, ALREADY PARSED.** Use [`Self::accept`] on bytes.
    pub fn apply(&mut self, wire: &ShapeWire) -> Receipt {
        self.fold(wire, &Repair::default())
    }

    /// The typed parse failed. Find every closed-enum word this build does not
    /// know, patch the frame so serde will hand over the envelope, fold what
    /// survives, and COUNT what did not.
    fn repair(&mut self, raw: &Value) -> Receipt {
        let repair = Repair {
            op: unknown_word(raw, &["op"], KNOWN_OPS),
            kind: unknown_word(raw, &["event", "kind"], KNOWN_KINDS),
            members: unknown_word(raw, &["members", "state"], KNOWN_MEMBER_STATES),
            shape_state: unknown_word(raw, &["epoch", "state"], KNOWN_SHAPE_STATES),
        };

        let mut patched = raw.clone();
        if let Some(obj) = patched.as_object_mut() {
            if repair.op.is_some() {
                obj.insert("op".to_string(), Value::String("read".to_string()));
            }
            if repair.kind.is_some() {
                obj.remove("event");
            }
            if repair.members.is_some() {
                obj.remove("members");
            }
        }
        if repair.shape_state.is_some() {
            let held = match self.epoch.state {
                ShapeState::Known => "known",
                ShapeState::NotHydrated => "not_hydrated",
                ShapeState::Dead => "dead",
            };
            if let Some(epoch) = patched.get_mut("epoch").and_then(Value::as_object_mut) {
                epoch.insert("state".to_string(), Value::String(held.to_string()));
            }
        }

        let wire = match serde_json::from_value::<ShapeWire>(patched) {
            Ok(wire) => wire,
            Err(why) => {
                // Nothing salvageable, and no single unknown word explains it.
                // The frame is RECEIVED and not applied, and the producer can
                // count that — which is the whole reason the reason is a code
                // and not prose. Serde's own words ride along as the detail,
                // because that is the one thing here a human can act on.
                self.received += 1;
                let receipt =
                    Receipt::rejecting(self.received, reject::UNREADABLE, Some(why.to_string()));
                self.rejected.extend(receipt.rejected.iter().cloned());
                return receipt;
            }
        };

        let mut receipt = self.fold(&wire, &repair);
        // The substitution is counted LAST, after the body's own codes, so that
        // a caller reading `rejected[0]` sees the word that explains the most
        // about what did not land.
        if let Some(word) = &repair.shape_state {
            let noted = Rejected {
                serial: self.received,
                reason: reject::UNKNOWN_SHAPE_STATE.to_string(),
                detail: Some(word.clone()),
            };
            receipt.rejected.push(noted.clone());
            self.rejected.push(noted);
        }
        if repair.any() && receipt.disposition() == Disposition::Applied {
            receipt.disposition = Some(Disposition::AppliedInPart);
        }
        receipt
    }

    // ── The fold ─────────────────────────────────────────────────────────────

    /// One frame's whole effect, in the order the order matters in.
    fn fold(&mut self, wire: &ShapeWire, repair: &Repair) -> Receipt {
        if wire.shape != self.name {
            // Deliberately not an error and deliberately not counted on the
            // ack: an application that grows a second shape must not have this
            // mirror throw on the other one's frames, and a frame that was
            // never this mirror's was never this mirror's to receive.
            return Receipt::rejecting(self.received, reject::NOT_MINE, Some(wire.shape.clone()));
        }
        self.received += 1;
        let serial = self.received;

        // THE EPOCH IS ABSORBED BEFORE `op` IS EVEN LOOKED AT, because the
        // death notice is a keyless export that means nothing at all until the
        // epoch has said the task is dead.
        self.absorb_epoch(&wire.epoch);

        let mut receipt = match (&repair.op, wire.op) {
            // An unknown `op` applies NOTHING. Everything that is not the body
            // still lands: the epoch above, rule 2.7's stages below, and the
            // promises. A consumer wedged by one word it does not recognise is
            // the failure class this whole design is against.
            (Some(_), _) => Receipt::default(),
            (None, Op::Read) => self.fold_read(wire, serial, repair),
            (None, Op::Live) => self.fold_live(wire, serial, repair.kind.as_deref()),
        };
        if let Some(word) = &repair.op {
            receipt.rejected.push(Rejected {
                serial,
                reason: reject::UNKNOWN_OP.to_string(),
                detail: Some(word.clone()),
            });
            // The `kind` is counted by `fold_live` when the body ran. When it
            // did not, it is counted here — an unknown `event.kind` is still a
            // fact about the seam.
            if let Some(kind) = &repair.kind {
                receipt.rejected.push(Rejected {
                    serial,
                    reason: reject::UNKNOWN_KIND.to_string(),
                    detail: Some(kind.clone()),
                });
            }
        }

        // ── rule 2.7's stages ───────────────────────────────────────────────
        receipt.complete = wire.complete;
        receipt.next = wire.next.clone();
        for id in &wire.subscribes {
            receipt.subscribes.push(*id);
            let stage = match wire.complete {
                Some(true) => SubscribeStatus::Complete,
                Some(false) => SubscribeStatus::Short,
                None => SubscribeStatus::Open,
            };
            let slot = self.subscribes.entry(*id).or_insert(SubscribeStatus::Open);
            // A frame that is not the last of a subscribe never downgrades one
            // that has already settled.
            if wire.complete.is_some() || *slot == SubscribeStatus::Unknown {
                *slot = stage;
            }
        }

        // ── rule 2.4's promises ─────────────────────────────────────────────
        //
        // **ONLY AN APPLIED FRAME RESOLVES ONE.** A promise resolves when the
        // fact it is about is VISIBLE, and a frame this consumer could not
        // apply put no fact in the mirror — so its ids stay pending and the
        // promise stays visibly unanswered. That is the same call
        // `wire::Resolving::put_back` makes on the producing side (*"an
        // unresolved promise is visible, a silently discarded one is not"*),
        // and it is safe because an export re-offers every id still owed.
        if receipt.applied() {
            for id in &wire.promises {
                if self.resolved.insert(*id) {
                    receipt.promises.push(*id);
                }
            }
        }

        self.rejected.extend(receipt.rejected.iter().cloned());
        receipt
    }

    fn absorb_epoch(&mut self, epoch: &Epoch) {
        // `init_at` is KEPT when the frame omits it: a live frame carries no
        // init stamp and must not erase the one the export gave.
        if epoch.init_at.is_some() {
            self.epoch.init_at = epoch.init_at;
        }
        if epoch.parent_epoch.is_some() {
            self.epoch.parent_epoch = epoch.parent_epoch;
        }
        // ── RULE 2.6's WARNING LATCHES. IT DOES NOT TRACK. ──────────────────
        //
        // Kept when a frame omits it, as `init_at` and `parent_epoch` are, and
        // for a stronger reason than either. Those two are omitted by frames
        // that do not carry the fact. This one is omitted by the DEATH FRAME,
        // which is built outside the shape task and addresses every connection
        // at once — so tracking the field would repaint a superseded link as
        // clean on precisely the last frame it will ever receive.
        //
        // And that property is what rule 2.6 asks for in its own words: a
        // consumer on a contract the producer no longer calls current must not
        // be able to look healthy. Only [`Self::reset`] clears it — a NEW link,
        // which states its own version and gets its own answer.
        if epoch.superseded.is_some() {
            self.epoch.superseded = epoch.superseded;
        }
        self.epoch.reinit = epoch.reinit;
        self.epoch.lost = epoch.lost;
        self.epoch.state = epoch.state.clone();
        if epoch.state == ShapeState::Dead {
            self.epoch.dead_reason =
                Some(epoch.dead_reason.clone().unwrap_or_else(|| DIED.to_string()));
        }
    }

    // ── `op: read` — the export ──────────────────────────────────────────────

    /// An export replaces what is held for one key, or states the pool's
    /// membership, or kills the lot. Four keyless shapes and one keyed one, and
    /// the order they are told apart in is load-bearing.
    fn fold_read(&mut self, wire: &ShapeWire, serial: u64, repair: &Repair) -> Receipt {
        if let Some(as_of) = wire.as_of {
            self.last_frame_at = Some(as_of);
        }

        // 1. A MEMBERSHIP PAGE IN A WORD THIS BUILD DOES NOT KNOW.
        //
        // First, and it RETURNS. A membership export is keyless, so falling
        // through would reach the keyless branch below and be read as a
        // shape-level read — or, on a dead epoch, as the death notice. The
        // roster this consumer holds STANDS, the word is counted, and the
        // frame's `subscribes` and `promises` are recorded by the caller, which
        // is the entire point of not losing the frame.
        if let Some(word) = &repair.members {
            return Receipt {
                rejected: vec![Rejected {
                    serial,
                    reason: reject::UNKNOWN_MEMBERS_STATE.to_string(),
                    detail: Some(word.clone()),
                }],
                ..Receipt::default()
            };
        }

        // 2. THE MEMBERSHIP FRAME. Before the death notice and before
        //    `shared`, because it is the one keyless export that is neither:
        //    substrate's `export_members` emits `key: null, members: Some(page)`
        //    and deliberately no `shared`.
        if let Some(page) = &wire.members {
            return Receipt::still(self.members.page(page));
        }

        let mut receipt = Receipt::default();
        self.absorb_shared(wire, serial, &mut receipt);

        let Some(key) = &wire.key else {
            // 3. THE KEYLESS EXPORT IS THE DEATH NOTICE. The supervisor emits
            //    exactly one frame when a task dies — `key: null`,
            //    `epoch.state: dead`, no row, no cells — to every connection
            //    holding any key of the shape. One send, every mirror.
            //
            //    A keyless export on a LIVE epoch is a shape-level read
            //    instead, and `absorb_shared` above has already taken the only
            //    thing it carries.
            if self.epoch.state == ShapeState::Dead {
                self.die();
            }
            receipt.moved = true;
            return receipt;
        };

        // 4. A KEYED EXPORT WITH NEITHER ROW NOR CELLS IS *I DO NOT HOLD THIS
        //    KEY* — see [`KeyState::Absent`]. substrate's `hub::export` builds
        //    `(root.row_at(k), root.cells(k))` and both are `None` exactly when
        //    the root does not hold the key: a `threads` subscribe naming a
        //    thread that does not exist, a per-key hydrate that refused.
        //
        //    That is a POSITIVE answer about the key, and it used to be folded
        //    as a bare row — `key_state` said `Held` and every cell said *no
        //    export has carried this cell*, for the life of the connection,
        //    which reads as *not yet* and means *never*.
        //
        //    The frame's bookkeeping still lands, because the subscribe this
        //    export answers IS answered: with an absence.
        if wire.row.is_none() && wire.cells.is_none() {
            let dropped = self.held.remove(key).is_some();
            let newly = self.absent.insert(key.clone());
            self.vanished.remove(key);
            self.exported = true;
            self.exports += 1;
            receipt.moved = dropped || newly;
            return receipt;
        }

        // 5. THE ORDINARY KEYED EXPORT.
        let (cells, unknown) = read_cells(wire.cells.as_ref());
        receipt.rejected.extend(
            unknown
                .into_iter()
                .map(|word| Rejected {
                    serial,
                    reason: reject::UNKNOWN_CELL_STATE.to_string(),
                    detail: Some(word),
                }),
        );
        let mut held = Held::empty(key.clone());
        held.row = match &wire.row {
            Some(row) if row.is_object() => row.clone(),
            _ => Value::Object(Default::default()),
        };
        held.cells = cells;
        held.as_of = wire.as_of;
        // An export IS a fold point, so `at` starts there: everything applied
        // on top of it is later by construction.
        held.at = wire.as_of;
        self.held.insert(key.clone(), held);
        // An export of a key that had been unsaid is the producer unsaying the
        // removal; a row for a key the producer had answered with none is the
        // producer now holding it.
        self.vanished.remove(key);
        self.absent.remove(key);
        // **A KEY EXPORTED WITH A ROW IS A KEY THAT EXISTS.** substrate's
        // `shapes.py` keeps holdings and membership in one dict and so never
        // had to say this. Here they are apart, and an export states existence
        // just as much as an `added` does.
        self.members.added(key);
        self.exported = true;
        self.exports += 1;
        receipt.moved = true;
        receipt
    }

    fn absorb_shared(&mut self, wire: &ShapeWire, serial: u64, receipt: &mut Receipt) {
        if let Some(shared) = &wire.shared {
            self.scalars.value = shared.clone();
        }
        if wire.shared_cells.is_some() {
            let (cells, unknown) = read_cells(wire.shared_cells.as_ref());
            receipt.rejected.extend(unknown.into_iter().map(|word| Rejected {
                serial,
                reason: reject::UNKNOWN_CELL_STATE.to_string(),
                detail: Some(word),
            }));
            self.scalars.cells = cells;
        }
    }

    // ── `op: live` — the derived events ──────────────────────────────────────

    fn fold_live(&mut self, wire: &ShapeWire, serial: u64, unknown_kind: Option<&str>) -> Receipt {
        let Some(event) = &wire.event else {
            if let Some(kind) = unknown_kind {
                // **THE EVENT WAS A WORD THIS BUILD DOES NOT KNOW, AND THE
                // REST OF THE FRAME STILL LANDED.** The epoch was absorbed
                // before `op` was looked at, and rule 2.7's stages are recorded
                // after this returns, so it is applied in part and not a lost
                // frame.
                return Receipt {
                    disposition: Some(Disposition::AppliedInPart),
                    rejected: vec![Rejected {
                        serial,
                        reason: reject::UNKNOWN_KIND.to_string(),
                        detail: Some(kind.to_string()),
                    }],
                    ..Receipt::default()
                };
            }
            // A live frame carries an event by construction. One that does not
            // states nothing at all, which is not the same as a word this build
            // failed to know.
            return Receipt::rejecting(
                serial,
                reject::UNREADABLE,
                Some("a live frame with no event".to_string()),
            );
        };

        if let Some(at) = wire.at {
            self.last_frame_at = Some(at);
        }

        // `shared` is the one derived event that is keyless, so it is answered
        // before the key is demanded.
        if let Derived::Shared { shared } = event {
            // **`shared_cells` IS NOT TOUCHED.** Only an export refreshes the
            // provenance of a shape-level scalar; a live `shared` carries the
            // value and one `at`.
            let moved = &self.scalars.value != shared;
            self.scalars.value = shared.clone();
            return Receipt::still(moved);
        }

        let Some(key) = &wire.key else {
            return Receipt::rejecting(serial, reject::NO_EXPORT, None);
        };

        match event {
            Derived::Row { row } => self.live_row(key, row, wire.at, serial),
            // Membership only. An `added` carries no row, and a consumer at
            // `detail: rows` upgrades by NAMING the key.
            Derived::Added => Receipt::still(self.members.added(key)),
            Derived::Gone => self.live_gone(key),
            Derived::Transition { cell, item } => self.live_transition(key, cell, item, wire.at, serial),
            Derived::Shared { .. } => unreachable!("answered above, before the key is demanded"),
        }
    }

    fn live_row(&mut self, key: &str, row: &Value, at: Option<Timestamp>, serial: u64) -> Receipt {
        if !row.is_object() {
            return Receipt::rejecting(serial, reject::UNKNOWN_KIND, None);
        }
        let entry = self.held.entry(key.to_string()).or_insert_with(|| {
            // A `row` for a key this connection was never exported. substrate's
            // `hub::drive` gates live frames on the export having happened, so
            // it should not arrive — but if it does, **THE ROW IS REAL AND ITS
            // PROVENANCE IS NOT.** It is held with no cells at all, so every
            // cell under it reads unhydrated. Inventing `Known` here is exactly
            // the lie the three-valued state exists to prevent.
            Held::empty(key.to_string())
        });
        entry.row = row.clone();
        entry.at = at;
        self.members.added(key);
        // A key that comes back is neither gone nor absent any more.
        self.vanished.remove(key);
        self.absent.remove(key);
        Receipt::still(true)
    }

    fn live_gone(&mut self, key: &str) -> Receipt {
        let dropped = self.held.remove(key).is_some();
        let unlisted = self.members.gone(key);
        // **RECORDED, NOT MERELY DROPPED.** Removing the row alone would make
        // every cell read *nobody has told me* about a key the producer has
        // just told the consumer about. A `gone` for a key this connection
        // never held is still worth keeping — it is the answer to *where is the
        // row I was waiting for*. See [`KeyState`].
        let newly = self.vanished.insert(key.to_string());
        Receipt::still(dropped || unlisted || newly)
    }

    fn live_transition(
        &mut self,
        key: &str,
        cell: &str,
        item: &Value,
        at: Option<Timestamp>,
        serial: u64,
    ) -> Receipt {
        let Some(entry) = self.held.get_mut(key) else {
            // The one place substrate's Python mirror silently drops an item.
            // The consumer rule is *hold the export, apply everything after
            // it*, and this is a frame with no export to be after — so it is a
            // COUNTABLE rejection rather than a `False` nobody can tally.
            return Receipt::rejecting(serial, reject::NO_EXPORT, Some(cell.to_string()));
        };
        // **THE STAMP IS ON THE FRAME, NOT ON THE ITEM, AND IT IS SPELLED THE
        // WAY THE EXPORT SPELLS IT.**
        //
        // substrate's `fold::Sequence::take_pending` serialises the item alone
        // while the export's `to_json` adds `at` to every retained item, so
        // grafting it here is what makes an appended item indistinguishable
        // from an exported one. And `Sequence::to_json` writes a retained
        // item's stamp with `to_rfc3339()` — `+00:00`, automatic sub-seconds —
        // while a FRAME's `at` serialises as `…Z`. The Python mirror grafts the
        // frame's own string, so there an appended item and an exported one are
        // still told apart by their spelling, which is the exact thing the
        // graft exists to prevent.
        let mut item = item.clone();
        if let (Some(at), Some(obj)) = (at, item.as_object_mut()) {
            obj.entry("at".to_string())
                .or_insert_with(|| Value::String(at.to_rfc3339()));
        }
        entry.transitions.entry(cell.to_string()).or_default().push(item);
        entry.at = at;
        Receipt::still(true)
    }

    // ── Death, and the other terminal ────────────────────────────────────────

    /// **EVERY HELD CELL BECOMES `Dead`. EVERY ROW IS KEPT.**
    ///
    /// Keeping them is the whole point: an operator has to be able to see WHAT
    /// went stale, and a board that empties itself on a dead fold is
    /// indistinguishable from a board whose keys all went away. The values stay
    /// and every one of them is marked last-known, so nothing renders as
    /// current.
    fn die(&mut self) {
        let reason = self
            .epoch
            .dead_reason
            .clone()
            .unwrap_or_else(|| DIED.to_string());
        self.go_terminal(&reason);
    }

    fn go_terminal(&mut self, reason: &str) {
        for entry in self.held.values_mut() {
            entry.mark_dead(reason);
        }
        for cell in self.scalars.cells.values_mut() {
            *cell = CellState::Dead {
                reason: reason.to_string(),
            };
        }
    }

    /// **THE LINK WAS REFUSED — rule 2.6's OTHER terminal, and the one that
    /// cannot arrive as a frame.**
    ///
    /// A version mismatch is refused at link time, and an unknown or dead shape
    /// is refused on the request id. Both reach a consumer as an
    /// `envelope::Outcome` rather than as a `ShapeWire`, so this module cannot
    /// see either and the connection has to say so. `docs/shapes.md` 6.5 puts
    /// *"going terminal on death or refusal"* on the consuming half, and
    /// without this method the second half of that sentence is unreachable.
    ///
    /// **Refusal is NOT death, and the two are kept apart**, because what a
    /// human does about them differs: a dead fold is the producer's problem, a
    /// refused link is usually this consumer's version. They have the same
    /// consequence for what is held — nothing under it is current — so the rows
    /// are marked exactly as death marks them, and KEPT, for exactly the same
    /// reason.
    pub fn refused(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.go_terminal(&reason);
        self.refused = Some(reason);
    }

    /// **GO TERMINAL ON A VERSION REFUSAL, CARRYING WHAT THE PRODUCER SERVES** —
    /// rule 2.6's refused state, as the typed twin of [`Self::refused`].
    ///
    /// [`Self::refused`] takes a free string, which is right for the refusals
    /// whose whole content is prose. This one exists because rule 2.6's refusal
    /// has STRUCTURE a consumer has to be able to report — *I am a v2 build and
    /// this daemon serves 4 and 3* — and a consumer that had to parse the
    /// producer's English back into numbers to say that would be re-deriving a
    /// fact it was handed, which is the collapse this crate repairs everywhere
    /// else.
    ///
    /// It leaves the mirror exactly as [`Self::refused`] does: every held cell
    /// `Dead`, every row kept, never repainted healthy.
    pub fn refused_version(&mut self, mismatch: &VersionMismatch) {
        self.version_mismatch = Some(mismatch.clone());
        self.refused(mismatch.to_string());
    }

    /// Why the link was refused, if it was.
    pub fn refusal(&self) -> Option<&str> {
        self.refused.as_deref()
    }

    /// The version refusal this mirror went terminal on, if that is why.
    /// Typed, so a consumer reports the numbers; [`Self::refusal`] still
    /// answers the prose for a renderer that only draws text.
    pub fn version_mismatch(&self) -> Option<&VersionMismatch> {
        self.version_mismatch.as_ref()
    }

    /// **NOTHING HERE WILL EVER BE CURRENT AGAIN ON THIS CONNECTION** — the
    /// fold died, or the link was refused. Drawn as such, never repainted
    /// healthy, and cleared only by [`Self::reset`].
    pub fn terminal(&self) -> bool {
        self.dead() || self.refused.is_some()
    }

    // ── What an application reads: the epoch ─────────────────────────────────

    pub fn epoch(&self) -> &Epoch {
        &self.epoch
    }

    pub fn state(&self) -> &ShapeState {
        &self.epoch.state
    }

    pub fn dead(&self) -> bool {
        self.epoch.state == ShapeState::Dead
    }

    pub fn dead_reason(&self) -> Option<&str> {
        self.epoch.dead_reason.as_deref()
    }

    /// Holding real state, from a fold that is still folding, over a link that
    /// was not refused.
    pub fn live_shape(&self) -> bool {
        self.exported && !self.terminal()
    }

    pub fn exported(&self) -> bool {
        self.exported
    }

    /// How many KEY exports have landed on this connection. A membership page
    /// is not one of them: it exports no row.
    pub fn exports(&self) -> u64 {
        self.exports
    }

    /// The newest stamp seen on any frame — a FACT ABOUT THE FOLD, and never a
    /// poll clock.
    pub fn last_frame_at(&self) -> Option<Timestamp> {
        self.last_frame_at
    }

    /// **THIS LINK IS BEING SERVED ON A CONTRACT THE PRODUCER NO LONGER CALLS
    /// CURRENT** — read off the epoch of the frames that actually arrived, not
    /// off a reply.
    ///
    /// `None` is two statements at once — *the link is current* and *no frame
    /// has arrived yet* — and a consumer that needs them apart reads
    /// [`Self::exported`] beside it. The mirror does not invent a third answer,
    /// because the producer never sent one.
    ///
    /// **A superseded link is NOT terminal.** Rule 2.6 says it *serves*, with a
    /// warning, and [`Self::terminal`] is for the other two ends — a dead fold
    /// and a REFUSED link. A consumer that drew this as dead would be refusing
    /// a link the producer accepted, which is the producer's call to make.
    pub fn superseded(&self) -> Option<Superseded> {
        self.epoch.superseded
    }

    // ── What an application reads: the state ─────────────────────────────────

    pub fn shared(&self) -> &Value {
        &self.scalars.value
    }

    pub fn shared_cells(&self) -> &BTreeMap<String, CellState> {
        &self.scalars.cells
    }

    pub fn shared_cell(&self, name: &str) -> CellState {
        self.scalars
            .cells
            .get(name)
            .cloned()
            .unwrap_or_else(|| self.absence(name, Level::Shape))
    }

    /// **THE REASON A CELL NOBODY HAS CARRIED GIVES — AND IT SAYS "NEVER" WHEN
    /// THE MANIFEST SAYS SO.**
    ///
    /// This adds no fourth value to `CellState`; the typed form of the
    /// distinction is [`Self::declared`]. It exists because a human reading a
    /// `?` on a screen must not be told *not yet* about a cell that will never
    /// come.
    fn absence(&self, name: &str, level: Level) -> CellState {
        let declaration = match level {
            Level::Key => self.declared(name),
            Level::Shape => self.shared_declared(name),
        };
        let reason = match declaration {
            Declaration::Undeclared { declares } => {
                let listed = if declares.is_empty() {
                    "nothing at this level".to_string()
                } else {
                    declares.join(", ")
                };
                format!(
                    "`{name}` is not a cell `{}` declares, so no export will ever carry \
                     it; it declares: {listed}",
                    self.name
                )
            }
            Declaration::Declared(_) | Declaration::Unstated => UNSEEN.to_string(),
        };
        CellState::NotHydrated {
            reason: Some(reason),
        }
    }

    /// **THE KEYS THIS CONNECTION HOLDS A ROW FOR**, sorted, so that row order
    /// never depends on frame arrival order. Distinct from [`Self::members`],
    /// which is what the producer says EXISTS.
    pub fn keys(&self) -> Vec<String> {
        self.held.keys().cloned().collect()
    }

    pub fn held(&self, key: &str) -> Option<&Held> {
        self.held.get(key)
    }

    pub fn row(&self, key: &str) -> &Value {
        self.held.get(key).map_or(&Value::Null, Held::row)
    }

    /// **ONE CELL'S STATE FOR ONE KEY.** Three values, as the wire has them. A
    /// cell no export carried is unhydrated, and its reason says NEVER rather
    /// than *not yet* when the manifest does not declare the name. For the
    /// typed form of that distinction ask [`Self::declared`]; for the key-level
    /// one ask [`Self::key_state`].
    pub fn cell(&self, key: &str, name: &str) -> CellState {
        match self.held.get(key) {
            Some(held) if held.cells.contains_key(name) || held.dead_reason.is_some() => {
                held.cell(name)
            }
            _ => self.absence(name, Level::Key),
        }
    }

    /// **DOES THIS KEY EXIST, AS FAR AS ANYTHING THAT ARRIVED HERE SAYS?** Four
    /// values — [`KeyState`] says why the middle two are not optional.
    ///
    /// Read this before dropping a row that has gone empty: `Gone` means the
    /// producer removed it and the row should go; `Unknown` means nothing has
    /// been said and the row should wait.
    pub fn key_state(&self, key: &str) -> KeyState {
        if self.held.contains_key(key) {
            KeyState::Held
        } else if self.vanished.contains(key) {
            KeyState::Gone
        } else if self.absent.contains(key) {
            KeyState::Absent
        } else {
            KeyState::Unknown
        }
    }

    /// Every key the producer has positively unsaid to this connection, in key
    /// order — the [`KeyState::Gone`] set.
    pub fn vanished(&self) -> Vec<String> {
        self.vanished.iter().cloned().collect()
    }

    /// Every key the producer has answered with no row, in key order — the
    /// [`KeyState::Absent`] set, and the twin of [`Self::vanished`].
    pub fn absent(&self) -> Vec<String> {
        self.absent.iter().cloned().collect()
    }

    /// What the producer says is IN THE POOL.
    pub fn members(&self) -> &Roster {
        &self.members
    }

    // ── What the producer published: `shapes.list` ───────────────────────────

    /// **READ THE CONTRACT BEFORE USING IT.** One `ShapeInfo` off
    /// `shapes.list` carries rule 2.8's ceilings, rule 2.6's version contract,
    /// the cell manifest, the paging contract, the parent and the referents —
    /// everything a consumer is supposed to know before it asks for anything.
    ///
    /// An entry for another shape is ignored outright: the ceilings are a fact
    /// about THIS shape, and a mirror that learned a neighbour's would refuse
    /// against the wrong numbers.
    pub fn learn(&mut self, info: &ShapeInfo) {
        if info.name != self.name {
            return;
        }
        self.learned = Learned {
            max: Some(info.max.clone()),
            contract: Some(info.contract.clone()),
            parent: info.parent.clone(),
            refs: info.refs.clone(),
            page: info.page,
            manifest: Some((info.cells.clone(), info.shared_cells.clone())),
        };
    }

    pub fn max(&self) -> Option<&ShapeMax> {
        self.learned.max.as_ref()
    }

    /// The producer's published version contract, off `shapes.list`.
    pub fn contract(&self) -> Option<&ShapeContract> {
        self.learned.contract.as_ref()
    }

    /// The parent shape's name, for a nested shape.
    pub fn parent(&self) -> Option<&str> {
        self.learned.parent.as_deref()
    }

    /// **WHAT A SILENT KEYLESS SUBSCRIBE ANSWERS WITH, AND THE MOST A `page`
    /// MAY BE** — the producer's paging contract, read before asking.
    pub fn page(&self) -> Option<PageContract> {
        self.learned.page
    }

    /// **WHICH SHAPE THE IDS IN A `Ref(name)` CELL ARE KEYS OF** — the
    /// key-level nesting edge, walked.
    ///
    /// `None` is one of two statements, told apart with [`Self::max`]: a mirror
    /// never handed a `ShapeInfo` knows nothing (and `max()` is `None` too),
    /// while one that was handed a `ShapeInfo` and still answers `None` has
    /// been told that `name` holds ids in a space no shape serves.
    pub fn refers(&self, ref_name: &str) -> Option<&str> {
        self.learned.refs.get(ref_name).map(String::as_str)
    }

    /// Every declared referent, `(ref name, shape name)`, in name order.
    pub fn refs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.learned.refs.iter().map(|(r, s)| (r.as_str(), s.as_str()))
    }

    /// **DOES THE PRODUCER DECLARE A PER-KEY CELL BY THIS NAME?** Ask it before
    /// believing a `NotHydrated` out of [`Self::cell`]: `Undeclared` means the
    /// cell will never arrive and the READ is the defect, `Declared` means
    /// wait. See [`Declaration`].
    pub fn declared(&self, name: &str) -> Declaration {
        match &self.learned.manifest {
            Some((cells, _)) => Self::look_up(cells, name),
            None => Declaration::Unstated,
        }
    }

    /// [`Self::declared`] for the shape-level cells — the `shared_cells` block.
    pub fn shared_declared(&self, name: &str) -> Declaration {
        match &self.learned.manifest {
            Some((_, shared)) => Self::look_up(shared, name),
            None => Declaration::Unstated,
        }
    }

    fn look_up(cells: &[CellInfo], name: &str) -> Declaration {
        match cells.iter().find(|c| c.name == name) {
            Some(found) => Declaration::Declared(found.clone()),
            None => Declaration::Undeclared {
                declares: cells.iter().map(|c| c.name.clone()).collect(),
            },
        }
    }

    /// The whole learned manifest, `(cells, shared_cells)`, or `None` before
    /// any `ShapeInfo` was handed over.
    pub fn manifest(&self) -> Option<(&[CellInfo], &[CellInfo])> {
        self.learned
            .manifest
            .as_ref()
            .map(|(cells, shared)| (cells.as_slice(), shared.as_slice()))
    }

    /// **WOULD A LINK AT THIS VERSION BE SERVED, SERVED-WITH-A-WARNING, OR
    /// REFUSED?** — rule 2.6's three states, decided against the contract this
    /// mirror was handed.
    ///
    /// The exact counterpart of [`Self::check`] for rule 2.8: ceilings and
    /// contract are both published so a consumer can know the answer BEFORE it
    /// asks, and both answers are typed so the consumer reports a number
    /// rather than a word. A mirror handed no `ShapeInfo` answers
    /// `Link::Unstated`, for the same reason `check` refuses nothing — it has
    /// no contract to judge against, and inventing one would be worse than
    /// asking.
    ///
    /// **It decides nothing the producer will not decide again.** The
    /// producer's refusal at link time is authoritative and this is advisory,
    /// which is why both call the SAME function, `ShapeContract::link`, instead
    /// of each implementing the comparison. Two implementations of one
    /// three-way decision is the second source rule 2.4 exists to delete.
    pub fn link(&self, version: Option<u32>) -> Link {
        match &self.learned.contract {
            Some(contract) => contract.link(version),
            None => Link::Unstated,
        }
    }

    /// **IS THIS ASK INSIDE THE PUBLISHED CEILINGS?** Asking for nothing is
    /// always inside them, because nothing MEANS the ceiling. A mirror that has
    /// not been handed a `ShapeInfo` refuses nothing: it has no contract to
    /// judge against, and inventing one would be worse than asking.
    ///
    /// The first ceiling exceeded is the one reported, in the order the fields
    /// are declared on [`Ask`]. Reporting all of them would be a list a
    /// consumer has to triage; reporting one is a number it can act on.
    pub fn check(&self, ask: &Ask) -> Result<(), OverCeiling> {
        let Some(max) = &self.learned.max else {
            return Ok(());
        };
        // One ceiling per line. Five hand-written `if let Some … if asked >`
        // blocks is where an axis gets added to `Ask` and silently never
        // checked; here, a new axis that is not on this list does not compile
        // into anything that looks checked.
        macro_rules! ceiling {
            ($asked:expr, $limit:expr, $variant:ident) => {
                if let Some(asked) = $asked {
                    if asked > $limit {
                        return Err(OverCeiling::$variant {
                            asked,
                            ceiling: $limit,
                        });
                    }
                }
            };
        }
        ceiling!(ask.rows, max.rows_per_subscribe, Rows);
        ceiling!(ask.keys_per_frame, max.keys_per_frame, KeysPerFrame);
        ceiling!(ask.future_at_secs, max.future_at_secs, FutureAt);
        ceiling!(ask.armed_timers, max.armed_timers_per_conn, ArmedTimers);
        // A `page` is bounded by the row ceiling and not by a dial of its own:
        // a page IS rows, and a second number would be a second policy.
        ceiling!(ask.page, max.rows_per_subscribe, Page);
        Ok(())
    }

    /// **ARE THIS CHILD'S ROWS AND THAT PARENT'S SUMMARY FROM THE SAME ERA?**
    ///
    /// The whole purpose of `Epoch::parent_epoch`, in one integer comparison. A
    /// parent re-init does NOT invalidate its children — they are independent,
    /// which is the nesting ruling — so this is how a consumer holding both
    /// learns that what it inferred from the parent about this child predates
    /// the parent's last re-hydrate. The repair is to re-read the parent, never
    /// the child.
    pub fn era(&self, parent: &Mirror) -> Era {
        match self.epoch.parent_epoch {
            None => Era::NoParent,
            Some(saw) if saw >= parent.epoch.reinit => Era::Same,
            Some(saw) => Era::ParentMovedOn {
                child_saw: saw,
                parent_now: parent.epoch.reinit,
            },
        }
    }

    // ── Rule 2.4, rule 2.7 ───────────────────────────────────────────────────

    /// Has the fact this promise was about arrived? Idempotent by
    /// construction: an export re-offers ids a lost frame already carried, and
    /// asking twice answers the same thing twice.
    pub fn resolved(&self, promise: u64) -> bool {
        self.resolved.contains(&promise)
    }

    /// Every promise id this mirror has seen resolved, in id order.
    pub fn resolved_promises(&self) -> Vec<u64> {
        self.resolved.iter().copied().collect()
    }

    /// Where one `shapes.subscribe` stands.
    pub fn subscribe(&self, id: u64) -> SubscribeStatus {
        self.subscribes
            .get(&id)
            .copied()
            .unwrap_or(SubscribeStatus::Unknown)
    }

    // ── Rule 2.3's ack ───────────────────────────────────────────────────────

    /// **HOW MANY FRAMES THIS MIRROR HAS RECEIVED.**
    ///
    /// `Ack::received_through` wants the highest CONTIGUOUS serial, and **no
    /// frame carries a serial today** — the *"soft"* paragraph of
    /// `docs/shapes.md` 2.2 is the whole design of that path and it has no
    /// code. So this is the mirror's own receive count, which is contiguous by
    /// construction because it is minted right here. When frames do carry a
    /// producer-minted serial, this is the thing it replaces.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Every seam fact this mirror has recorded and not yet handed out.
    pub fn rejections(&self) -> &[Rejected] {
        &self.rejected
    }

    /// **AN ACK SAYS WHAT RECEIPT DID** — received, applied, rejected. The
    /// draining is deliberate: a rejection is reported once, so a consumer that
    /// never acks is not one that accumulates the same complaint for ever.
    ///
    /// **Nothing sends one of these yet.** No producer reads an ack
    /// (`wire::Ack`'s own doc says so); this fills in the half rule 2.3 named
    /// and the consumer owes.
    pub fn ack(&mut self) -> Ack {
        Ack {
            shape: self.name.clone(),
            received_through: self.received,
            rejected: std::mem::take(&mut self.rejected),
        }
    }

    // ── Rule 2.9's transport choice ──────────────────────────────────────────

    /// **THE SUMMARY THIS KEY'S CELL CARRIES, OR WHY THERE IS NONE.**
    ///
    /// Public because [`Self::plan`] and [`Self::read`] both need it, and a
    /// caller that wants the SIZE for its own reasons — a progress bar, a
    /// refusal to spend a gigabyte — should not have to re-derive it off
    /// [`Held::field`]. It carries no transport word, so reading it tells an
    /// application nothing about how bytes would arrive.
    ///
    /// **THE CELL STATE GATES THE SUMMARY, BEFORE ANY ID IS READ.** That is
    /// deliver-or-refuse applied one storey above the bytes: a summary sitting
    /// in an unhydrated or dead cell is not a statement about the world, it is
    /// the absence of one, and fetching the id off it would be a fetch against
    /// a number nobody vouched for. substrate's 416-rather-than-clamp argument
    /// in `resource` rests on the summary's `bytes` being the one place the
    /// length is stated, and it is not stated while the cell is unknown.
    pub fn summary(&self, key: &str, cell: &str) -> Result<ResourceSummary, ContentError> {
        match self.cell(key, cell) {
            CellState::Known { .. } => {}
            CellState::NotHydrated { reason } => {
                return Err(ContentError::NotKnown {
                    reason: reason.unwrap_or_else(|| UNSEEN.to_string()),
                })
            }
            CellState::Dead { reason } => return Err(ContentError::NotKnown { reason }),
        }
        let raw = match self.held(key) {
            Some(held) => held.field(cell).clone(),
            None => Value::Null,
        };
        if raw.is_null() {
            return Err(ContentError::NoSummary);
        }
        // **A VALUE THAT IS NOT A SUMMARY IS `NoSummary`, NOT A PARSE ERROR.**
        // The serde detail is dropped deliberately: this variant is a statement
        // about the SHAPE — *it did not say this exists* — and no application
        // can act on `missing field 'bytes' at line 1 column 14`. A shape that
        // half-states a resource is a producer defect, and the consumer's move
        // is the same as for a cell that states nothing at all.
        serde_json::from_value::<ResourceSummary>(raw).map_err(|_| ContentError::NoSummary)
    }

    /// **THE DECISION: THE CONSUMING HALF OF THE CHASSIS CHOOSING A TRANSPORT,
    /// PER PAYLOAD, OUT OF STATE IT HAS ALREADY MIRRORED.**
    ///
    /// The operator's ruling, from substrate: *"On PUT operations and POSTs,
    /// the size of the payload may exceed the websocket capability so we need a
    /// way to make the operation take advantage of REST for POST and PUT under
    /// the hood of the chassis. … The consumer of the chassis doesn't (read:
    /// shouldn't) care what the protocols under the hood are."* And on the
    /// grain: *"I don't actually think per operation configuration anymore, I
    /// think per payload, simply because resource sizes are known in advance in
    /// the client mirror metadata."*
    ///
    /// ## Why a PLAN and not a transport
    ///
    /// Three shapes were available and this is the one that survives the laws
    /// already in force.
    ///
    /// * **A transport the mirror OWNS** — a socket handle, an HTTP client.
    ///   Refused: this module's own doc says *no socket, no connection, no
    ///   runtime*, and `check-features.sh` asserts it by driving a whole
    ///   connection's lifecycle through a build whose dependency graph contains
    ///   no tokio at all. A mirror that opened a socket would stop being the
    ///   pure state machine the floor is.
    /// * **A trait the mirror CALLS for bytes** — that is [`Self::read`], and
    ///   it is the application's one call. It cannot be the ONLY surface,
    ///   though: fetching bytes is I/O, and a connected consumer's door is
    ///   `async`. A synchronous trait would force that consumer to block a
    ///   runtime thread inside a mirror method, so the async half has to be
    ///   able to make the decision WITHOUT calling through the mirror.
    /// * **A plan the caller executes** — this. It is a value, so an `async`
    ///   door can take it and await its own fetch. It is pure, so it can be
    ///   asserted with no socket, no runtime and no producer. And it is the
    ///   single source of the decision, because [`Self::read`] is implemented
    ///   by calling it rather than by deciding again.
    ///
    /// That last clause is the load-bearing one. Two entry points that each
    /// decided would be two policies, which is the second-source defect rule
    /// 2.4 exists to delete. There is one decision function and it is this one.
    ///
    /// ## The rule
    ///
    /// Prefer the websocket while the payload fits — it is already open, so
    /// there is no connection to make and no second round trip — and fall back
    /// to the by-id door above a threshold that is a private const of this
    /// module (see `WS_INLINE_CEILING_BYTES` for why it is not published).
    /// **It costs no extra round trip**, which is what makes per-payload
    /// affordable at all: `ResourceSummary::bytes` is already mirrored by the
    /// time anybody wants the bytes, so the choice is made out of state in
    /// hand.
    ///
    /// ## What it refuses
    ///
    /// Deliver-or-refuse, never deliver-and-warn. There is no *try the socket
    /// and fall back if it is too big*, and no plan that carries a caveat: a
    /// payload above the threshold whose summary does not offer the by-id door
    /// is [`ContentError::NoTransport`], named with what the producer DID
    /// offer, rather than an inline ask that the door one layer down would
    /// refuse as too large.
    pub fn plan(&self, key: &str, cell: &str) -> Result<Fetch, ContentError> {
        Self::choose(&self.summary(key, cell)?)
    }

    /// The decision itself, over nothing but a summary — so it can be read, and
    /// tested, without a mirror at all.
    fn choose(summary: &ResourceSummary) -> Result<Fetch, ContentError> {
        let id = || summary.id.clone();
        if summary.bytes <= WS_INLINE_CEILING_BYTES && summary.supports(Transport::Ws) {
            return Ok(Fetch::Inline { id: id() });
        }
        if summary.supports(Transport::Get) {
            return Ok(Fetch::ById { id: id() });
        }
        // Either `ws` is the only door and the payload is over this chassis's
        // own inline ceiling, or the producer offers no door this build knows.
        // Both are refused HERE and not attempted: an ask that would come back
        // `too_large` from the producer's door is a round trip spent learning
        // something already in hand.
        Err(ContentError::NoTransport {
            bytes: summary.bytes,
            offered: summary.transports.clone(),
        })
    }

    /// **THE ONE CALL AN APPLICATION MAKES. IT ANSWERS BYTES.**
    ///
    /// *"The consumer of the chassis doesn't (read: shouldn't) care what the
    /// protocols under the hood are."* So: a key, a cell, and the resource's
    /// bytes come back. Nothing in the signature, the `Ok` value or any of the
    /// refusals names a protocol, a door, a URL or a frame — an application
    /// cannot learn which door was used by reading what it got, and
    /// `transport_tests` drives this call over both doors and asserts the
    /// results are byte-identical and indistinguishable.
    ///
    /// `door` is supplied by the CALLER — the half of the consuming chassis
    /// that owns a connection. The mirror never holds it, never stores it, and
    /// this takes `&self`, so nothing about the method gives the mirror a
    /// transport; it borrows one for the length of one call. An `async`
    /// consuming half that cannot block calls [`Self::plan`] and awaits its own
    /// door, taking the same decision from the same function.
    ///
    /// ## The one place a door CAN be named, stated rather than hidden
    ///
    /// [`ContentError::Door`] passes a producer's refusal through untouched and
    /// both doors refuse in one vocabulary, so a refusal ABOUT THE RESOURCE is
    /// identical either way. The exception is a refusal about the TRANSPORT:
    /// [`inline_bytes`] reports a corrupt inline reply, and a message
    /// describing a corruption necessarily describes what corrupted.
    ///
    /// That is kept deliberately. The CODE is what an application branches on
    /// and the code set is identical; the message is a chassis DEFECT REPORT,
    /// and a generic one would make a real corruption undiagnosable to the only
    /// person who could fix it. Rule 2.9's claim is that nothing the chassis
    /// hands an application ABOUT A RESOURCE names a transport — not that a
    /// chassis bug is unattributable.
    ///
    /// ## The declared length is ENFORCED here
    ///
    /// A door that answers a different number of bytes than
    /// `ResourceSummary::bytes` declared is [`ContentError::Integrity`], and
    /// the bytes are DROPPED. substrate's `resource` refuses to clamp a byte
    /// range precisely because *"the summary is the one place a resource's
    /// length is stated"*; honouring that on the read side means a disagreement
    /// is a refusal rather than bytes plus a discrepancy the application is
    /// left to notice on its own.
    pub fn read(&self, door: &dyn ResourceDoor, key: &str, cell: &str) -> ContentResult {
        let summary = self.summary(key, cell)?;
        // THE ONE PLACE THE TWO PATHS DIVERGE, AND IT IS TWO LINES. Both arms
        // end in the same `Vec<u8>` and the same check below.
        let bytes = match Self::choose(&summary)? {
            Fetch::Inline { id } => door.inline(&id),
            Fetch::ById { id } => door.by_id(&id),
        }
        .map_err(ContentError::Door)?;
        if bytes.len() as u64 != summary.bytes {
            return Err(ContentError::Integrity {
                declared: summary.bytes,
                got: bytes.len() as u64,
            });
        }
        Ok(bytes)
    }

    // ── Recovery ─────────────────────────────────────────────────────────────

    /// **THE SOCKET DIED, OR THE DAEMON DID. EVERYTHING THE CONNECTION HELD
    /// GOES.**
    ///
    /// A mirror is only ever valid for the connection that was exported to it:
    /// a subscription is per connection, a lazy shape re-hydrates on the next
    /// subscribe, and keeping rows across a reconnect would present the last
    /// fold's answers as this fold's.
    ///
    /// **WHAT SURVIVES IS EVERYTHING `shapes.list` SAID** — the ceilings, the
    /// version contract, the parent, the referents, the paging contract and the
    /// cell manifest. Every one of those is a fact about the PRODUCER, and a
    /// reset is about the CONNECTION. Re-reading them to make the first
    /// subscribe of a reconnect legal would be a round trip for something that
    /// did not change, and a consumer that had been refused would have to be
    /// refused a second time to re-learn what it had already been told.
    ///
    /// **WHAT DOES NOT SURVIVE IS `version_mismatch`, AND THAT ASYMMETRY IS
    /// THE POINT.** A refusal is a fact about a LINK and a reset ends that
    /// link; keeping it would leave the mirror terminal across the very
    /// reconnect that exists to repair it. `refused` goes for the same reason.
    ///
    /// **THE `resolved` SET GOES TOO, AND THE REASON SUBSTRATE'S DOC FIRST
    /// GAVE FOR THAT WAS WRONG.** It said a promise id is minted per connection
    /// by the producer, so carrying one across would match this connection's id
    /// 5 against the last one's. It is not per connection:
    /// `command::PromiseBook::next` mints from ONE `AtomicU64` per producer
    /// PROCESS, so an id is unique per daemon BOOT. That is precisely what lets
    /// rule 2.5's per-key retention re-offer an id on the export a RECONNECTING
    /// consumer receives. substrate's `docs/consumer-seam.md` §8 W5 recorded
    /// the error and this is the corrected statement of it.
    ///
    /// What is actually dropped is this mirror's record of *which ids I have
    /// already seen resolved*, which is connection-scoped bookkeeping and
    /// nothing else. A consumer that needs a promise to survive a blip keeps
    /// its OWN book above the mirror and clears it on a CHANGED `boot_id`,
    /// which is the one event that really does restart the counter.
    pub fn reset(&mut self) {
        // Two moves and a replacement. Everything `shapes.list` taught this
        // mirror lives in ONE field, so it survives by construction: a seventh
        // fact added to `learn` cannot be forgotten here, which a per-field
        // restore list cannot promise.
        let name = std::mem::take(&mut self.name);
        let learned = std::mem::take(&mut self.learned);
        *self = Self::new(name);
        self.learned = learned;
    }
}

#[cfg(test)]
mod tests;

/// Rule 2.9's transport choice, kept in a file of its own rather than appended
/// to `tests.rs`. Every test there drives frames through `accept` to assert
/// what RECEIPT did; these assert what the mirror DECIDES about state it
/// already holds. Two different questions, and the fake the second needs — a
/// door — has no business in the first.
#[cfg(test)]
mod transport_tests;
