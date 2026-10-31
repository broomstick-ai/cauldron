//! # The `threads/log` shape — a thread's LIVE EVENT LOG, as a bounded window
//!
//! The 58% of the ledger no shape carried. `crate::shapes::threads` says WHERE
//! the log's head is (`newest`, `last_message_id`, `seen_seq`, `unread`) and
//! DISCARDS [`crate::shapes::threads::BULK_KINDS`] — `stream_event` 674,493 rows,
//! `message_usage` 431,018, `unknown` 333,422 — so a consumer that wanted the
//! transcript it is looking at had to keep a legacy firehose arm and a cursor.
//! `docs/consumer-seam.md` §8 names that as Gap A, *"the big one"*, and names the
//! cost: *"both apps need the ledger of the open/own thread as it happens: the
//! TUI assembles a reply from 125 `stream_event` deltas per message and renders
//! tool calls as they land; the AUI speaks `assistant_text` paragraphs as they
//! stream"*, and *"the gap is what keeps the legacy firehose alive in both
//! seams"*.
//!
//! This is that gap closed on the producer side. **No consumer is migrated here
//! and no existing method is removed** — removal comes after the consumers move,
//! or they break.
//!
//! ## What a row is, and why — the central decision
//!
//! **A row is ONE LEDGER EVENT, one-for-one with a row of the `events` table,
//! every kind, assembled into nothing.** The alternative — a window of ASSEMBLED
//! messages, 125 deltas folded into one `assistant_text` — was argued and is
//! refused on four grounds, the last of which is decisive:
//!
//! 1. **Assembly is INTERPRETATION, and the operator ruled interpretation out of
//!    a general shape.** The ruling is quoted in `crate::shapes::threads`' module
//!    doc: *"Anything that INTERPRETS those payloads — the summary/title
//!    derivation being the example — is a separate application-specific shape. A
//!    general shape must not encode one app's meaning."* Deciding that these
//!    deltas plus that `assistant_text` are one message is exactly that
//!    derivation, one layer down.
//! 2. **Assembly would be a SECOND SOURCE for a fact the ledger already holds.**
//!    The daemon appends `assistant_text` when the turn completes. A shape that
//!    assembled its own copy would hold a reconstruction beside the authoritative
//!    row, and a consumer that saw the two differ could not say which was right —
//!    the thing S3 spent a stage deleting.
//! 3. **Assembly cannot pass the re-init proof.** An assembled window would be
//!    built one way by the hydrate (reading `events`) and another by the live fold
//!    (folding deltas), so the two paths would produce different objects for the
//!    same thread and a re-subscribe would change what the mirror holds. A log of
//!    envelopes has ONE builder — [`row_of`] — called by both.
//! 4. **A filtered or assembled window does not retire the firehose, which is the
//!    entire point.** `docs/consumer-seam.md` §8: a consumer that sees only
//!    completed messages *"would see a reply only when it completed, and would
//!    issue a history read per 100 ms debounce while anything streams"*. The
//!    firehose stays load-bearing and nothing is gained.
//!
//! So the window is UNFILTERED. `stream_event` is folded here and the counter
//! `crate::shapes::threads` keeps for discarding it (`events_ignored`) has no
//! sibling on this shape, because this fold discards nothing by kind.
//!
//! ## Where the bytes go — [`Payload`], and the operator's division
//!
//! > *"Message bodies stay resources. The shape carries structure — seq, ts,
//! > kind, resource id — and the resource door carries bytes. Do not put large
//! > content in the sequence."*
//!
//! Obeyed, with one bound and three answers, and `bytes` is never lost:
//!
//! * **[`Payload::Here`]** — the whole event, held OPAQUELY, on the frame, when it
//!   serialises at or under [`MAX_INLINE_PAYLOAD`]. Every `stream_event` lands
//!   here, which is what makes live rendering possible at all: a delta is ~150
//!   bytes, its `ResourceSummary` is ~75, and a fetch per delta is 125 round trips
//!   a message — strictly worse than the firehose this shape exists to retire.
//!   Carrying it is not a preference, it is the arithmetic of a payload class that
//!   is small by construction.
//! * **[`Payload::Split`]** — a MESSAGE whose text is large: the event's structure
//!   on the frame with its `text` key REMOVED, and the text behind the door. So
//!   `intent`, `metadata`, `parent_tool_use_id` and `api_message_id` still reach a
//!   consumer that is nesting subagent output, and the paragraph does not.
//! * **[`Payload::There`]** — anything else over the bound: the whole event behind
//!   the door, by summary.
//!
//! **Nothing in this file reads the `Value`.** The event is held exactly as
//! `threads/{id}/history`'s `event` field holds it, which is `crate::shapes::
//! threads`' `app` cell's rule applied to every kind.
//!
//! ## The `kinds` filter moved CLIENT-SIDE, and what that cost
//!
//! `threads/{id}/history` filters by kind server-side. This window does not, and
//! the argument is that **a filter is a VIEW and a shape carries FACTS**: two
//! consumers of one thread want different kinds, a producer that filtered would
//! hold N windows for N views, and a `kinds` subscribe parameter would be a
//! second dial on a root there is only one of (`crate::wire::Retain`'s own
//! who-pays argument, which is why `retain` is a projection at delivery and not a
//! retention request). A consumer filters its own mirror, for free, and can
//! change its mind without a round trip.
//!
//! **It costs bytes and the number is measured, not asserted** —
//! `tests/live_log_size.rs` serialises a full window of realistic rows and pins
//! it. See [`WINDOW`] for what the measurement decided.
//!
//! ## `last_user_input_at` is NOT re-introduced
//!
//! A sibling landed `recent-updates`' ordering on last user input only (`37187d6`)
//! because *"any event is activity"* was wrong for an INDEX: only 0.36% of
//! 2,463,981 events are `user_message`, so a `stream_event` reordering the fleet
//! was a defect. **This shape carries no order over threads and no activity
//! claim.** Its key is a thread somebody NAMED, its cell is that thread's own log
//! in `seq` order, and nothing here is comparable across keys. The one thing that
//! could have re-introduced the defect — a shape-level recency built out of these
//! appends — is deliberately absent: that index exists, it is
//! `crate::shapes::recent`, and it orders on `user_message` alone.
//!
//! ## Deep scrollback is NOT this shape, and this shape does not foreclose it
//!
//! Past the window the transcript is a paged walk over immutable per-event
//! resources, and *"I am scrolled into the past"* is a face-owned position rather
//! than a fact. What this shape owes that walk is the place to start it, and it
//! pays it without a cell: the oldest retained row's `seq` is on the export, so
//! `docs/consumer-seam.md`'s own rule — *"page `history` up to `as_of`, fold
//! transitions after"* — is readable off the mirror. No `as_of` cell exists,
//! because a position derivable from a row the consumer already holds is derived
//! at render (`docs/shapes.md` §4.2).
//!
//! The door the walk goes through is widened by this shape and not invented by
//! it: `crate::shapes::body`'s `served_of` now serves EVERY event by
//! `(thread_id, seq)`, where before it refused anything that was not one of the
//! three message kinds. That is what makes the past reachable without an RPC.
//!
//! ## Tracking and the fold's cost
//!
//! `Lazy`, for `crate::shapes::threads`' reason and more sharply: this fold reads
//! EVERY `Push::Event`, including the 58% its sibling returns on, at a measured
//! ~50 events/s peak. `Lazy` is what makes an unwatched daemon pay none of it, and
//! the scope guard ([`holds`]) is what keeps the root from growing to the fleet —
//! 2,463,981 events exist and an arm that minted a key would hold every thread
//! within minutes.
//!
//! **This fold mints NO key and records NO membership.** `crate::shapes::threads`
//! is the pool and owns the roster; a second shape claiming *this thread exists*
//! would be the second source again. A consumer learns what exists from the parent
//! (`detail: keys`) and opens a window on what it wants.
//!
//! ## The hydrate bound
//!
//! **ONE statement per key, flat in the thread's size.** `Store::tail_events` is
//! one seek plus a backward walk on `idx_events_thread_seq` for [`WINDOW`] rows.
//! Production's largest thread carries a 177,995-entry tail; this reads 4,000 of
//! them and never 177,995, and that bound is the reason the window is a
//! producer-declared ceiling rather than *"the log"*.
//!
//! ## Version
//!
//! [`Log::VERSION`] is `1` and `SERVES` is empty, so a consumer built against any
//! other version is refused at link time (rule 2.6, built on this chassis since
//! 2026-10-05).

