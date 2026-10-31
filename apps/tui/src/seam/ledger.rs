//! `ledger.rs` — **A THREAD'S LIVE EVENT LOG, AND IT IS A MIRROR NOW.**
//!
//! ## The decision this file is, stated once
//!
//! `docs/consumer-seam.md` §2.0 and §8 specified this file as the seam's one
//! INTERIM component — *"the hand-rolled mirror, fenced off so it can be
//! deleted in one commit"*, *"INTERIM (§8 Gap A): the hand-rolled tail of one
//! thread's event log. Deleted when a log shape lands."* It was written that
//! way because at the time the chassis had no shape for a thread's live log and
//! both faces had to keep a legacy firehose arm plus a cursor to get at it.
//!
//! **`threads/log` landed, and it is in production, so the interim is not
//! built.** The choice was between shipping the hand-rolled tailer the design
//! specified — born with a `DELETE ME` header — and making this file the
//! `threads/log` mirror and never shipping the interim at all. The second, for
//! three reasons and one piece of evidence:
//!
//! 1. **A file whose header says "delete me" is a file somebody preserves.**
//!    The interim's deletion was scheduled for migration step M7, after the
//!    whole adoption; every line of it would have been load-bearing in two
//!    faces for the length of that schedule, and `docs/consumer-seam.md` §9 R1
//!    says so itself: *"If a log shape is not built, `ledger.rs` is permanent,
//!    the firehose cannot be retired, and the seam's reconnect has a cursor in
//!    it forever — which is the apparatus the brief hoped would die."*
//! 2. **The interim could not be conformance-tested.** The kit in §7 replays
//!    producer-generated transcripts through [`Held::fold`]. A hand-rolled
//!    tailer folds legacy `event` frames outside the mirror, so the one
//!    component with the most state would have been the one component the
//!    golden transcripts could not reach.
//! 3. **The interim is the only thing that kept a cursor in the seam.** With
//!    `threads/log` there is no `after_seq` anywhere in these five files, which
//!    is what makes rule 2.5 checkable rather than aspirational.
//!
//! The evidence is `seam/probe/`, run against the live daemon on 2026-10-07:
//! two separate connections, 4,000-row windows over tails of 175,858 and
//! 202,707 events, **identical on every axis**. A face whose own buffer was
//! 4,000 deep can delete that buffer and read this mirror, measured in
//! production rather than argued.
//!
//! **Gap A is closed.** `threads/log` is a bounded `Sequence` window of raw
//! ledger rows, 4,000 per thread, every kind including the `BULK_KINDS` the
//! `threads` shape discards, and eight open windows per connection.
//!
//! So this file is not a mirror at all. It is a READER over
//! `held.mirrors["threads/log"]`, and the firehose arm the design budgeted for
//! does not exist. What that deleted, named so the absence is checkable:
//!
//! | gone | why there is nothing to replace it with |
//! |---|---|
//! | `backfill` | the export IS the backfill, and it arrives without being asked for |
//! | `fold_catch_up` / `fold_seed_and_catch_up` | there is no gap: *hold the export, apply everything after it* |
//! | `watch_turn`'s `Reconnected` arm | a re-subscribe's mirror is identical to one that never disconnected (rule 2.5) |
//! | the `Lagged` arms | a dropped wake-up costs nothing; see `link.rs`'s `SeamEvent` |
//! | `SUBSCRIBE_FRAME` (`{"type":"subscribe","threads":null}`) | not sent. The seam speaks no legacy frame at all |
//!
//! ## The two positions, and why there are two
//!
//! A mirror is state; a turn watcher is a state machine over an event ORDER. So
//! something must turn the window into *the rows after the ones I have seen*,
//! and there are two distinct questions:
//!
//! 1. **What has THIS WATCHER processed?** `TurnWatcher::last_seq`, per watcher,
//!    unchanged. Every watcher sees every row — a coalesced turn
//!    (`turn_started` carrying `message_ids: [47, 49, 50]`) has one watcher per
//!    queued message and each of them needs the `turn_started`/`turn_ended`
//!    edges to reach its own verdict.
//! 2. **What has the OPERATOR already heard?** [`claim`], over `Held::emitted`,
//!    process-wide and per THREAD. This is the sibling-watcher dedup that
//!    stopped one paragraph being spoken three times (2026-08-03, daemon seq
//!    12607, local log idx 291/292/293 written 4 ms apart). It is a property of
//!    the AUI's turn path — several watchers, one turn — and NOT of the
//!    firehose, so adopting shapes does not retire it.
//!
//! **What adopting shapes DID retire is its two repair halves.** The durable
//! copy of that mark (`delivered_seq`, persisted into `~/.aui/outstanding`) and
//! the query that seeded it (`seed_delivered_from_status`, reading
//! `last_seen_seq` off a `threads/status` payload) are both gone, because the
//! producer has held that cursor all along — `thread_seen`, advanced by the
//! `threads/{id}/seen` command AUI already sends — and it arrives on the
//! `threads` export as `seen_seq`. [`floor`] reads it. One copy of the fact, on
//! the side that owns it.

