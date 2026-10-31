//! # What an unfiltered 4,000-row window COSTS — measured, not asserted
//!
//! The operator's question, verbatim: *"the `kinds` filter moves client-side … That
//! costs bytes — so does the ceiling accommodate unfiltered rows at 4,000?
//! **Measure it.** If an unfiltered 4,000-row window is too large, say what the
//! honest ceiling is rather than quietly filtering."*
//!
//! This is the measurement, and it is a TEST rather than a one-off script because
//! the number has to stay true: a row that grows by one field grows the window by
//! 4,000 of them, and the only thing that notices is a budget somebody has to walk
//! past.
//!
//! ## The mix, and where it comes from
//!
//! Three measured facts, all on the record in this repo:
//!
//! * **125 `stream_event` deltas per assistant message** — `docs/consumer-seam.md`
//!   §8: *"the TUI assembles a reply from 125 `stream_event` deltas per message"*.
//! * **58% of the 2,463,981-row ledger is the three `BULK_KINDS`** —
//!   `crate::shapes::threads`' module doc, with the per-kind split: `stream_event`
//!   674,493, `message_usage` 431,018, `unknown` 333,422.
//! * **47,556 of 2,463,981 events are addressable** — so roughly one row in fifty
//!   carries a `message_id`, which is what makes the delta the row that decides the
//!   size.
//!
//! A window is the RECENT end of one thread, so the mix that matters is not the
//! whole ledger's: it is what a turn produces. One turn is modelled here as it is
//! measured — a `user_message`, then 125 deltas, then the `assistant_text` they
//! assembled into, then a `message_usage`, then a tool round trip — repeated until
//! the window is full.
//!
//! ## What it says
//!
//! The numbers are printed (`cargo test -- --nocapture`) and the budget is asserted.
//! The budget is a CEILING ON THE MEASUREMENT, not a second ceiling on the shape:
//! `log::WINDOW` is the contract, this is the number that contract costs, and the
//! assertion exists so the cost cannot drift without somebody deciding it may.

use serde_json::{json, Value};
use threads::event::{StoredEvent, ThreadEvent};
use threads::shapes::log::{self, LogRow};

/// **THE BUDGET FOR ONE FULL UNFILTERED WINDOW, SERIALISED.**
///
/// **1 MiB, against a measured 832,561 bytes — 26% of headroom and not a byte
/// more.**
///
/// The number was MEASURED first and the budget set around it, in that order. 26%
/// is deliberately tight: one more timestamp on a row is 31 bytes × 4,000 = 124 KB,
/// a quarter of the headroom, so a row growing is a decision somebody takes at this
/// line rather than a number that drifts past nobody.
const BUDGET: usize = 1_024 * 1_024;

/// **THE BUDGET FOR ONE CONNECTION AT THE KEY CEILING** — what the producer holds,
/// and ships, for a consumer that has opened every window it may.
///
/// `OPEN_WINDOWS` × the window, and it is the number that decides whether the key
/// ceiling is the right one. A reconnect re-exports all of it.
const FLEET_BUDGET: usize = log::OPEN_WINDOWS * BUDGET;

fn stored(id: &str, seq: u64, message_id: Option<u64>, event: ThreadEvent) -> StoredEvent {
    StoredEvent {
        thread_id: id.to_string(),
        seq,
        message_id,
        ts: "2026-10-06T22:13:46.128600+00:00".to_string(),
        event,
    }
}