use serde::Serialize;
use serde_json::{json, Value};

use crate::event::{Fact, Push, StoredEvent};
use shapewire::cell::Cells;
use shapewire::declare_cells;
use shapewire::operation::{
    DType::{Cursor, Enum, List, Scalar as Sc, Struct, Timestamp as Ts},
    Presence::{Optional, Required},
    Scalar::{Int},
    TimeEncoding::{Rfc3339},
};
use crate::shape::{
    Handles, Hydrate, HydrateRequest, Hydration, Input, Map, Register, Root, Sequence, Shape,
    Stamped, Timestamp, Tracking, Tx,
};

/// **HOW MANY EVENTS ONE THREAD'S WINDOW HOLDS** — rule 2.8's ceiling for the
/// `retain` parameter on this shape, and the number that decides everything else
/// here.
///
/// **4,000, and it is the TUI's own `OPEN_CAP`.** That constant is not in this
/// workspace — the Rust faces live outside it — and it is on the record in
/// `docs/consumer-seam.md` §5 row D7: *"the open thread's bounded `Tail`
/// (`OPEN_CAP` 4,000) and the fleet activity tail (`TAIL_CAP` 2,000) are
/// rendered"*. Matching it exactly is the point: a consumer whose own buffer is
/// 4,000 rows deep can DELETE that buffer and read the mirror, which is what
/// retiring the firehose means. A smaller number here would leave every face
/// holding a bigger tail than the producer serves, and the firehose would stay.
///
/// **The bound is a COUNT and not a time window** — `Sequence::new_capacity_only`,
/// and that function carries the argument: the thread a consumer has open may have
/// been quiet for a week, and a 24 h window would hand it an empty transcript in
/// exactly the state where it most wants the last thing that was said.
///
/// **What it costs is measured, not assumed.** `tests/live_log_size.rs` serialises
/// a full unfiltered window and pins the number, so a row that grows is a red test
/// rather than a surprise on the wire.
pub const WINDOW: usize = 4_000;

