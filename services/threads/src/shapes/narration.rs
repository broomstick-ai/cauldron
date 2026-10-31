//! # The `narration` shape — the daemon talking while it works
//!
//! **This is the shape that lets a consumer stop subscribing to the legacy
//! firehose**, and that is the only reason it exists. `Push::Progress`,
//! `Push::Lifecycle` and `Push::Info` have no other home: they belong to no
//! thread and no account, no RPC returns them, and `accounts` and `threads`
//! both decline them by name. Operator ruling, in
//! `docs/consumer-migration-ledger.md`:
//!
//! > *"Every Push variant gets a shape home, `Liveness` included, because a
//! > variant with no home is what keeps the legacy firehose alive."*
//!
//! So this file is the home of three of the six, and the sixth is dealt with
//! below.
//!
//! ## Keys: three CHANNELS, and why this is keyed at all
//!
//! `docs/shapes.md` §7.3 and `docs/thread-shapes-plan.md` §3.3 both specify
//! narration as KEYLESS — `Shape::KEYED = false`, one shared `Sequence<Line>`.
//! **The code contradicted that, the code won, and `Shape::KEYED` HAS NOW BEEN
//! DELETED (2026-09-28) on the strength of this paragraph.** The const was
//! carried onto `shapes.list` and changed nothing else:
//! `ShapeTask::do_subscribe` answers a keyless subscribe by iterating
//! `Root::keys()`, and `export` with `key: None` builds a frame whose `row` and
//! `cells` are both `None` by construction. A genuinely keyless shape would
//! therefore have been acked with `{queued: true}` and then handed nothing —
//! the ack-then-silence class the `KeyRequired` refusal and the death export
//! were both built to close. A declaration nothing can honour is not a
//! declaration.
//!
//! So narration is KEYED, and its keys are the three channels — `progress`,
//! `lifecycle`, `info` — which buys three things beyond merely working:
//!
//! * a consumer that only wants restart notices subscribes to `lifecycle` and
//!   is charged for nothing else;
//! * the three windows evict independently, so a burst of `info` cannot push
//!   the going-down notice out of a shared 64-item window;
//! * `HYDRATE` is honestly `Whole` — the three channels are read at init and
//!   there is never a fourth, so a keyless subscribe is a real inventory and a
//!   keyed subscribe to `"progress"` is a real selection. There is nothing
//!   per-key about a fixed set of three.
//!
//! ## One cell per channel, and it is a `Sequence`
//!
//! `lines: Sequence<Line>{capacity 64, window 1 h}` — `docs/shapes.md` §7.3's
//! bounds, unchanged. `docs/shapes.md` §4.2 step 5: "the last N within D".
//! Narration is TRANSITIONS by definition (a line is an occurrence; two lines
//! in the wrong order are two different stories), so rule 2.2 routes every
//! append down the serial, un-coalesced path with no dial to choose.
//!
//! **Every cell here is `since_init` and the export says so.** There is no
//! hydrate request in this file at all, and that is honest rather than lazy:
//! these frames are transient by construction — `Push::Progress` "replaces a
//! client that tailed a log file", `LifecycleNote` is "the doorbell, not the
//! ledger" — so there is nothing durable to read. A consumer that has been
//! connected longer than the window knows more than a re-subscribe can give it
//! back, and `covers_since` on the export is what says so.
//!
//! ## `Push::Liveness` is NOT here, and that is a decision rather than a gap
//!
//! Liveness's home is [`crate::shapes::threads`]' `liveness` cell — a LEVEL on
//! the thread the beat is about. Three reasons it is not a narration line:
//!
//! 1. **It belongs to a thread.** Everything in this shape belongs to no thread,
//!    which is exactly why these three variants had nowhere to go; the beat has
//!    somewhere to go.
//! 2. **It is a level, not an occurrence.** "This turn is still working" is the
//!    current value of something, and the fold there drops a beat that repeats
//!    the held level.
//! 3. **It would empty the window.** Three running threads beat 3×/s; a 64-item
//!    window fed those holds twenty-one seconds of beats and nothing else. The
//!    restart notice this shape exists to carry would be evicted by noise.
//!
//! **"Is the daemon alive" is not what the beat answers either**, and it is
//! worth saying where a reader will look for it: beats exist only while a thread
//! is running, so silence means "nothing is running", not "the daemon is gone".
//! Transport liveness is the websocket ping, and IDENTITY across a restart is
//! `boot_id` on the welcome frame — `docs/shapes.md` §0 measured that the execv
//! self-update keeps the pid, so `boot_id` is the only restart signal. This
//! shape's `lifecycle` channel is the same boot's own account of why it went
//! down and came back, which is the part a consumer could not otherwise get.
//!
//! ## Version
//!
//! [`Narration::VERSION`] is `1`, published on `shared` as `shape_version`. See
//! [`crate::shapes::threads`]' module doc for what declaring the number does and
//! does not buy — `SERVES` and the link-time refusal are not built.

