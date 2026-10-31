//! **THE FACE'S WIRE VOCABULARY — AND NOTHING THAT SPEAKS IT.**
//!
//! # What this file was, and what is left
//!
//! It was ONE WebSocket to the threads daemon and everything that kept it
//! honest: a supervisor task, a reconnect loop, the `welcome`/`auth`/`subscribe`
//! handshake, the legacy firehose subscribe, an id-correlated RPC channel, the
//! per-thread delivered-seq cursors, the three-way `boot_id` resync ladder, the
//! `Lagged` repair and the outbound drain. Just over 1,200 lines of it, and its
//! header opened by quoting `DIR-7`: *"DO NOT rely on refresh. This should be
//! subscribed via websockets."*
//!
//! **It was also this face's own second implementation of the producer's wire**,
//! and that is why it is gone. The operator's framing:
//!
//! > *"I was originally imagining that TUI and AUI would both have the same
//! > backend and just different skins... with a copy in each one instead of a
//! > real shared backend. **Threads basically is the backend.**"*
//!
//! A hand-rolled tolerant mirror of the daemon's frames IS a backend — a second
//! one, in a second crate, drifting from the first. [`crate::seam`] replaced it
//! with a copy of the chassis's own, and [`crate::mirror`] is the ten percent
//! that is genuinely this face's. What survives HERE is the three types the face
//! renders through, and they survive unchanged on purpose: `store.rs` has **zero
//! lines changed** by the migration, which is the strongest available evidence
//! that this was a mirror put behind an existing vocabulary and not a rewrite.
//!
//! # What went, and what answers each thing it was for
//!
//! | what it did | why it is not needed |
//! |---|---|
//! | the handshake and `subscribe{threads:null}` | `link.rs` speaks the chassis door; the legacy firehose frame is sent by nothing |
//! | `Handle::call(&method, params)` | a door wide enough for a read. [`crate::acts::Verb`] is three variants and `seam::commands` is the whole of what can be said |
//! | the per-thread cursors, the repeat-over-drop bias | `seam::ledger::claim` — once per `(thread, seq)` in this process, floored by the PRODUCER's `seen_seq` rather than by anything this face persisted |
//! | the three-way `boot_id` resync ladder and its two replays | rule 2.5: a re-subscribe's export REPLACES the row. [`ResyncKind`] survives as the LABEL on a note; there is no repair left behind it |
//! | the `Lagged` repair (`threads/status` re-query + `history` replay) | the seam's wake-ups carry no facts, so a dropped one costs latency and nothing else |
//! | the outbound drain | there is no request queue to drain; a command takes the link or is refused at once |
//! | `DaemonWarnings` off `threads/status` | **nothing. See its own doc comment** — the field is not in the producer's source at all |
//!
//! # And `DIR-7` is more true than it was, not less
//!
//! There is still no poll in this crate and no refresh key. `^R` re-reads two
//! FILES and says so (`app.rs::reload`); the fleet is a mirror the producer
//! pushes, so there is nothing left for a refresh to be a crutch for.

use serde_json::Value;

use crate::proto::ServerMsg;

/// How a connect was classified. **The whole point of `boot_id` — and now the
/// whole of what is left of it.**
///
/// It used to CHOOSE A REPAIR: `Blip` replayed each cursor with
/// `threads/{id}/history{after_seq}`, and `Restart` did that plus a full fleet
/// resync, because every derived fact the daemon held had been rebuilt from
/// nothing. Rule 2.5 deletes both: a re-subscribe's export replaces the row
/// rather than merging into it, so a reconnect needs no replay to be correct
/// and a restart needs no resync to be current.
///
/// What it still is, is the WORD on the note the face draws — `link up · daemon
/// restarted` is worth more to a person watching than `link up` — and
/// `store.rs` still renders it. [`crate::mirror::Projection`] classifies it the
/// same way the ladder did, off the `boot_id` the mirror publishes on
/// `LinkState::Up`, which was always the only signal allowed to decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResyncKind {
    /// The process just started. Nothing was held, so nothing is suspect.
    First,
    /// Same `boot_id`: the socket blipped under a daemon that never moved.
    Blip,
    /// A different `boot_id`: the daemon restarted. Everything it derived is
    /// new, and every export this face now holds is the new one.
    Restart,
}

impl ResyncKind {
    pub fn label(&self) -> &'static str {
        match self {
            ResyncKind::First => "first connect",
            ResyncKind::Blip => "socket blip",
            ResyncKind::Restart => "daemon restarted",
        }
    }
}