/// **HOW MANY OF THIS SHAPE'S KEYS ONE CONNECTION MAY HOLD OPEN** — rule 2.8's
/// ceiling for the KEY parameter, published as `ShapeMax::open_keys_per_conn` and
/// refused by `crate::wire::ShapeError::TooManyKeys`.
///
/// **8.** Under `Hydrate::PerKey` a key costs a whole window, so the honest
/// question is how many transcripts one face is legitimately looking at: ONE open
/// thread, plus a navigation history of what it just left. Eight is that, with
/// room; fifty is a navigator that opens a key per thread it passes and closes
/// none, which is the hazard this ceiling exists for and which no `Lazy` drop
/// catches, because the connection never ends.
///
/// The refusal says, in full:
///
/// ```text
/// shape `threads/log` holds at most 8 keys open per connection and this one
/// already holds 8: `shapes.unsubscribe` a key you are no longer looking at to
/// release a slot. It is not clamped and nothing was queued
/// ```
///
/// It is `TooManyKeys` and not `AboveMax` because `AboveMax`'s remedy — *"ask for
/// 8 or less"* — is not the repair: the ask was for ONE key and there is no
/// smaller ask. **A re-subscribe to a key already held costs nothing**, because the
/// reservation is a SET: rule 2.5 makes re-subscribe the remedy for every doubt a
/// consumer can have, and a ceiling that refused the remedy would break recovery.
pub const OPEN_WINDOWS: usize = 8;

/// **THE LARGEST EVENT THIS WINDOW PUTS ON A FRAME** — the bound between
/// [`Payload::Here`] and its two siblings, and the operator's *"do not put large
/// content in the sequence"* expressed as a number.
///
/// **1,024 bytes.** Above every `stream_event` delta the API produces and below
/// every real message body, so the class that must be inline is inline by
/// construction and the class that must not be is not.
///
/// It is NOT published and no consumer asks against it, which is deliberate and is
/// `docs/shapes.md` §2.9's own rule for the sibling threshold: *"a published
/// number becomes a thing consumers depend on and can then never be changed"*. A
/// consumer reads [`Payload`]'s tag and does what it says; it never computes this.
pub const MAX_INLINE_PAYLOAD: usize = 1_024;

// ─────────────────────────────────────────────────────────────────────────────
// The row
// ─────────────────────────────────────────────────────────────────────────────

/// **WHERE THIS ROW'S BYTES ARE. Exactly one answer, always** — never both, never
/// neither, and the tag is the whole of what a consumer branches on.
///
/// Three variants and not two, because eliding a MESSAGE's text is a different act
/// from withholding a whole event and a consumer must be able to tell them apart:
/// after a [`Self::Split`] it holds the event's structure and knows the text is one
/// fetch away; after a [`Self::There`] it holds nothing but the envelope.
///
/// Serialised externally tagged — `{"here": {...}}`, `{"split": {...}}`,
/// `{"there": {...}}` — so the discriminator is a key and not a field whose
/// absence has to be interpreted.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Payload {
    /// **THE WHOLE EVENT, HELD OPAQUELY, ON THE FRAME.** Nothing in this crate
    /// reads it — it is `threads/{id}/history`'s `event` field, byte for byte.
    Here(Value),
    /// **A MESSAGE WHOSE TEXT IS LARGE**: the structure here with its `text` key
    /// REMOVED, the text behind the door.
    ///
    /// Removed rather than set to `null`, because the tag already says the text was
    /// lifted out and a `null` would be a second spelling of that — and one a
    /// consumer could mistake for *the text is empty*, which is the collapse every
    /// rule in `docs/shapes.md` §2 is against.
    Split {
        event: Value,
        text: shapewire::wire::ResourceSummary,
    },
    /// **THE WHOLE EVENT BEHIND THE DOOR.** What `crate::shapes::body::served_of`
    /// serves for this `(thread_id, seq)`, and `bytes` is that function's own
    /// answer measured — so the size the summary states is the size the door
    /// delivers.
    There(shapewire::wire::ResourceSummary),
}