use serde::Serialize;
use serde_json::{json, Value};

use crate::event::{Fact, Push};
use shapewire::cell::Cells;
use shapewire::declare_cells;
use shapewire::operation::{
    DType::{Enum, List, Ref, Scalar as Sc, Struct, Timestamp as Ts},
    Presence::{Optional, Required},
    Scalar::{Bool, Int, Str},
    TimeEncoding::{Rfc3339},
};
use crate::shape::{
    Hydrate, Input, Map, Register, Root, Sequence, Shape, Stamped, Timestamp, Tracking, Tx,
};

/// The `progress` channel: a long account operation talking while it runs.
pub const CHANNEL_PROGRESS: &str = "progress";
/// The `lifecycle` channel: a service going down, or coming back.
pub const CHANNEL_LIFECYCLE: &str = "lifecycle";
/// The `info` channel: "something you are displaying is no longer what you are
/// displaying".
pub const CHANNEL_INFO: &str = "info";

/// The three, in export order. A fixed set, which is what makes `KeyScope::Fleet`
/// the honest declaration.
pub const CHANNELS: &[&str] = &[CHANNEL_INFO, CHANNEL_LIFECYCLE, CHANNEL_PROGRESS];

/// **How many lines a channel keeps, and for how long.** `docs/shapes.md` §7.3's
/// numbers. Both bounds, always: `capacity` stops a burst from growing the
/// export and `window` stops an idle daemon exporting last week's news as
/// current.
pub const LINE_CAPACITY: usize = 64;

/// One narration line.
///
/// **Internally tagged on `kind`**, like `ThreadEvent` on the legacy wire, so a
/// consumer switches on one field and a channel's payload keeps its own field
/// names. Each variant is the corresponding `Push` variant's fields VERBATIM —
/// this shape re-words nothing, because a narration line that paraphrased its
/// frame would be a second vocabulary for one fact.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Line {
    Progress {
        op: String,
        id: String,
        account: Option<String>,
        phase: String,
        detail: Option<String>,
        done: bool,
        ok: Option<bool>,
    },
    /// The whole `LifecycleNote`, flattened, including the `boot_id` a consumer
    /// correlates against its own `welcome`.
    Lifecycle {
        service: String,
        phase: String,
        reason: String,
        detail: String,
        boot_id: String,
        at: String,
    },
    Info {
        domain: String,
        reason: String,
        account: Option<String>,
        detail: Option<String>,
        at: String,
    },
}

/// One channel: its window, and nothing else.
#[derive(Debug)]
pub struct Channel {
    pub lines: Sequence<Line>,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            lines: Sequence::new(LINE_CAPACITY, chrono::Duration::hours(1)),
        }
    }
}

pub struct NarrationRoot {
    pub channels: Map<String, Channel>,
    pub shape_version: Register<u32>,
    shared_dirty: bool,
}

impl Default for NarrationRoot {
    fn default() -> Self {
        Self {
            channels: Map::new(),
            shape_version: Register::new(),
            shared_dirty: false,
        }
    }
}

impl Root for NarrationRoot {
    fn keys(&self) -> Vec<String> {
        self.channels.keys().cloned().collect()
    }

    fn row(&self, key: &str) -> Option<Value> {
        self.channels.get(&key.to_string()).map(|c| {
            json!({
                "channel": key,
                "lines": c.lines.to_json(),
            })
        })
    }

    fn cells(&self, key: &str) -> Option<Value> {
        self.channels
            .get(&key.to_string())
            .map(|c| json!({ "lines": c.lines.state() }))
    }

    fn shared(&self) -> Value {
        json!({
            "shape_version": self.shape_version.now(),
            "channels": CHANNELS,
            "line_capacity": LINE_CAPACITY,
            "window_hours": 1,
        })
    }