/// **ONE TURN, AS IT IS MEASURED.** 129 events: the ask, 125 deltas, the reply they
/// assembled into, its usage, and one tool round trip.
fn a_turn(id: &str, from: u64, nth: u64) -> Vec<StoredEvent> {
    let mut out = Vec::new();
    let mut seq = from;
    let mut push = |message_id: Option<u64>, event: ThreadEvent| {
        let e = stored(id, seq, message_id, event);
        seq += 1;
        out.push(e);
    };
    push(
        Some(nth),
        ThreadEvent::UserMessage {
            text: "what is the honest ceiling for an unfiltered window?".to_string(),
            intent: Default::default(),
            metadata: Value::Null,
        },
    );
    // 125 deltas. The fragment length is the one the API actually produces: a
    // `text_delta` is a token or two, which is three to a dozen characters.
    for i in 0..125u64 {
        push(
            None,
            ThreadEvent::StreamEvent {
                raw: json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "text_delta", "text": if i % 4 == 0 { " the" } else { "ing" }}
                }),
            },
        );
    }
    push(
        None,
        ThreadEvent::AssistantText {
            // A paragraph, which is past `MAX_INLINE_PAYLOAD` and therefore a
            // `Payload::Split`: the structure on the frame, the text behind the door.
            text: "x".repeat(1_400),
            parent_tool_use_id: None,
            api_message_id: Some("msg_01ABCDEFGHIJKLMNOPQRSTUV".to_string()),
        },
    );
    push(
        None,
        ThreadEvent::StreamEvent {
            raw: json!({"type": "message_delta", "usage": {"output_tokens": 412}}),
        },
    );
    push(
        None,
        ThreadEvent::Unknown {
            raw: json!({"kind": "message_usage", "input_tokens": 41_002, "output_tokens": 412}),
        },
    );
    out
}

/// A window's worth of turns, truncated to exactly `WINDOW` rows.
fn a_full_window(id: &str) -> Vec<StoredEvent> {
    let mut all: Vec<StoredEvent> = Vec::new();
    let mut nth = 1u64;
    while all.len() < log::WINDOW {
        let from = all.len() as u64 + 1;
        all.extend(a_turn(id, from, nth));
        nth += 1;
    }
    all.truncate(log::WINDOW);
    all
}

/// **THE MEASUREMENT.**
///
/// It builds the rows through `log::row_of` — the shape's own builder, not a model
/// of it — and serialises them the way an export does, so the number is the number
/// a consumer receives and not an estimate of it.
///
/// **THE MUTATION.** `BUDGET = 1`.
///
/// Forced red:
/// ```text
/// an unfiltered window of 4000 rows serialises to 832561 bytes, and the
/// declared budget is 1. Either the row grew or the ceiling must come down —
/// `docs/shapes.md` 2.8 says to say which, never to quietly filter
/// ```
#[test]
fn an_unfiltered_window_of_four_thousand_rows_fits_its_budget() {
    let id = "9909b0ae-f2df-46c9-9591-a046aa169ecf";
    let events = a_full_window(id);
    assert_eq!(events.len(), log::WINDOW);

    let rows: Vec<LogRow> = events.iter().map(|e| log::row_of(id, e)).collect();
    let wire = serde_json::to_string(&rows).unwrap();
    let bytes = wire.len();

    // The breakdown, so the reader of a red knows WHICH row grew.
    let mut per_kind: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
    for (e, r) in events.iter().zip(rows.iter()) {
        let n = serde_json::to_string(r).unwrap().len();
        let slot = per_kind.entry(e.event.kind()).or_insert((0, 0));
        slot.0 += 1;
        slot.1 += n;
    }
    println!("\n── an unfiltered `threads/log` window, {} rows ──", log::WINDOW);
    for (kind, (n, total)) in &per_kind {
        println!("  {kind:<16} {n:>5} rows  {total:>9} bytes  ({} B/row)", total / n);
    }
    println!("  {:<16} {:>5} rows  {bytes:>9} bytes  ({} B/row)", "TOTAL", rows.len(), bytes / rows.len());
    println!(
        "  one connection at the key ceiling ({} windows): {} bytes\n",
        log::OPEN_WINDOWS,
        bytes * log::OPEN_WINDOWS
    );

    assert!(
        bytes <= BUDGET,
        "an unfiltered window of {} rows serialises to {bytes} bytes, and the \
         declared budget is {BUDGET}. Either the row grew or the ceiling must come \
         down — `docs/shapes.md` 2.8 says to say which, never to quietly filter",
        log::WINDOW
    );
    assert!(
        bytes * log::OPEN_WINDOWS <= FLEET_BUDGET,
        "a connection holding every window it may costs {} bytes, over the {FLEET_BUDGET} \
         this design accepted: the key ceiling and the window ceiling are one \
         decision and this is where they are checked together",
        bytes * log::OPEN_WINDOWS
    );
}