/// **ONE LEDGER EVENT, AS THE WINDOW HOLDS IT.**
///
/// `ts` is carried as the store holds it, so a row read here and a row read from
/// `threads/{id}/history` name the same instant in the same spelling. The chassis
/// ALSO stamps each retained item with an `at`, and for this cell the two are the
/// same instant by construction — see [`stamp_of`], which is the whole of why a
/// re-subscribed mirror is byte-identical to one that never disconnected.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LogRow {
    /// **THE ORDERING KEY, AND THE FOLD'S GUARD.** Monotone per thread by
    /// construction: `seq` is allocated by a `fetch_add` before the append.
    pub seq: u64,
    /// The addressable position, when this event has one. Only 47,556 of
    /// 2,463,981 events do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<u64>,
    pub kind: String,
    pub ts: String,
    pub payload: Payload,
}

/// **THE ONE BUILDER — called by the live fold AND by the hydrate, and that is
/// not a convenience.**
///
/// Both paths hold a [`StoredEvent`]: the fold gets it off `Push::Event`, the
/// hydrate gets it off `Store::tail_events`, which goes through the same
/// `rows_to_events` that degrades an unreadable payload to `Unknown { raw }`
/// exactly once. So there is one expression producing one row for one event, and a
/// mutation that changes what a hydrated row looks like changes what a folded row
/// looks like in the same edit.
///
/// That is the defence against the trap a sibling measured on 2026-10-06: twelve
/// of fourteen tests stayed GREEN under a hydrate-only mutation. Here there is no
/// hydrate-only mutation to make — and
/// `tests/live_log.rs::the_hydrate_and_the_live_fold_build_the_same_row` asserts it
/// on the rows rather than trusting the structure.
pub fn row_of(thread_id: &str, e: &StoredEvent) -> LogRow {
    LogRow {
        seq: e.seq,
        message_id: e.message_id,
        kind: e.event.kind().to_string(),
        ts: e.ts.clone(),
        payload: payload_of(thread_id, e),
    }
}

/// [`Payload`]'s decision, and the only place the bound is applied.
fn payload_of(thread_id: &str, e: &StoredEvent) -> Payload {
    let whole = serde_json::to_value(&e.event).unwrap_or(Value::Null);
    let n = serde_json::to_string(&e.event).map(|s| s.len()).unwrap_or(0);
    if n <= MAX_INLINE_PAYLOAD {
        return Payload::Here(whole);
    }
    // Over the bound. A summary needs an id, and an id cannot be minted for a
    // thread whose name holds a byte outside the resource charset
    // (`body::body_id`). There is no honest third door for such a row, so the
    // event stays on the frame: a summary naming an unfetchable resource would be
    // this shape claiming something exists that cannot be read, and dropping the
    // bytes would be worse than carrying them.
    let Some(summary) = crate::shapes::body::served_summary(thread_id, e.seq, &e.event) else {
        return Payload::Here(whole);
    };
    // A MESSAGE: lift the text out and keep the structure. `intent`, `metadata`,
    // `parent_tool_use_id` and `api_message_id` are what a consumer nesting
    // subagent output reads, and they are small.
    if crate::shapes::body::body_text(&e.event).is_some() {
        let mut event = whole;
        if let Value::Object(m) = &mut event {
            m.remove("text");
        }
        return Payload::Split {
            event,
            text: summary,
        };
    }
    Payload::There(summary)
}

/// **THE ITEM'S STAMP, WHICH MUST BE THE SAME QUANTITY ON BOTH PATHS.**
///
/// The chassis stamps every retained item and puts that stamp on the wire as `at`.
/// A hydrate can only produce the event's OWN `ts`; a live fold could produce
/// either that or the hub's fold instant. If they differed, a mirror rebuilt by
/// re-subscribing would hold a different `at` for the same item than one that never
/// disconnected — and rule 2.5's claim is that those two mirrors are the same
/// mirror. So both paths use this, and the quantity is the event's `ts`.
///
/// `fallback` is used only for a `ts` that will not parse as RFC 3339. The store
/// writes `chrono::Utc::now().to_rfc3339()`, so that is an unreachable row today;
/// it is spelled rather than `unwrap`ped because the alternative to an honest
/// fallback is a panic in a fold, and it is the ONE case in which the two paths can
/// still produce different `at`s. `LogRow` carries `ts` itself precisely so that
/// even then the ROW is identical and only the chassis stamp differs.
fn stamp_of(e: &StoredEvent, fallback: Timestamp) -> Timestamp {
    chrono::DateTime::parse_from_rfc3339(&e.ts)
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or(fallback)
}

