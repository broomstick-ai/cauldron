//! `held.rs` — **WHAT THIS PROCESS HOLDS A MIRROR OF.** Pure. No tokio, no
//! socket, no clock of its own.
//!
//! This file is the replacement for the hand-rolled mirror each face grew: the
//! AUI's `threads_client.rs::StatusTable`, which held `running`, `pending`,
//! `progress`, `fresh` and `watching_since` and was kept current by a firehose,
//! a 5-second stale tick and a `threads/status` query; and the terminal face's
//! `Store.threads` with its `window_full`, `status_full` and `invalidate()`.
//! Everything either of them held is now a cell on a shape, and the thing that
//! applies a frame to a cell is
//! [`shapewire::mirror::Mirror`], vendored beside this reference and differed
//! against the producer's own oracle (`lib/shapewire/tests/mirror_oracle.rs`,
//! which drives the same frame sequences through the Rust mirror and the Python
//! `ShapeMirror` and asserts the same resulting state).
//!
//! **The one-line rule this file exists to obey** (`docs/shapes.md` §2.5):
//! *hold the export, apply everything after it.* So there is no cursor here, no
//! `after_seq`, no re-request, no repair. A socket death calls [`Held::reset`]
//! and the next connection re-subscribes the whole [`Book`]; the mirror that
//! results is the same mirror as one that never disconnected, which is the
//! property `lib/shapewire/tests/resubscribe_identity.rs` asserts and
//! `lib/shapewire/seam/probe/` measures in production — and the reason
//! `backfill`, `fold_catch_up` and `resync_status` could be deleted rather than
//! ported.
//!
//! ## The two books
//!
//! A [`Mirror`] knows about frames. It does not know what this process ASKED
//! FOR, and it does not know which of its own commands are still outstanding.
//! Those are the consumer's two pieces of bookkeeping and they are the only
//! state in this file that is not a mirror:
//!
//! * [`Book`] — one [`Entry`] per `(shape, key)` this process wants, and where
//!   that ask stands ([`EntryState`]). `holders` is a count, because a face has
//!   several independent reasons to want one thread's key at once (the AUI: a
//!   menu row, a running turn, an owed reply; the terminal face: a row on
//!   screen and the open thread) and the LAST of them releasing it is what
//!   sends the `unsubscribe`.
//! * [`Promises`] — the ids of commands whose EFFECT has not yet arrived on a
//!   frame (rule 2.4: a command acknowledges, it does not answer with the fact).
//!
//! ## `emitted` — the one position, and why it is not `delivered_seqs`
//!
//! A mirror is STATE and the AUI's dispatch loop is an EVENT CONSUMER, so
//! something has to say which rows of a thread's log window have already been
//! handed over. That is [`Held::emitted`], and it replaces FOUR mechanisms:
//! `claim_delivery`, the process-wide `delivered_seqs` map, the
//! `delivered_seq` field persisted into `~/.aui/outstanding`, and
//! `seed_delivered_from_status`.
//!
//! It is one `BTreeMap<thread, seq>`, it is never sent to anybody, and —— the
//! part that lets the persisted copy die —— **its floor on first sight of a key
//! is the producer's own `seen_seq`**, read off the `threads` mirror. The
//! durable record of what the operator has already heard was always on the
//! daemon (`thread_seen`, advanced by the `threads/{id}/seen` command AUI
//! already sends); the AUI was keeping a second copy of it in its own state
//! directory and seeding that copy from a query. Now there is one copy, it is
//! the producer's, and it arrives on the export.
//!
//! See `seam/ledger.rs` for who reads it.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use shapewire::mirror::{Era, KeyState, Mirror, Receipt, SubscribeStatus};
use shapewire::wire::{Beat, Detail, Retain, ShapeInfo, Timestamp};

// ──────────────────────────────────────────────────────────────────────────
//  The shapes this face can mirror, by name
// ──────────────────────────────────────────────────────────────────────────