use std::collections::BTreeMap;

use serde_json::Value;
use shapewire::wire::Timestamp;

use super::held::{Held, LOG, LOG_WINDOWS, THREADS};
use super::view;

/// One row of the window, as an event consumer above the seam wants it. The
/// field names are the ledger's own — `seq`, `message_id`, `ts`, `kind` — so a
/// row read here and a row that used to arrive on an `event` frame are the same
/// object and `classify_ledger_event`/`TurnWatcher::on_event` did not change.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub seq: u64,
    pub message_id: Option<u64>,
    pub kind: String,
    pub ts: Option<String>,
    /// **The event, held exactly as `threads/{id}/history`'s `event` field holds
    /// it.** `Payload::Here` is the whole of it on the frame, which is what makes
    /// live rendering possible: a `stream_event` delta is ~150 bytes and a fetch
    /// per delta would be 125 round trips a message, strictly worse than the
    /// firehose this shape retires.
    pub event: Value,
    /// `true` when the producer put this row's bytes behind the resource door
    /// instead of on the frame (`Payload::Split` — a message whose text is
    /// large — or `Payload::There`). The face fetches through
    /// `view::body_plan`/`Mirror::read`; it never reconstructs the text.
    pub elided: bool,
}

/// Turn one window item into a [`Row`]. The `payload` tag is the whole of what a
/// consumer branches on; the three spellings are the producer's and are read
/// here and nowhere else.
fn row_of(item: &Value) -> Option<Row> {
    let seq = item.get("seq").and_then(Value::as_u64)?;
    let payload = item.get("payload")?;
    let (event, elided) = if let Some(v) = payload.get("here") {
        (v.clone(), false)
    } else if let Some(s) = payload.get("split") {
        (s.get("event").cloned().unwrap_or(Value::Null), true)
    } else {
        // `there`, or a FOURTH tag this build has never met. Either way the
        // envelope is all there is, and saying so is better than guessing: the
        // producer publishes `payload_modes` on `shared` precisely so a
        // consumer that meets a fourth knows it is behind a version.
        (Value::Null, true)
    };
    Some(Row {
        seq,
        message_id: item.get("message_id").and_then(Value::as_u64),
        // Declared `Required` on `threads/log.events`: a row with no kind is not
        // a row this consumer can classify, and an empty kind would be read as
        // one it does not know rather than as a malformed row.
        kind: item.get("kind").and_then(Value::as_str)?.to_string(),
        ts: item.get("ts").and_then(Value::as_str).map(str::to_string),
        event,
        elided,
    })
}

/// **EVERY ROW THE WINDOW HOLDS FOR ONE THREAD, OLDEST FIRST, AFTER `after`.**
///
/// There is no request in this function and no door it could ask through. The
/// rows are already here.
pub fn rows_after(h: &Held, thread: &str, after: u64) -> Vec<Row> {
    view::log_rows(h, thread)
        .known()
        .unwrap_or_default()
        .iter()
        .filter_map(row_of)
        .filter(|r| r.seq > after)
        .collect()
}

/// **THE FLOOR ON WHAT MAY BE SPOKEN, AND IT COMES FROM THE PRODUCER.**
///
/// On first sight of a thread, everything at or below the daemon's own
/// `seen_seq` has already reached the operator — that is what the cursor means
/// and the AUI is the thing that advances it. So a fresh process folds the
/// window and speaks only what is above it, with no state of its own to
/// persist and nothing to seed from a query.
///
/// `None` when the `threads` key is not held or its `newest` cell has not
/// arrived: the honest answer is *I do not know what he has heard*, and the
/// caller must not speak on that.
pub fn floor(h: &Held, thread: &str) -> Option<u64> {
    view::positions(h, thread).known().map(|p| p.seen_seq)
}