// ─────────────────────────────────────────────────────────────────────────────
// The cells and the root
// ─────────────────────────────────────────────────────────────────────────────

/// ONE cell. There is deliberately nothing else per key: every other fact about a
/// thread is `crate::shapes::threads`', and a second home for any of them would be
/// the second source *"one way to know a thing"* forbids.
#[derive(Debug)]
pub struct LogCells {
    /// **THE WINDOW.** `Sequence`, because the order is the meaning and an item is
    /// a fact in its own right — which is also what routes it through rule 2.2's
    /// SERIAL path rather than the row debounce's coalescing one.
    pub events: Sequence<LogRow>,
}

impl Default for LogCells {
    fn default() -> Self {
        Self {
            // COUNT ONLY, NO CLOCK — see `WINDOW`.
            events: Sequence::new_capacity_only(WINDOW),
        }
    }
}

impl LogCells {
    fn to_json(&self, key: &str, retain: shapewire::wire::Retain) -> Value {
        json!({
            "thread_id": key,
            "events": self.events.to_json_retained(retain),
            // **DERIVED, NOT HELD** — how many the producer retains for this
            // thread, so a consumer can tell "the thread is short" from "the
            // window is full and the rest is scrollback" without a cell for it.
            "retained": self.events.len(),
        })
    }

    fn cells_json(&self) -> Value {
        json!({ "events": self.events.state() })
    }

    fn mark_dead(&mut self, reason: &str) {
        self.events.mark_dead(reason);
    }
}

/// The held state: the keyed `Map` plus two shape-level cells.
pub struct LogRoot {
    pub rows: Map<String, LogCells>,
    /// The ceilings, so no consumer hardcodes them — `crate::shapes::threads`'
    /// `limits` pattern. The CEILINGS THEMSELVES are published on `shapes.list` as
    /// `ShapeMax`, which is their one home; what rides here is the pair of numbers
    /// that are not ceilings a subscriber asks against.
    pub limits: Register<Value>,
    pub shape_version: Register<u32>,
    shared_dirty: bool,
}

impl Default for LogRoot {
    fn default() -> Self {
        Self {
            rows: Map::new(),
            limits: Register::new(),
            shape_version: Register::new(),
            shared_dirty: false,
        }
    }
}

impl LogRoot {
    /// Idempotent: a re-read that finds the same value must not mark `shared`
    /// dirty, or every hydrate would emit a `shared` event that changed nothing.
    fn set_shared_constants(&mut self, at: Timestamp) {
        let limits = json!({
            // Not a ceiling anybody asks against — it is the number of bytes above
            // which a row's payload moves behind the door, stated so a consumer
            // can see WHY a row is `split` or `there` rather than guessing.
            "max_inline_payload_bytes": MAX_INLINE_PAYLOAD,
            // What a `Payload` tag can be, so a consumer that meets a fourth one
            // knows it is behind a version rather than looking at a typo.
            "payload_modes": ["here", "split", "there"],
        });
        if self.limits.now() != Some(&limits) {
            self.limits.emit(at, limits);
            self.shared_dirty = true;
        }
        if self.shape_version.now() != Some(&Log::VERSION) {
            self.shape_version.emit(at, Log::VERSION);
            self.shared_dirty = true;
        }
    }
}

impl Root for LogRoot {
    fn keys(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }

    /// **THE DEFAULT, DELIBERATELY** — which answers the HELD keys.
    ///
    /// `crate::shapes::threads` overrides this with a membership read because it is
    /// the POOL and owes a `detail: keys` subscriber the roster. This shape is not
    /// a pool: it is a window per thread, opened because somebody named one. Its
    /// "what exists" question is the parent's, already answered there, and
    /// answering it again here would be a second source for the roster.
    fn row(&self, key: &str) -> Option<Value> {
        self.row_at(key, shapewire::wire::Retain::Max)
    }

    /// **THE `retain` PROJECTION, AND THIS SHAPE MUST OVERRIDE IT.** Declaring
    /// `MAX_RETAINED_ITEMS` without this would accept an ask and silently ignore
    /// it — the pairing `fold::Root::row_at` says the chassis cannot check and a
    /// test must.
    fn row_at(&self, key: &str, retain: shapewire::wire::Retain) -> Option<Value> {
        self.rows
            .get(&key.to_string())
            .map(|r| r.to_json(key, retain))
    }

    fn cells(&self, key: &str) -> Option<Value> {
        self.rows.get(&key.to_string()).map(|r| r.cells_json())
    }

    fn shared(&self) -> Value {
        json!({
            "limits": self.limits.now(),
            "shape_version": self.shape_version.now(),
        })
    }

    fn shared_cells(&self) -> Value {
        json!({
            "limits": self.limits.state(),
            "shape_version": self.shape_version.state(),
        })
    }