/// The pool: one thread in depth. **`OnDemand`** — a keyless `detail: rows`
/// subscribe is refused `key_required`, so every key is named.
pub const THREADS: &str = "threads";
/// A thread's NAME. Keyless; the 500 most recently named threads.
pub const SUMMARY: &str = "threads/summary";
/// The order to look at first, plus the unread roll-up over it. One key,
/// `fleet`.
pub const RECENT: &str = "threads/recent-updates";
/// A thread's LIVE EVENT LOG — a bounded window of raw ledger rows, every kind.
/// This is the shape that made the seam possible: with it the legacy firehose
/// carries nothing either face reads, and `ledger.rs` is a plain mirror rather
/// than the hand-rolled interim tailer the design had to budget for.
pub const LOG: &str = "threads/log";
/// The account board. Since R2.S8 it carries no threads at all — the roster
/// that used to ride it as `accounts.*.threads` is [`NON_IDLE`].
pub const ACCOUNTS: &str = "accounts";
/// **The cheapest honest fleet-wide *which threads are executing right now*
/// there is**: one key, `fleet`, holding every non-idle thread with its state,
/// whether it is executing, and the account it is placed on. Subscribed
/// keyless, because the whole point of the shape is that no key is needed.
pub const NON_IDLE: &str = "threads/non-idle";
/// **AN ACCOUNT'S WEEK OF GAUGE READINGS** (R2.S3d) — the series the 7-day
/// sparkline draws, which only the retired `gauge/history` request answered.
/// `PerKey`, keyed by account and nested under [`ACCOUNTS`]: a face opens one
/// entry per account it draws, never a keyless one.
pub const GAUGE_HISTORY: &str = "accounts/gauge-history";
/// The daemon talking while it works.
pub const NARRATION: &str = "narration";
/// **Frames are reaching me** — not *the socket is open*, not *the thread is
/// busy*. Keyed `link`.
pub const HEARTBEAT: &str = "heartbeat";

/// `threads/recent-updates`' only key.
pub const FLEET: &str = "fleet";
/// `heartbeat`'s only key.
pub const LINK: &str = "link";

/// **HOW MANY `threads/log` WINDOWS ONE CONNECTION MAY HOLD.** Not a choice:
/// it is the producer's published `ShapeMax::open_keys_per_conn` for that shape
/// (`shapes/log.rs::OPEN_WINDOWS`), refused as `TooManyKeys` and never clamped.
/// Named here so the eviction below is a deliberate policy against a number the
/// producer owns, and `Mirror::max()` is what is actually consulted at run time.
pub const LOG_WINDOWS: usize = 8;

/// Every shape a standing entry can name, in book order. Order matters only in
/// that the subscribe walk follows it, and `heartbeat` first means *is anything
/// arriving* is answerable before anything else has hydrated.
pub const STANDING: &[&str] = &[HEARTBEAT, RECENT, SUMMARY, ACCOUNTS, NON_IDLE, NARRATION];

/// **THE NARRATION CHANNELS, AND THE ORDERING CONSTRAINT NOBODY HAD WRITTEN
/// DOWN.**
///
/// `progress`, `lifecycle` and `info` are FIREHOSE-ONLY BY CONSTRUCTION in the
/// legacy wire: those frames carry an EMPTY thread id, so no per-thread
/// `{"type":"subscribe","threads":[…]}` filter can reach them. A face that
/// still reads any of the three off the firehose therefore cannot retire the
/// firehose, however complete its `threads/log` adoption is. Moving all three
/// to this shape is a PRECONDITION of that retirement, not a tidy-up after it
/// (`docs/consumer-seam.md` §6, M6).
pub const CHANNELS: &[&str] = &["lifecycle", "info", "progress"];

// ──────────────────────────────────────────────────────────────────────────
//  Three-valued answers
// ──────────────────────────────────────────────────────────────────────────