/// How loud a note is.
///
/// Deliberately not the render crate's `Severity`: this module is allowed to
/// know a socket exists and the shell translates. Keeping the two names apart
/// is what stops a transport concern from leaking into a pure crate that is not
/// allowed to have one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteLevel {
    Calm,
    Warn,
    Critical,
}

/// **EVERYTHING THE FACE IS TOLD ABOUT THE DAEMON**, and the one vocabulary
/// `store.rs` and `app.rs` consume.
///
/// Produced by [`crate::mirror::Projection`] now, off the mirror, and by
/// nothing else. It is UNCHANGED from the version the legacy socket produced —
/// same variants, same fields, same meanings — which is what let `store.rs`
/// stay at zero lines changed.
#[derive(Debug)]
pub enum TransportEvent {
    Connecting {
        attempt: u32,
    },
    Connected {
        boot_id: String,
        resync: ResyncKind,
    },
    Disconnected {
        reason: String,
        /// **ZERO FROM THE MIRROR, AND THAT IS HONEST.** The retry ladder is
        /// the seam's (500 ms doubling to 30 s) and it does not publish the
        /// next attempt's time. `store.rs` renders `retry_at` from this, so
        /// zero means *no countdown claimed* rather than *retrying now* — which
        /// is the correct thing to show about a schedule this face cannot see.
        next_retry_in_ms: u64,
    },
    /// A ledger row for a thread, as the face has always received one.
    ///
    /// **`replayed` IS ALWAYS `false` NOW.** It marked a row that came back
    /// through `threads/{id}/history` rather than off the wire, and there is no
    /// history call left to come back through: every row is read off the
    /// producer's own `threads/log` window. The field is kept because
    /// `store.rs` takes it and ignores it (`apply_frame(msg, _replayed, …)`),
    /// and deleting a field nobody reads would edit a file this migration is
    /// claiming it did not touch.
    Frame {
        msg: ServerMsg,
        replayed: bool,
    },
    /// **EVERY CURRENT-STATE FACT THE FACE HOLDS IS NOW SUSPECT.** Sent on
    /// every reconnect, BEFORE any export lands. The face answers by putting
    /// its daemon-owned scalars back to `Known::Unknown` rather than keeping
    /// the last value: *never render stale as current*.
    ///
    /// What it no longer precedes is a REPAIR — see [`ResyncKind`].
    Invalidate {
        reason: String,
    },
    /// The fleet's STATE: one row per thread in the window, carrying only the
    /// cells the mirror actually answers. See
    /// [`crate::mirror::status_row`] for why an omitted field is the whole
    /// mechanism by which an un-hydrated cell renders as `?`.
    ///
    /// `capped` means there is more fleet than this: the producer's own
    /// `threads/summary` `truncated`, rather than a page size this face chose
    /// and then rendered as a fleet size.
    Fleet {
        statuses: Vec<Value>,
        capped: bool,
    },
    /// The fleet's IDENTITY — `cwd`, `model`, `account`, `updated_at`. `capped`
    /// as above.
    Threads {
        rows: Vec<Value>,
        capped: bool,
    },
    Note {
        text: String,
        level: NoteLevel,
    },
    /// **THE DAEMON'S COUNT OF ITS OWN WARNINGS — AND NOTHING SENDS THIS.**
    ///
    /// It was read off a `daemon_warnings` object on the `threads/status`
    /// reply. That field does not exist: `grep -r daemon_warnings` over
    /// `substrate-interface-layer` at `6bb96b1a` finds it in no shape, no
    /// handler and no test, so no producer on this wire has ever sent one.
    ///
    /// The old reader was written correctly — *"ABSENT IS NOT ZERO: a daemon
    /// without the field sends no event at all, and the store's
    /// `Known::Unknown` stands"* — so it was SILENT rather than wrong, which is
    /// why nobody noticed. The header segment it feeds has therefore always
    /// read `?`, and it still does.
    ///
    /// Kept rather than deleted because `store.rs` holds the two counters and
    /// `app.rs` draws them, and removing them would be this migration editing a
    /// rendering path it is claiming not to have touched. **It is a producer
    /// ask, not a consumer bug:** a `warnings` cell on `threads/summary` would
    /// light it up with no code here beyond one line of projection.
    DaemonWarnings {
        warn: u32,
        error: u32,
    },
    /// **THE FIRST LINE OF A THREAD'S FIRST MESSAGE** (TUI-P1a) — read once off
    /// the thread's first message page through the resource door, for a thread
    /// in the window that has no name. It is what an unnamed thread is called,
    /// in place of the project path it used to fall back to.
    FirstLine {
        thread_id: String,
        line: String,
    },
}