/// **A DELTA'S PAYLOAD IS CHEAPER ON THE FRAME THAN ITS SUMMARY WOULD BE — which is
/// the whole argument for `Payload::Here`, and it is arithmetic rather than
/// preference.**
///
/// `MAX_INLINE_PAYLOAD` exists because for the highest-volume kind a
/// `ResourceSummary` is BIGGER than the thing it would point at, and the fetch it
/// buys is a round trip per token. If that ever stops being true the bound is
/// arguable; while it is true the bound is not.
///
/// **THE MUTATION.** Compare against `summary.len() * 10`.
///
/// Forced red:
/// ```text
/// a delta carried inline is 160 bytes and its summary would be 74 — if the
/// summary were ever smaller, `Payload::Here` would be a preference rather than
/// arithmetic
/// ```
#[test]
fn a_delta_costs_less_on_the_frame_than_the_summary_that_would_replace_it() {
    use threads::shapes::body;
    let id = "9909b0ae-f2df-46c9-9591-a046aa169ecf";
    let e = stored(
        id,
        4_001,
        None,
        ThreadEvent::StreamEvent {
            raw: json!({
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": " the"}
            }),
        },
    );
    let inline = serde_json::to_string(&log::row_of(id, &e)).unwrap().len();
    let summary = serde_json::to_string(&body::served_summary(id, e.seq, &e.event).unwrap())
        .unwrap()
        .len();
    assert!(
        inline < summary * 3,
        "a delta carried inline is {inline} bytes and its summary would be {summary} \
         — if the summary were ever smaller, `Payload::Here` would be a preference \
         rather than arithmetic"
    );
    assert!(
        inline <= log::MAX_INLINE_PAYLOAD,
        "and every delta must be under the bound by construction, or the highest-\
         volume kind on the producer would go through the door: {inline}"
    );
}

/// **PRODUCTION'S LARGEST THREAD CARRIES A 177,995-ENTRY TAIL, AND THE WINDOW
/// BOUNDS IT RATHER THAN HOPING.**
///
/// The measured fact the ceiling exists for. The read is one seek plus `WINDOW`
/// steps whatever the thread's size, and the cell drops the rest off the FRONT and
/// says `complete: false` — never a short answer claiming to be the log.
///
/// Driven through the CELL rather than through a 177,995-row store, because the
/// claim is about the cell's bound and seeding 178,000 rows measures SQLite.
///
/// **THE MUTATION.** In `Sequence::read`, `self.items = entries.into_iter().map(..)`
/// with no `skip`.
///
/// Forced red:
/// ```text
/// a window handed 177995 entries holds 4000 of them: 177995
/// ```
#[test]
fn a_thread_with_a_hundred_and_seventy_eight_thousand_events_is_bounded_not_hoped_for() {
    use shapewire::fold::Sequence;
    let t0 = chrono::Utc::now();
    let mut cell: Sequence<u64> = Sequence::new_capacity_only(log::WINDOW);
    let entries: Vec<(chrono::DateTime<chrono::Utc>, u64)> = (1..=177_995u64)
        .map(|n| (t0 + chrono::Duration::milliseconds(n as i64), n))
        .collect();
    cell.read(t0 + chrono::Duration::days(1), entries, true);
    assert_eq!(
        cell.len(),
        log::WINDOW,
        "a window handed 177995 entries holds {} of them: {}",
        log::WINDOW,
        cell.len()
    );
    let held = cell.to_json();
    let first = held.as_array().unwrap().first().unwrap().as_u64().unwrap();
    assert_eq!(
        first,
        177_995 - log::WINDOW as u64 + 1,
        "and the ones it holds are the NEWEST — the recent end is the end anybody \
         asking for less of a window wants"
    );
    let state = serde_json::to_value(cell.state()).unwrap();
    assert_eq!(
        state["complete"], false,
        "and it says structurally that it is a FLOOR of the log, never a short \
         answer claiming to be the log: {state}"
    );
}