/// **THE CHASSIS'S THREE UNKNOWNS, AND THEY MAY NOT BE COLLAPSED TO TWO.**
///
/// `NotHydrated` is *nothing has told me yet* and `Dead` is *the fold that
/// maintained this is gone, so what I hold is last-known and must not be shown
/// as current*. The old tables had one of these (`fresh: false`) and every
/// reader turned it into `false`/`None`, which is why a daemon whose shape task
/// had died looked exactly like a daemon that had not answered yet.
#[derive(Debug, Clone, PartialEq)]
pub enum Fact<T> {
    Known(T),
    NotHydrated { reason: String },
    Dead { reason: String },
    /// **THE PRODUCER DECLARES ITS CELLS AND THIS IS NOT ONE OF THEM** — a
    /// fourth unknown, and the one `NotHydrated` was silently standing in for.
    ///
    /// `NotHydrated` is *not yet*. This is *never*: no export will carry the
    /// cell this read names, because the producer's manifest (`shapes.list`'s
    /// `cells`, `shapewire::cell`) does not declare it. A face read `pending`
    /// off `threads` for months on a `NotHydrated` that could not say this, and
    /// a veto built on it never fired. The reason names the cell and what the
    /// producer DOES declare. A face draws it as a defect in its own build, not
    /// as a `?` to wait on.
    ///
    /// Reachable only after `shapes/list` has been learned: before that the
    /// mirror judges nothing, and a read answers `NotHydrated` as it always did.
    Undeclared { reason: String },
}

impl<T> Fact<T> {
    /// The value, or `None` for either unknown. **The audio law's safe
    /// direction**: a caller that cannot distinguish the two unknowns must not
    /// be allowed to accidentally assert one of them, so this is the only way
    /// to get the value out without naming both arms.
    pub fn known(self) -> Option<T> {
        match self {
            Fact::Known(v) => Some(v),
            _ => None,
        }
    }

    pub fn is_known(&self) -> bool {
        matches!(self, Fact::Known(_))
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Fact<U> {
        match self {
            Fact::Known(v) => Fact::Known(f(v)),
            Fact::NotHydrated { reason } => Fact::NotHydrated { reason },
            Fact::Dead { reason } => Fact::Dead { reason },
            Fact::Undeclared { reason } => Fact::Undeclared { reason },
        }
    }

    pub(crate) fn not_hydrated(reason: impl Into<String>) -> Self {
        Fact::NotHydrated {
            reason: reason.into(),
        }
    }
}

/// **THE KEY-LEVEL QUESTION**, which is a different question from the cell-level
/// one and is answered by [`Mirror::key_state`] plus the [`Book`].
///
/// `NotAsked` is the one a face needs most and the one the old tables could not
/// express: *this thread may well be running and I have not asked about it*.
/// Reading that as "idle" is the defect `daemon_activity` was built to avoid and
/// the reason it answered `Option`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holding {
    /// Asked for, and the producer holds it.
    Held,
    /// Asked for, and the producer said it left.
    Gone,
    /// **Asked for, and the producer answered: no row.** A keyed export with
    /// neither `row` nor `cells` — a `threads` subscribe naming a thread that
    /// does not exist. A positive answer with a frame behind it, kept apart from
    /// [`Self::Unknown`] (nothing has arrived) and from [`Self::Gone`] (it was
    /// here and left). Before `KeyState::Absent` existed this read as `Held` with
    /// every cell `NotHydrated` for ever, which was the locked finding in
    /// `conformance/absent_key.jsonl`.
    Absent,
    /// Asked for, and no export has named it yet.
    Unknown,
    /// Never asked for. Nothing may be inferred about it.
    NotAsked,
    /// Asked for and REFUSED, per key. `Mirror::refused` is shape-wide, so a
    /// `threads` subscribe to a thread id that does not exist cannot live
    /// there — it would mark the whole pool refused. It lives on the entry.
    /// (`docs/consumer-seam.md` §8 G3.)
    Refused { code: String, message: String },
}