    fn drain(&mut self) -> crate::shape::Outbox {
        let dirty = self.rows.take_dirty_keys();
        let gone = self.rows.take_removed();
        // Always empty: this fold records no membership, so nothing becomes a
        // member without becoming a row. Drained rather than assumed, because
        // `Map::key` records an arrival and the one thing worse than an `added`
        // nobody wants is one nobody drains.
        let added = self.rows.take_added();
        let mut transitions = Vec::new();
        let mut rows = Vec::new();
        for k in dirty {
            let Some(r) = self.rows.get_mut(&k) else {
                continue;
            };
            for item in r.events.take_pending() {
                transitions.push((k.clone(), "events", item));
            }
            rows.push(k);
        }
        crate::shape::Outbox {
            rows,
            gone,
            added,
            transitions,
            shared: std::mem::take(&mut self.shared_dirty),
        }
    }

    fn mark_dead(&mut self, reason: &str) {
        self.limits.mark_dead(reason);
        self.shape_version.mark_dead(reason);
        self.rows.mark_dead(reason);
        for (_, r) in self.rows.entries_mut() {
            r.mark_dead(reason);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The hydrate request — ONE, one statement
// ─────────────────────────────────────────────────────────────────────────────

/// **THE NEWEST [`WINDOW`] EVENTS OF ONE THREAD** — the only read this shape does
/// per key, and the thing `Sequence::read` exists for.
#[derive(Default)]
pub struct ThreadTail {
    pub thread_id: String,
}

impl HydrateRequest for ThreadTail {
    const NAME: &'static str = "thread_tail";
    type Response = StoredEvent;
}

// ─────────────────────────────────────────────────────────────────────────────
// The shape
// ─────────────────────────────────────────────────────────────────────────────

pub struct Log;

impl Log {
    /// **THIS SHAPE'S VERSION.** `SERVES` is empty, so exact-match-only: a
    /// consumer stating any other number is refused at link time.
    pub const VERSION: u32 = 1;
}

declare_cells! {
    /// Per-key cells of `threads/log`: the window. `payload` is externally
    /// tagged (`here` | `split` | `there`) and is declared as the struct it is;
    /// the three tags are published on `shared.limits.payload_modes`.
    pub LOG_CELLS = [
        ("events", List(&Struct("LogRow")), Required, [
            ("seq", Cursor("event"), Required),
            ("message_id", Cursor("message"), Optional),
            ("kind", Enum(crate::event::KINDS), Required),
            ("ts", Ts(Rfc3339), Required),
            ("payload", Struct("Payload"), Required),
            ("at", Ts(Rfc3339), Required),
        ]),
    ]
}

declare_cells! {
    /// Shape-level cells of `threads/log`.
    pub LOG_SHARED = [
        ("limits", Struct("LogLimits"), Required, [
            ("max_inline_payload_bytes", Sc(Int), Required),
            ("payload_modes", List(&Enum(&["here", "split", "there"])), Required),
        ]),
        ("shape_version", Sc(Int), Required, []),
    ]
}

impl Shape<Input> for Log {
    const NAME: &'static str = "threads/log";
    const CELLS: Cells = LOG_CELLS;
    const SHARED: Cells = LOG_SHARED;
    /// **NESTED UNDER THE POOL**, because a window onto one thread's log is a
    /// child of the thing that says that thread exists. `register` enforces the
    /// name and the parents-first order.
    const PARENT: Option<&'static str> = Some("threads");
    const TRACKING: Tracking = Tracking::Lazy;
    /// A key is a thread id and the default key set is EMPTY: the whole log of the
    /// whole fleet is 2,463,981 events and 3.8 GB, and mirroring it is not a
    /// design, it is an accident. A keyless `detail: rows` is refused naming
    /// `MAX_ROWS_PER_SUBSCRIBE`; a keyless `detail: keys` is refused too, because
    /// `hydrates_members` is not implemented here — see `LogRoot::row`.
    const HYDRATE: Hydrate = Hydrate::PerKey;
    const VERSION: u32 = Self::VERSION;
    /// Rule 2.8's `retain` ceiling. Declaring it is what makes `retain: n` legal on
    /// this shape at all, and `LogRoot::row_at` is the override it obliges.
    const MAX_RETAINED_ITEMS: usize = WINDOW;
    /// Rule 2.8's KEY ceiling — see [`OPEN_WINDOWS`] for the number and the
    /// refusal.
    const MAX_OPEN_KEYS_PER_CONN: Option<usize> = Some(OPEN_WINDOWS);
    /// One ledger event appends at most one item to at most one key, so one input
    /// is one `transition` plus one `row`. Anything past a handful means the fold
    /// has started reaching for keys it does not change.
    const BURST: usize = 4;

    type Producer = crate::runtime::ThreadManager;
    type Root = LogRoot;

    fn root(&self) -> LogRoot {
        LogRoot::default()
    }

    /// **NO COUNTER, AND THE ABSENCE IS ARGUED.**
    ///
    /// `crate::shapes::threads` counts `events_ignored` because it discards by
    /// KIND and `counts` can see the kind. This fold discards nothing by kind — it
    /// is the shape that exists to carry the kinds its sibling drops. The only
    /// thing it DOES discard is by SCOPE (the thread is not held), and `counts`
    /// structurally cannot see the root. Declaring a counter that could not be
    /// incremented from here would be a number that reads as zero while the fold
    /// throws away most of its input, which is worse than no number.
    fn counts(&self, _input: &Stamped<Input>) -> Option<&'static str> {
        None
    }

    /// **INIT READS NO THREAD.** Two constants and an empty, COMPLETE key set.
    fn hydrates(
        &mut self,
        _p: &crate::runtime::ThreadManager,
        root: &mut LogRoot,
        as_of: Timestamp,
    ) {
        root.set_shared_constants(as_of);
        // Zero keys is the complete default scope: `Known` with no rows renders as
        // empty, `NotHydrated` renders `?`, and a consumer that has not yet named a
        // thread is not looking at a failure.
        root.rows.mark_read(as_of, true);
        // A hydrate publishes nothing — the export IS the publication.
        let _ = root.drain();
    }

    /// **ONE THREAD'S WINDOW, BECAUSE SOMEBODY NAMED IT. ONE STATEMENT.**
    ///
    /// Answers `false` for a thread with no events at all, which is the honest
    /// refusal and the same one `crate::shapes::threads` makes for an id that names
    /// no thread: **nothing is minted on that path**, because a window of `?` for a
    /// thread that does not exist cannot be told apart from one whose hydrate
    /// failed.
    ///
    /// A thread that EXISTS and is empty is therefore not distinguishable here from
    /// one that does not, and that is deliberate rather than overlooked: the
    /// question *does this thread exist* is the POOL's and is answered there. This
    /// shape answers *what is in its log*, and for both of those threads the answer
    /// is nothing.
    fn hydrates_key(
        &mut self,
        p: &crate::runtime::ThreadManager,
        root: &mut LogRoot,
        key: &str,
        as_of: Timestamp,
    ) -> bool {
        let id = key.to_string();
        let t_read = std::time::Instant::now();
        let got = p.hydrate(ThreadTail {
            thread_id: id.clone(),
        });
        let read_us = t_read.elapsed().as_micros();
        let reason = got.reason();
        let complete = got.is_complete();
        let events = got.rows();
        if events.is_empty() {
            // A read that FAILED and a thread with no events are two different
            // facts. The first must not mint a row at all — `Hydration::Partial`
            // with no rows is how `Handles<ThreadTail>` reports a store error — and
            // the second is the empty-or-absent case above.
            let _ = reason;
            return false;
        }
        let entries: Vec<(Timestamp, LogRow)> = events
            .iter()
            .map(|e| (stamp_of(e, as_of), row_of(&id, e)))
            .collect();
        // DEBUG, not INFO (TLL-1b.2): this fires once per `threads/log` hydrate,
        // which is per subscribe, and at INFO it is prod log volume.
        tracing::debug!(
            key = %id,
            rows = entries.len(),
            store_read_us = read_us as u64,
            total_us = t_read.elapsed().as_micros() as u64,
            "threads/log hydrated one key"
        );
        root.rows.key(id).events.read(as_of, entries, complete);
        // A hydrate publishes nothing — the export IS the publication.
        let _ = root.drain();
        true
    }

    /// **EXHAUSTIVE, WITH NO `_` ARM**, so a new `Push` or `Fact` variant is a
    /// compile error here rather than a silent drop.
    ///
    /// ONE arm appends. Three rules bind it:
    ///
    /// * **NO BULK-KIND RETURN.** This is the shape that carries them.
    /// * **NO ARM MINTS A KEY.** [`holds`] gates the one arm there is, which is
    ///   what keeps a 2.46 M-event ledger from growing this root to the fleet.
    /// * **THE GUARD IS `seq`, NOT THE STAMP.** `Sequence::append` is called rather
    ///   than `emit`, and that function's doc carries the argument: the item stamps
    ///   are the events' own `ts` (so a re-subscribed mirror matches one that never
    ///   disconnected), which makes them incomparable with the hydrate's `as_of` —
    ///   and `seq` is a stronger guard anyway, because it refuses a DUPLICATE,
    ///   which no stamp comparison can.
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, LogRoot>) {
        let at = tx.at();
        match &input.value {
            Input::Push(Push::Event(se)) => {
                if !holds(tx, &se.thread_id) {
                    return;
                }
                // **READ BEFORE REACHING.** `Map::key` marks the key dirty on
                // reach, so a guard that runs after `key(..)` has already bought
                // the `row` frame it exists to avoid.
                let held = tx
                    .rows
                    .get(&se.thread_id)
                    .and_then(|r| r.events.newest().map(|n| n.seq));
                // A replay, or an out-of-order arrival of something the window
                // already holds. `seq` is monotone per thread by construction — a
                // `fetch_add` before the append — so "at or below the head" is
                // exactly "already seen", and appending it would double-count a
                // row for ever.
                if held.is_some_and(|h| se.seq <= h) {
                    return;
                }
                tx.rows
                    .key(se.thread_id.clone())
                    .events
                    .append(stamp_of(se, at), row_of(&se.thread_id, se));
            }

            // ── EVERY OTHER INPUT, AND EACH HAS A HOME ELSEWHERE ──────────────
            //
            // EXHAUSTIVE AND DELIBERATE. `Status` and `Liveness` are
            // `crate::shapes::threads`' cells; the three narration channels are
            // `crate::shapes::narration`'s; every `Fact` below is about an ACCOUNT,
            // the daemon, or a thread-level cell `threads` owns. **Not one of them
            // is a ledger event**, and this shape holds the ledger and nothing
            // else: a `Fact::ThreadSeen` is a consumer's own act, not something
            // that happened in the transcript, and appending it would put a
            // consumer's cursor into every other consumer's log.
            Input::Push(Push::Status { .. })
            | Input::Push(Push::Liveness { .. })
            | Input::Push(Push::Progress { .. })
            | Input::Push(Push::Lifecycle(_))
            | Input::Push(Push::Info { .. })
            | Input::Fact(Fact::ThreadHeader { .. })
            | Input::Fact(Fact::ThreadSettings { .. })
            | Input::Fact(Fact::ThreadPlacement { .. })
            | Input::Fact(Fact::ThreadSeen { .. })
            | Input::Fact(Fact::ThreadCompaction { .. })
            // **THE UNDELIVERED QUEUE IS NOT THE LOG.** A queued message has not
            // happened yet; this window carries what did. The row appears here
            // only once delivery appends its event, and then as that event.
            | Input::Fact(Fact::ThreadQueue { .. })
            | Input::Fact(Fact::ThreadChildren { .. })
            | Input::Fact(Fact::ThreadRolled { .. })
            | Input::Fact(Fact::ThreadSubagents { .. })
            | Input::Fact(Fact::ThreadChildActivity { .. })
            | Input::Fact(Fact::GaugeObserved { .. })
            | Input::Fact(Fact::LimitRecorded { .. })
            | Input::Fact(Fact::LimitCleared { .. })
            | Input::Fact(Fact::PoolShare { .. })
            | Input::Fact(Fact::AccountDisabled { .. })
            | Input::Fact(Fact::LoginHealth { .. })
            | Input::Fact(Fact::CredentialChanged { .. })
            | Input::Fact(Fact::AccountRegistered { .. })
            | Input::Fact(Fact::AccountRemoved { .. })
            | Input::Fact(Fact::Pressure { .. })
            | Input::Fact(Fact::Beat { .. })
            // The producer's own warning count is `crate::shapes::warnings`'
            // business and nothing else's.
            | Input::Fact(Fact::WarningsSampled { .. }) => {}
        }
    }
}

/// **THE SCOPE GUARD.** Does this root already hold a window for that thread? A
/// `PerKey` shape's fold may never mint one — see [`Log::folds`].
fn holds(tx: &Tx<'_, LogRoot>, thread_id: &str) -> bool {
    tx.rows.contains(&thread_id.to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// The handler — ONE statement
// ─────────────────────────────────────────────────────────────────────────────

impl Handles<ThreadTail> for crate::runtime::ThreadManager {
    /// ONE seek plus a backward walk on `idx_events_thread_seq`, flat in the
    /// thread's size.
    ///
    /// **A SHORT READ IS COMPLETE AND A FULL ONE IS NOT.** Exactly [`WINDOW`] rows
    /// back means the statement hit its own limit, so the window is a FLOOR of the
    /// thread's log rather than the log — `Hydration::Partial`, which lands
    /// `complete: false` on the cell and rides out on the export. Fewer than
    /// `WINDOW` means the thread is shorter than the window and the answer is the
    /// whole thing.
    fn hydrate(&self, r: ThreadTail) -> Hydration<StoredEvent> {
        match self.store.tail_events(&r.thread_id, WINDOW) {
            Ok(events) if events.len() >= WINDOW => Hydration::Partial {
                got: events,
                reason: format!(
                    "this thread's log is longer than the {WINDOW}-event window this \
                     shape retains; the window is its recent end, and `covers_since` \
                     and the oldest row's `seq` say where it starts"
                ),
            },
            Ok(events) => Hydration::Complete(events),
            Err(e) => Hydration::Partial {
                got: Vec::new(),
                reason: format!("{e}"),
            },
        }
    }
}