    fn shared_cells(&self) -> Value {
        json!({ "shape_version": self.shape_version.state() })
    }

    fn drain(&mut self) -> crate::shape::Outbox {
        let dirty = self.channels.take_dirty_keys();
        let gone = self.channels.take_removed();
        // The three channels are minted at init and never again, so this is
        // empty on every live fold — carried anyway, because "never" is a
        // property of this shape's fold and not of the mechanism.
        let added = self.channels.take_added();
        let mut transitions = Vec::new();
        let mut rows = Vec::new();
        for k in dirty {
            let Some(c) = self.channels.get_mut(&k) else {
                continue;
            };
            for item in c.lines.take_pending() {
                transitions.push((k.clone(), "lines", item));
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
        self.shape_version.mark_dead(reason);
        self.channels.mark_dead(reason);
        for (_, c) in self.channels.entries_mut() {
            c.lines.mark_dead(reason);
        }
    }
}

pub struct Narration;

impl Narration {
    /// See [`crate::shapes::threads`]' module doc for what a declared version is
    /// and is not.
    pub const VERSION: u32 = 1;
}

declare_cells! {
    /// Per-key cells of `narration`: one channel's window of lines.
    ///
    /// A `Line` is internally tagged on `kind`, so the declaration is the tag's
    /// closed words plus the UNION of every variant's fields, each `Optional` —
    /// the manifest goes one level deep and cannot say which fields go with
    /// which kind (see `shapewire::cell`, "A tagged union"). `at` is on every
    /// line: two variants carry their own and the chassis stamps the third.
    /// **There is no `text` field**, and there never was: a line is its frame's
    /// fields verbatim, and the SENTENCE is the face's (the audio law).
    pub NARRATION_CELLS = [
        ("lines", List(&Struct("Line")), Required, [
            ("kind", Enum(&["progress", "lifecycle", "info"]), Required),
            ("at", Ts(Rfc3339), Required),
            ("op", Sc(Str), Optional),
            ("id", Sc(Str), Optional),
            ("account", Ref("account"), Optional),
            ("phase", Sc(Str), Optional),
            ("detail", Sc(Str), Optional),
            ("done", Sc(Bool), Optional),
            ("ok", Sc(Bool), Optional),
            ("service", Sc(Str), Optional),
            ("reason", Sc(Str), Optional),
            ("boot_id", Sc(Str), Optional),
            ("domain", Sc(Str), Optional),
        ]),
    ]
}

declare_cells! {
    /// Shape-level cells of `narration`.
    pub NARRATION_SHARED = [
        ("shape_version", Sc(Int), Required, []),
    ]
}

impl Shape<Input> for Narration {
    const NAME: &'static str = "narration";
    const CELLS: Cells = NARRATION_CELLS;
    const SHARED: Cells = NARRATION_SHARED;
    /// A line's `account` is a key of `accounts`.
    const REFERS: &'static [(&'static str, &'static str)] = &[("account", "accounts")];
    const TRACKING: Tracking = Tracking::Lazy;
    /// **`Fleet`, and it is not a default.** The three channels are the whole
    /// universe of channels this daemon narrates on, so "every key I know about"
    /// and "every key there is" are the same three — which is precisely what
    /// `Fleet` claims. A keyless subscribe here costs three exports of a window
    /// each, and that is an inventory rather than a fleet dump.
    const HYDRATE: Hydrate = Hydrate::Whole;
    /// One input appends to exactly one channel: one `transition` plus one `row`.
    /// Anything above that means the fold started touching channels it did not
    /// write.
    const BURST: usize = 4;

    type Producer = crate::runtime::ThreadManager;
    type Root = NarrationRoot;

    fn root(&self) -> NarrationRoot {
        NarrationRoot::default()
    }

    /// **THE ONLY SHAPE ON THIS PRODUCER THAT MAKES NO HYDRATE REQUEST.** There
    /// is nothing to read: all three inputs are transient by construction and
    /// none of them is persisted anywhere. So init creates the three channels and
    /// marks each `since_init` at `as_of`, which is the export's own statement
    /// that it covers only this epoch.
    fn hydrates(
        &mut self,
        _p: &crate::runtime::ThreadManager,
        root: &mut NarrationRoot,
        as_of: Timestamp,
    ) {
        if root.shape_version.now() != Some(&Narration::VERSION) {
            root.shape_version.emit(as_of, Narration::VERSION);
            root.shared_dirty = true;
        }
        for c in CHANNELS {
            root.channels.key((*c).to_string()).lines.mark_since_init(as_of);
        }
        root.channels.mark_read(as_of, true);
        // A hydrate publishes nothing — the export IS the publication.
        let _ = root.drain();
    }

    /// **EXHAUSTIVE, WITH NO `_` ARM.** Three arms append; everything else has a
    /// home elsewhere and says which.
    fn folds(&mut self, input: &Stamped<Input>, tx: &mut Tx<'_, NarrationRoot>) {
        let at = tx.at();
        match &input.value {
            Input::Push(Push::Progress {
                op,
                id,
                account,
                phase,
                detail,
                done,
                ok,
            }) => {
                tx.channels.key(CHANNEL_PROGRESS.to_string()).lines.emit(
                    at,
                    Line::Progress {
                        op: op.clone(),
                        id: id.clone(),
                        account: account.clone(),
                        phase: phase.clone(),
                        detail: detail.clone(),
                        done: *done,
                        ok: *ok,
                    },
                );
            }
            Input::Push(Push::Lifecycle(note)) => {
                tx.channels.key(CHANNEL_LIFECYCLE.to_string()).lines.emit(
                    at,
                    Line::Lifecycle {
                        service: note.service.clone(),
                        phase: note.phase.clone(),
                        reason: note.reason.clone(),
                        detail: note.detail.clone(),
                        boot_id: note.boot_id.clone(),
                        at: note.at.clone(),
                    },
                );
            }
            Input::Push(Push::Info {
                domain,
                reason,
                account,
                detail,
                at: stamped_at,
            }) => {
                tx.channels.key(CHANNEL_INFO.to_string()).lines.emit(
                    at,
                    Line::Info {
                        domain: domain.clone(),
                        reason: reason.clone(),
                        account: account.clone(),
                        detail: detail.clone(),
                        at: stamped_at.clone(),
                    },
                );
            }

            // ── homed elsewhere, every one of them ───────────────────────
            //
            // `Event` and `Status` are `crate::shapes::threads`'; `Liveness` is
            // too, as a LEVEL on the thread it is about — the module doc carries
            // the three reasons it is not a line here. Every `Fact` is an
            // account fact and belongs to `crate::shapes::accounts`, except the
            // five thread facts, which are `threads`'.
            Input::Push(Push::Event(_))
            | Input::Push(Push::Status { .. })
            | Input::Push(Push::Liveness { .. })
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
            | Input::Fact(Fact::ThreadHeader { .. })
            | Input::Fact(Fact::ThreadSettings { .. })
            | Input::Fact(Fact::ThreadPlacement { .. })
            | Input::Fact(Fact::ThreadSeen { .. })
            | Input::Fact(Fact::ThreadCompaction { .. })
            // The queue is a LEVEL a consumer reads off `threads`, not a line
            // the daemon says while it works.
            | Input::Fact(Fact::ThreadQueue { .. })
            | Input::Fact(Fact::ThreadChildren { .. })
            | Input::Fact(Fact::ThreadRolled { .. })
            | Input::Fact(Fact::ThreadSubagents { .. })
            | Input::Fact(Fact::ThreadChildActivity { .. })
            // The link beat is `crate::shapes::heartbeat`'s. It is emphatically
            // NOT a narration line: it belongs to no channel, it would empty a
            // 64-item window in 64 seconds, and — the reason that matters — a
            // line is an OCCURRENCE worth reading, while a beat's entire content
            // is that it arrived.
            | Input::Fact(Fact::Beat { .. })
            // The producer's own warning count is `crate::shapes::warnings`'
            // business and nothing else's.
            | Input::Fact(Fact::WarningsSampled { .. }) => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// NO `Handles` IMPL EXISTS FOR THIS SHAPE, DELIBERATELY
// ─────────────────────────────────────────────────────────────────────────────
//
// `docs/shapes.md` §4.4's first compile-time guarantee — "the producer answers
// every request the shape makes" — is vacuous here, because the shape makes
// none. Stated rather than left to be noticed: the guarantee is not weakened,
// there is simply nothing for it to check, and a future narration cell with a
// durable source behind it would acquire a request and the bound with it.
//
// It is also why this shape's hydrate cost is ZERO statements and its first
// export is three windows of whatever this epoch has heard.