// ──────────────────────────────────────────────────────────────────────────
//  The link
// ──────────────────────────────────────────────────────────────────────────

/// Where the one socket stands. Published by `link.rs`, kept here so a reader
/// answers synchronously without touching the connection.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkState {
    Connecting { attempt: u32 },
    Up { boot_id: String, since: Timestamp },
    Down { reason: String },
}

impl LinkState {
    pub fn is_up(&self) -> bool {
        matches!(self, LinkState::Up { .. })
    }

    pub fn boot_id(&self) -> Option<&str> {
        match self {
            LinkState::Up { boot_id, .. } => Some(boot_id),
            _ => None,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  The subscription book
// ──────────────────────────────────────────────────────────────────────────

/// Where one ask stands. Rule 2.7's stages from the consuming side, plus the
/// two refusals that are per-ENTRY rather than per-shape.
#[derive(Debug, Clone, PartialEq)]
pub enum EntryState {
    /// In the book, not yet sent (or sent on a connection that died).
    Asked,
    /// The request went out and carries this id.
    Open { subscribe_id: u64 },
    /// Every frame of the answer arrived (`complete: true`).
    Complete { subscribe_id: u64 },
    /// **The producer's ceiling cut the answer short and SAID SO**
    /// (`complete: false`, rule 2.8). Not a failure and not something to retry
    /// blindly — the face says so instead.
    Short { subscribe_id: u64 },
    /// A typed refusal naming this entry.
    Refused { code: String, message: String },
    /// `shapes/list` did not name this shape. Terminal for this connection,
    /// cleared by the next reconnect (rule 2.6).
    Unsupported { note: String },
}

/// One ask: a shape, optionally a key, and how much of it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub shape: &'static str,
    pub key: Option<String>,
    pub detail: Detail,
    pub retain: Retain,
    pub beat: Beat,
    pub state: EntryState,
    /// **How many independent reasons this face has to want this key.** A face
    /// has several at once — the menu row under the cursor, a turn it is
    /// running, a reply it is owed from before this process existed — and the
    /// last of them letting go is what releases the subscription. A bare
    /// boolean here is the bug where navigating away from a thread cancels the
    /// window its own running turn is reading.
    pub holders: u32,
}

impl Entry {
    pub fn standing(shape: &'static str, key: Option<&str>) -> Self {
        Self {
            shape,
            key: key.map(str::to_string),
            detail: Detail::Rows,
            retain: Retain::Max,
            beat: Beat::Max,
            state: EntryState::Asked,
            // A standing entry is wanted by the face itself and is never
            // released, so it starts at one holder that nothing drops.
            holders: 1,
        }
    }

    pub fn status(&self) -> &EntryState {
        &self.state
    }

    /// Is this entry's answer whole, or honestly short? Either way the face may
    /// read it; `Short` only changes what the face is allowed to CLAIM about
    /// completeness.
    pub fn hydrated(&self) -> bool {
        matches!(
            self.state,
            EntryState::Complete { .. } | EntryState::Short { .. }
        )
    }

    fn matches(&self, shape: &str, key: Option<&str>) -> bool {
        self.shape == shape && self.key.as_deref() == key
    }
}

/// What this process asked for. A `Vec` and not a map because the subscribe walk
/// is in book order and the book is short (six standing entries plus the keys
/// on screen).
#[derive(Debug, Default)]
pub struct Book {
    entries: Vec<Entry>,
}

impl Book {
    /// The standing entries — **divergence D3**: which shapes a face mirrors is
    /// the one thing about the seam that is genuinely the face's own, because it
    /// is a statement about what the face draws.
    ///
    // SEAM-DIVERGE(D3): which narration channels this face draws.
    //
    // The reference holds all three, because `CHANNELS` says all three must
    // leave the firehose before the firehose can be retired. A face narrows
    // this ONLY for a channel it has no renderer for, and names which: the
    // AUI drops `progress` — there is no spoken form for a progress line and
    // the audio law forbids inventing one — and holds `lifecycle` (the
    // announcer) and `info`. The terminal face's activity lane renders
    // `progress` and it holds all three.
    //
    // Everything else in the book is the same in both faces, which is why only
    // the channel list is inside the fence and the shapes are not. The design
    // (§5 D3) had `accounts` as a per-face question too; it is not one — both
    // faces draw the account board, and both count `threads/non-idle`, which is
    // the cheapest honest fleet-wide "which threads are executing" there is.
    pub fn standing() -> Self {
        let mut entries = vec![
            // `heartbeat` FIRST and at the floor: *are frames reaching me* is
            // the question every other answer's honesty rests on, and the floor
            // is 1,000 ms (`shapes/heartbeat.rs`). A smaller number is refused
            // in-process by `Mirror::check` before it is sent.
            Entry {
                beat: Beat::Every(1_000),
                ..Entry::standing(HEARTBEAT, Some(LINK))
            },
            Entry::standing(RECENT, Some(FLEET)),
            Entry::standing(SUMMARY, None),
            Entry::standing(ACCOUNTS, None),
            Entry::standing(NON_IDLE, None),
        ];
        entries.extend(CHANNELS.iter().map(|c| Entry::standing(NARRATION, Some(c))));
        entries.shrink_to_fit();
        Self { entries }
    }
    // SEAM-END

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, shape: &str, key: Option<&str>) -> Option<&Entry> {
        self.entries.iter().find(|e| e.matches(shape, key))
    }

    pub fn get_mut(&mut self, shape: &str, key: Option<&str>) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.matches(shape, key))
    }

    /// Take a hold on `(shape, key)`, adding the entry if this is the first
    /// holder. Answers `true` when the entry is NEW and therefore needs a
    /// subscribe sent.
    pub fn hold(&mut self, shape: &'static str, key: Option<&str>) -> bool {
        if let Some(e) = self.get_mut(shape, key) {
            e.holders = e.holders.saturating_add(1);
            return false;
        }
        self.entries.push(Entry::standing(shape, key));
        true
    }

    /// Release one hold. Answers `true` when the LAST holder let go and the
    /// entry was removed, which is when a keyed `unsubscribe` must be sent —
    /// `threads` and `threads/log` are on-demand and their roots grow for the
    /// life of a connection, and a long-lived face's connection lives for days.
    pub fn release(&mut self, shape: &str, key: Option<&str>) -> bool {
        let Some(i) = self.entries.iter().position(|e| e.matches(shape, key)) else {
            return false;
        };
        let e = &mut self.entries[i];
        // A standing entry is never released: its single holder is the face.
        if e.holders > 1 {
            e.holders -= 1;
            return false;
        }
        if key.is_none() {
            return false;
        }
        self.entries.remove(i);
        true
    }

    /// Every key held for one shape, in book order.
    pub fn keys_of(&self, shape: &str) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| e.shape == shape)
            .filter_map(|e| e.key.clone())
            .collect()
    }

    /// Every entry back to `Asked`. The reconnect, and there is nothing else to
    /// it: no cursor to rewind and no decision about what to re-ask for.
    fn reset(&mut self) {
        for e in &mut self.entries {
            e.state = EntryState::Asked;
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  The promise book
// ──────────────────────────────────────────────────────────────────────────

/// One command whose effect has not arrived yet.
#[derive(Debug, Clone)]
pub struct Pending {
    pub verb: &'static str,
    pub subject: Option<String>,
    pub asked_at: Timestamp,
    /// Which daemon boot minted the id. Promise ids come from ONE `AtomicU64`
    /// per PRODUCER process, not per connection, so they survive a socket blip
    /// and do not survive a restart.
    pub boot_id: String,
}

/// Where a command stands, as a face asks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    /// Waiting for a frame to carry the id.
    Pending,
    /// A frame carried it: the fact is in the mirror.
    Visible,
    /// `visible: false` on the ack — accepted, applied, and **no frame will
    /// ever carry this id**. The face's final word is "accepted".
    NeverVisible,
    /// The id outlived the producer's per-key retention without a frame, or the
    /// daemon restarted under it. Inferred from a COMPLETE export, never from a
    /// clock.
    Lost,
    /// Nothing here ever knew this id.
    Unknown,
}

#[derive(Debug, Default)]
pub struct Promises {
    open: BTreeMap<u64, Pending>,
    settled: BTreeSet<u64>,
    never: BTreeSet<u64>,
    lost: BTreeSet<u64>,
}

impl Promises {
    pub fn open(&mut self, id: u64, p: Pending) {
        self.open.insert(id, p);
    }

    /// `visible: false`: there is nothing to wait for.
    pub fn never_visible(&mut self, id: u64) {
        self.open.remove(&id);
        self.never.insert(id);
    }

    /// A frame carried the id.
    pub fn settle(&mut self, id: u64) -> Option<Pending> {
        self.settled.insert(id);
        self.open.remove(&id)
    }

    /// Every id still outstanding for `subject`, oldest first.
    pub fn outstanding_for(&self, subject: &str) -> Vec<u64> {
        self.open
            .iter()
            .filter(|(_, p)| p.subject.as_deref() == Some(subject))
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn status(&self, id: u64) -> Settled {
        if self.settled.contains(&id) {
            Settled::Visible
        } else if self.never.contains(&id) {
            Settled::NeverVisible
        } else if self.lost.contains(&id) {
            Settled::Lost
        } else if self.open.contains_key(&id) {
            Settled::Pending
        } else {
            Settled::Unknown
        }
    }

    /// **A COMPLETE EXPORT IS WHAT LETS AN ABSENCE BE INFERRED**, and nothing
    /// else is. Called when an entry reaches `Complete`: every id this process
    /// opened against that subject which the export did not re-offer is now
    /// `Lost`, because the producer's per-key promise retention is what would
    /// have carried it and the export is the whole of that retention.
    pub fn lose_unoffered(&mut self, subject: Option<&str>) -> Vec<u64> {
        let gone: Vec<u64> = self
            .open
            .iter()
            .filter(|(_, p)| p.subject.as_deref() == subject)
            .map(|(id, _)| *id)
            .collect();
        for id in &gone {
            self.open.remove(id);
            self.lost.insert(*id);
        }
        gone
    }

    /// A daemon RESTART: the counter restarted, so an id from the old boot
    /// could collide with a new one and every outstanding id is `Lost`.
    pub fn boot_changed(&mut self, boot_id: &str) -> Vec<u64> {
        let gone: Vec<u64> = self
            .open
            .iter()
            .filter(|(_, p)| p.boot_id != boot_id)
            .map(|(id, _)| *id)
            .collect();
        for id in &gone {
            self.open.remove(id);
            self.lost.insert(*id);
        }
        gone
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  Held
// ──────────────────────────────────────────────────────────────────────────

/// **EVERYTHING THIS PROCESS HOLDS.** One mirror per shape in the book, the two
/// books, and the one output position.
pub struct Held {
    pub link: LinkState,
    pub mirrors: BTreeMap<&'static str, Mirror>,
    pub book: Book,
    pub promises: Promises,
    /// Reject code → count. Nothing branches on this; it is the log line that
    /// says a producer sent something this build does not understand, which is
    /// the one thing `Mirror::accept`'s leniency would otherwise hide.
    pub rejections: BTreeMap<String, u64>,
    /// See the module doc: the ONE position, replacing four.
    pub emitted: BTreeMap<String, u64>,
    /// Which `shapes/list` this connection learned. Cleared by [`Self::reset`]
    /// so a reconnect to a different build re-learns rather than carrying a
    /// ceiling the new daemon never published.
    pub learned: bool,
}

impl Default for Held {
    fn default() -> Self {
        Self::new()
    }
}

impl Held {
    /// **A SUBSCRIBE THIS FACE SENT WAS REFUSED** — put the refusal on the entry
    /// that asked, and go shape-wide only for the two refusals that are about
    /// the SHAPE rather than the key.
    ///
    /// * `dead`: the fold died, so every key under it is last-known, not current.
    /// * `version_refused` (R2.S8): rule 2.6's third state. The producer will
    ///   not serve the contract this build states for the shape, so nothing it
    ///   holds will be current again on this link — the mirror goes TERMINAL
    ///   through [`Mirror::refused_version`], carrying the version this seam
    ///   stated and the versions the producer serves (off the contract it
    ///   learned from `shapes/list`), so a face reports numbers and not the word
    ///   "mismatch". Until R2.S8 this code landed on the entry only, and
    ///   `refused_version` was unreachable from the seam.
    pub fn refuse(&mut self, shape: &str, key: Option<&str>, r: &shapewire::envelope::Refusal) {
        use shapewire::envelope::codes;
        if let Some(e) = self.book.get_mut(shape, key) {
            e.state = EntryState::Refused {
                code: r.code.to_string(),
                message: r.message.clone(),
            };
        }
        let Some(m) = self.mirrors.get_mut(shape) else {
            return;
        };
        if r.code == codes::DEAD {
            m.refused(r.message.clone());
        } else if r.code == codes::VERSION_REFUSED {
            let wanted = super::contract_version(shape);
            let serves = m.contract().map(|c| c.serves.clone()).unwrap_or_default();
            m.refused_version(&shapewire::mirror::VersionMismatch {
                shape: shape.to_string(),
                wanted,
                serves,
            });
        }
    }

    pub fn new() -> Self {
        let mut mirrors = BTreeMap::new();
        for shape in [THREADS, SUMMARY, RECENT, LOG, ACCOUNTS, GAUGE_HISTORY, NON_IDLE, NARRATION, HEARTBEAT] {
            mirrors.insert(shape, Mirror::new(shape));
        }
        Self {
            link: LinkState::Connecting { attempt: 0 },
            mirrors,
            book: Book::standing(),
            promises: Promises::default(),
            rejections: BTreeMap::new(),
            emitted: BTreeMap::new(),
            learned: false,
        }
    }

    pub fn mirror(&self, shape: &str) -> Option<&Mirror> {
        self.mirrors.get(shape)
    }

    /// Apply one `type: shape` frame. **The whole delivery contract** — route by
    /// `shape`, hand to [`Mirror::accept`], then move the two books on what the
    /// [`Receipt`] says. Nothing else in this process touches a frame.
    pub fn fold(&mut self, frame: &Value) -> Receipt {
        let shape = frame
            .get("shape")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let key = frame
            .get("key")
            .and_then(Value::as_str)
            .map(str::to_string);

        let Some(m) = self.mirrors.get_mut(shape.as_str()) else {
            // A frame for a shape this face does not mirror. Counted, not
            // dropped silently: a producer that started emitting something new
            // should be visible in a log line.
            *self
                .rejections
                .entry(shapewire::wire::reject::NOT_MINE.to_string())
                .or_insert(0) += 1;
            return Receipt::default();
        };
        let receipt = m.accept(frame);

        for r in &receipt.rejected {
            *self.rejections.entry(r.reason.clone()).or_insert(0) += 1;
        }

        // Rule 2.7's stages, read off the receipt and written onto the entry.
        if !receipt.subscribes.is_empty() {
            let complete = receipt.complete;
            for id in &receipt.subscribes {
                let id = *id;
                if let Some(e) = self.book.get_mut(&shape, key.as_deref()) {
                    e.state = match complete {
                        Some(true) => EntryState::Complete { subscribe_id: id },
                        Some(false) => EntryState::Short { subscribe_id: id },
                        None => EntryState::Open { subscribe_id: id },
                    };
                }
            }
        }

        for id in &receipt.promises {
            self.promises.settle(*id);
        }

        receipt
    }

    /// **THE RECONNECT, AND IT IS THE WHOLE OF IT.**
    ///
    /// Every mirror resets (its learned ceilings and parent survive; the rows do
    /// not), every book entry goes back to `Asked`, and the link is published
    /// down. There is no cursor to rewind, no `boot_id` branch, no parked-frame
    /// buffer and nothing to re-request: the next handshake re-subscribes the
    /// book and the export is complete by construction.
    ///
    /// **`emitted` deliberately SURVIVES.** It is not mirror state — it is this
    /// face's record of what it already said out loud, and forgetting it would
    /// make a dropped socket cost the operator a repeated paragraph. That is the
    /// asymmetry the old `claim_delivery` was built around, kept.
    pub fn reset(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        for m in self.mirrors.values_mut() {
            m.reset();
        }
        self.book.reset();
        self.learned = false;
        self.link = LinkState::Down { reason };
    }

    /// `shapes/list` arrived. Teaches every mirror its ceilings and parent, and
    /// marks any book entry whose shape the daemon did not name `Unsupported`.
    pub fn learn(&mut self, infos: &[ShapeInfo]) {
        let served: BTreeSet<&str> = infos.iter().map(|i| i.name.as_str()).collect();
        for info in infos {
            if let Some(m) = self.mirrors.get_mut(info.name.as_str()) {
                m.learn(info);
            }
        }
        for e in &mut self.book.entries {
            if !served.contains(e.shape) {
                e.state = EntryState::Unsupported {
                    note: format!("this daemon does not serve `{}`", e.shape),
                };
            }
        }
        self.learned = true;
    }

    /// **`shapes/list` COULD NOT BE READ**, so nothing was learned and nothing
    /// will be subscribed on this connection. Every entry is marked with the
    /// reason, under `EntryState::Unsupported` — the state a face already draws
    /// for *not served on this daemon* — but with a note that says what actually
    /// happened, which is a fact about THIS BUILD and not about the daemon.
    /// `learned` stays `false`: a list that did not parse was not learned.
    pub fn unreadable(&mut self, why: &str) {
        for e in &mut self.book.entries {
            e.state = EntryState::Unsupported {
                note: format!("not subscribed: {why}"),
            };
        }
        self.learned = false;
    }

    /// The key-level question, from the mirror and the book together.
    pub fn holding(&self, shape: &str, key: &str) -> Holding {
        let Some(e) = self.book.get(shape, Some(key)) else {
            return Holding::NotAsked;
        };
        if let EntryState::Refused { code, message } = &e.state {
            return Holding::Refused {
                code: code.clone(),
                message: message.clone(),
            };
        }
        match self.mirrors.get(shape).map(|m| m.key_state(key)) {
            Some(KeyState::Held) => Holding::Held,
            Some(KeyState::Gone) => Holding::Gone,
            Some(KeyState::Absent) => Holding::Absent,
            _ => Holding::Unknown,
        }
    }

    /// Where one subscribe stands, straight off the mirror.
    pub fn subscribe_status(&self, shape: &str, id: u64) -> SubscribeStatus {
        self.mirrors
            .get(shape)
            .map(|m| m.subscribe(id))
            .unwrap_or(SubscribeStatus::Unknown)
    }

    /// Whether a child's state and the pool's state were built in the same
    /// parent era. The only thing `parent_epoch` exists for.
    pub fn era(&self, child: &str) -> Era {
        let Some(c) = self.mirrors.get(child) else {
            return Era::NoParent;
        };
        let Some(p) = self.mirrors.get(THREADS) else {
            return Era::NoParent;
        };
        c.era(p)
    }
}