/// **Claim the right to hand thread `thread`'s row `seq` to the operator.**
/// `true` exactly once per `(thread, seq)` in this process, whichever watcher
/// asks.
///
/// `seq == 0` cannot be identified, so it is delivered rather than silently
/// swallowed and logged loudly: speaking something twice is a nuisance and
/// dropping it is a lost message, and the producer always stamps a seq.
pub fn claim(h: &mut Held, thread: &str, seq: u64) -> bool {
    if seq == 0 {
        tracing::warn!(
            thread = %thread,
            "seam: a log row carries no ledger seq — delivered without a duplicate guard"
        );
        return true;
    }
    // The producer's cursor is the floor on first sight. Computed before the
    // mutable borrow because `floor` reads the `threads` mirror.
    let seeded = h.emitted.contains_key(thread);
    let from_producer = if seeded { None } else { floor(h, thread) };
    let mark = h.emitted.entry(thread.to_string()).or_insert(0);
    if let Some(f) = from_producer {
        if f > *mark {
            *mark = f;
        }
    }
    if seq <= *mark {
        return false;
    }
    *mark = seq;
    true
}

/// What this process has handed over per thread. Read by the outstanding-turn
/// recovery so a re-attached watcher starts from his ears.
pub fn emitted(h: &Held) -> &BTreeMap<String, u64> {
    &h.emitted
}

/// The newest `ts` in the window, for a face that wants *when did anything last
/// happen on this thread* without subscribing `threads` as well.
pub fn newest_at(h: &Held, thread: &str) -> Option<Timestamp> {
    view::log_rows(h, thread)
        .known()?
        .last()
        .and_then(|r| r.get("ts").and_then(Value::as_str))
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc))
}

// ──────────────────────────────────────────────────────────────────────────
//  Which windows are open — the producer's ceiling, honoured client-side
// ──────────────────────────────────────────────────────────────────────────

/// **WHY THIS FACE CANNOT WATCH EVERY THREAD'S LOG, STATED ONCE.**
///
/// `threads/log` publishes `open_keys_per_conn: 8` and refuses a ninth with
/// `TooManyKeys` — *"`shapes.unsubscribe` a key you are no longer looking at to
/// release a slot. It is not clamped and nothing was queued"*. Under
/// `Hydrate::PerKey` a key costs a whole 4,000-row window, and the producer's
/// position is that one face is legitimately looking at one transcript plus a
/// short navigation history.
///
/// **This is a real narrowing of what the AUI sees and it is named rather than
/// hidden.** The keyless firehose carried every thread's events, so
/// `classify_ledger_event` — the 0113 fold of the daemon's ledger into the AUI's
/// own per-thread JSONL logs — ran for threads nobody had asked about, including
/// turns started from the terminal face. It now runs for the ≤8 threads this
/// face is actually transacting with: a running turn, an owed reply, the thread
/// under the cursor. A turn run entirely elsewhere on a thread the AUI never
/// opens is no longer folded live; it is folded the next time the AUI holds that
/// key, from the window, which reaches 4,000 rows back.
///
/// Widening it is a PRODUCER decision (raise the ceiling, or an `Active`-scoped
/// index), not a consumer one, and asking for nine keys is refused rather than
/// clamped so there is no way to drift into it quietly.
pub fn capacity(h: &Held) -> usize {
    h.mirror(LOG)
        .and_then(|m| m.max())
        .and_then(|max| max.open_keys_per_conn)
        .unwrap_or(LOG_WINDOWS)
}

/// Which log windows this face holds, in book order.
pub fn open(h: &Held) -> Vec<String> {
    h.book.keys_of(LOG)
}

/// The thread keys this face holds on the POOL. Separate from [`open`] because
/// `threads` has no open-key ceiling and `threads/log` does — the AUI holds
/// status for every thread in the recent band and a window for only the few it
/// is transacting with.
pub fn pooled(h: &Held) -> Vec<String> {
    h.book.keys_of(THREADS)
}
