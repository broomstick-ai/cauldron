//! # Message bodies — the first resource on any producer
//!
//! Before this file `GET /resource/<id>` answered `501 unsupported` for every
//! id on this daemon, because `threads` implemented neither `Resources` nor
//! `Uploads`; the `blob` dtype was dropped for having no instance, and
//! transport transparency had nothing to be transparent about. This is the
//! instance.
//!
//! ## The reasoning this was asked to verify, and it holds
//!
//! `docs/thread-shapes-plan.md` argued that the shape carries WHERE the log is
//! while the content is fetched, and `docs/chassis-resources.md` 1.2 sharpened
//! it to *"a shape says what exists; the resource fetch says what it is"*.
//! Verified against the code rather than taken from the documents:
//!
//! * **A `Sequence` of the log is still not available.** `shapewire::fold`'s
//!   `Sequence` has `emit`, `mark_since_init`, `now`, `to_json`, `take_pending`,
//!   `len`, `is_empty`, `state`, `mark_dead` — and nothing that accepts an
//!   `as_of`. A message tail would be `since_init` from the instant of
//!   subscription, which is strictly worse than the cursor that exists.
//! * **`threads/{id}/history {after_seq}` is the paged read, and it stays.**
//!   `Store::list_events` is `seq > after_seq ORDER BY seq ASC LIMIT ?`. Rule
//!   2.5 is not engaged by it for the same reason it is not engaged by a byte
//!   `Range`: the position is held by the CONSUMER, the producer keeps nothing
//!   between two calls, and a retry is the identical request.
//! * **So the division is confirmed, and this file only narrows it.** The
//!   plan's §7 table said a consumer must use the RPC for *message bodies*;
//!   that row is what leaves the table. `history` keeps replay-from-a-cursor
//!   and full-text query; one body by id is a resource.
//!
//! ## A SUMMARY, never an enumeration
//!
//! The row carries ONE summary: the newest message's body. A thread with 5,000
//! messages lists no ids anywhere — not 5,000, not 64, not a page of them. That
//! is the rule `docs/chassis-resources.md` 3.3 states and it is also what keeps
//! `threads`' export the size it was measured at: a summary is three fields.
//!
//! The ids of OLDER bodies are reachable, and not from the shape: `history`
//! answers events by `seq`, and `seq` is what an id is built from — by the
//! PRODUCER, in [`body_id`], never by a consumer. A consumer that has walked
//! history holds seqs, not ids, and asks the shape or the paged read for
//! content; it cannot spell a resource id, which is the point of
//! `ResourceId`'s private field.
//!
//! ## What a body IS, and what has none
//!
//! The TEXT of a message: `user_message.text`, `assistant_text.text`,
//! `thinking.text`. Everything else — a turn boundary, a tool result, a gauge
//! observation — has no body, and the summary is then absent rather than
//! present-and-empty. *There is no body* and *the body is zero bytes* are two
//! different statements and one spelling for two claims is the collapse every
//! rule in `docs/shapes.md` 2 is against.
//!
//! **It is the text and not the whole payload JSON.** A body is what a reader
//! reads; the payload's other fields (`intent`, `metadata`, the tool-use
//! envelope) are the event's structure, and `history` already answers those.
//! Serving the JSON here would make the resource a second spelling of an event
//! the RPC already returns — a second source for one fact, which rule 2.4 is
//! for deleting.
//!
//! ## Retention, and deletion orthogonal to it
//!
//! `Retention::Fixed` — immutable, one id for ever. Not an assumption:
//! `(thread_id, seq)` is `UNIQUE` in `schema.sql` and **no `UPDATE events` and
//! no `DELETE FROM events` exists anywhere under `lib/threads/src/`**, so the
//! row an id names cannot change under the writer set that exists. Deletion is
//! orthogonal (`chassis-resources.md` 2.3) and the door answers it: a `seq`
//! this store does not hold is [`codes::UNKNOWN`], honestly, because this store
//! cannot tell *never written* from *deleted* and the design says to answer
//! `unknown` rather than guess `gone`.

use shapewire::envelope::{codes, Refusal};
use shapewire::wire::{ResourceSummary, Transport};

use crate::event::ThreadEvent;

/// The media type a message body is served under, on BOTH doors.
pub const BODY_CONTENT_TYPE: &str = "text/plain; charset=utf-8";

/// **THE DOORS A MESSAGE BODY CAN BE READ THROUGH — A FACT, NOT A PREFERENCE.**
///
/// Both exist on this producer: `resource/{id}/read` inline over the socket, and
/// `GET /resource/<id>`. The list is the same for every body REGARDLESS OF SIZE,
/// and that is deliberate — pruning `ws` off a large payload would be this
/// producer making the policy call the operator assigned to the consuming side
/// (*"how can you say on the producer side push a payload over REST if the
/// consumer didn't ask for it?"*). The producer states what is supported; the
/// consuming chassis reads `bytes` and decides.
const BODY_TRANSPORTS: &[Transport] = &[Transport::Ws, Transport::Get];

/// **THE ID, MINTED BY THE SHAPE: `<thread_id>.<seq>`.**
///
/// Opaque to everyone else. `ResourceId`'s field and constructor are both
/// private, so there is no path by which a consumer reaches the door without
/// having read one of these off a summary — and this function is the only place
/// the spelling exists, so changing it is one edit and a `VERSION` bump
/// (`chassis-resources.md` 2.1's `ID_SCHEME` ruling).
///
/// `None` when the thread id contains a byte outside the resource id charset.
/// `ThreadManager::create_thread` takes a caller-supplied id when one is given,
/// so an id with a `/` or a `%` in it is reachable; an id this door could not
/// accept is NOT published as a summary, because a summary naming an
/// unfetchable resource is a shape claiming something exists that cannot be
/// read. The charset is `shapewire::resource`'s and is restated as a
/// predicate here rather than guessed — `tests/thread_bodies.rs` pins the two
/// against each other through the real door.
pub fn body_id(thread_id: &str, seq: u64) -> Option<String> {
    if thread_id.is_empty() || !thread_id.bytes().all(is_id_byte) {
        return None;
    }
    Some(format!("{thread_id}.{seq}"))
}

/// The resource id charset, as `shapewire::resource::is_id_byte` defines it.
fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b':' | b'@' | b'+')
}

/// **THE ID, READ BACK — and `rsplit_once`, not `split_once`.**
///
/// A thread id may legally contain `.` (the charset allows it, and
/// `bob.example.com` is the kind of id a caller supplies), so splitting from
/// the LEFT would cut a dotted thread id in half and look up a seq that is not a
/// number. The seq is the last segment by construction of [`body_id`], so the
/// last `.` is the separator and the thread id is everything before it.
///
/// Returns `None` for anything this producer did not mint — which the door turns
/// into [`codes::UNKNOWN`], the honest answer for *"your summary is corrupt or
/// came from another producer"*.
pub fn parse_body_id(id: &str) -> Option<(&str, u64)> {
    let (tid, seq) = id.rsplit_once('.')?;
    if tid.is_empty() {
        return None;
    }
    // No leading `+`, no leading zeros, no whitespace: `u64::from_str` accepts
    // `+7` and this producer never mints it, so accepting it would make two ids
    // name one resource and two ETags disagree about one blob.
    if seq.is_empty() || (seq.len() > 1 && seq.starts_with('0')) || !seq.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((tid, seq.parse().ok()?))
}

/// **WHAT COUNTS AS A BODY.** `None` for every event that has no text a reader
/// would read.
pub fn body_text(event: &ThreadEvent) -> Option<&str> {
    match event {
        ThreadEvent::UserMessage { text, .. } => Some(text),
        ThreadEvent::AssistantText { text, .. } => Some(text),
        ThreadEvent::Thinking { text, .. } => Some(text),
        _ => None,
    }
}

/// **THE KINDS A MESSAGE BODY IS THE TEXT OF** — [`body_text`]'s own set, named
/// so the door and the shape cannot hold two ideas of it.
pub const BODY_KINDS: &[&str] = &["user_message", "assistant_text", "thinking"];

/// **WHAT THE DOOR SERVES FOR AN EVENT THAT IS NOT A MESSAGE** — its PAYLOAD, as
/// the store holds it, under `application/json`.
///
/// This is the widening `crate::shapes::log` needs and it is deliberately NOT a
/// change to [`body_text_of_payload`], which is `crate::shapes::threads`' `body`
/// cell's builder and must keep answering `Ok(None)` for a kind that is not a
/// message. Two questions, two functions:
///
/// * *What is this MESSAGE's body?* — [`body_text_of_payload`], three kinds, and
///   `threads.body` publishes a summary only when it answers.
/// * *What is behind this id?* — this door, every kind, because a shape now
///   publishes a summary for rows of every kind and a summary naming something
///   the door refuses is a shape claiming something exists that cannot be read.
///
/// Before `crate::shapes::log` existed the door answered [`codes::UNKNOWN`] for a
/// bodiless id, on the stated grounds that *"the shape publishes no id for a
/// bodiless event — so arriving here means the consumer built an id"*. A shape
/// does publish them now, so that reasoning expires with it; what remains true,
/// and is still the refusal, is an id naming a `(thread_id, seq)` this store does
/// not hold.
pub const PAYLOAD_CONTENT_TYPE: &str = "application/json; charset=utf-8";

/// **WHAT THE DOOR HANDS BACK FOR ONE EVENT — the bytes and the media type,
/// decided by the KIND and by nothing else.**
///
/// A message's body is its TEXT under [`BODY_CONTENT_TYPE`]; every other event is
/// its PAYLOAD under [`PAYLOAD_CONTENT_TYPE`]. The discriminator is on the row a
/// consumer already holds (`kind`), so nothing is guessed from the bytes and no
/// second id scheme exists.
///
/// **IT TAKES THE `ThreadEvent`, NOT THE STORED COLUMN, AND THAT IS THE WHOLE
/// POINT.** A summary states `bytes`; the door serves bytes; if the two were
/// produced by two expressions they could disagree, and a consumer reading a
/// `bytes` the door then contradicts has two sources for one number. Both ends
/// call THIS function on a `ThreadEvent`, and both ends have one: the shape's live
/// fold gets it off `Push::Event`, the shape's hydrate and the door get it off
/// `Store::rows_to_events`, which is also where an unreadable row is degraded to
/// `Unknown` exactly once. So `bytes` is the length of this function's own answer,
/// by construction, on every path.
pub fn served_of(event: &ThreadEvent) -> (String, &'static str) {
    if let Some(t) = body_text(event) {
        return (t.to_string(), BODY_CONTENT_TYPE);
    }
    (
        // `ThreadEvent` is `Serialize` and infallible for every variant it has —
        // the degrade path already turned anything unreadable into
        // `Unknown { raw }` at the store boundary, so there is no second failure
        // mode to represent here and no `Result` to make a caller unwrap.
        serde_json::to_string(event).unwrap_or_else(|_| "null".to_string()),
        PAYLOAD_CONTENT_TYPE,
    )
}

/// **THE SUMMARY FOR ANY EVENT — what exists, by which id, how big, through which
/// doors.** [`summary`]'s sibling, and the generalisation of it:
/// `summary` answers only for a MESSAGE and is `crate::shapes::threads`' `body`
/// cell's builder; this answers for every kind and is
/// `crate::shapes::log`'s. The `bytes` is [`served_of`]'s own answer measured, so
/// the number the summary states is the number the door delivers.
///
/// `None` when the thread id holds a byte outside the resource-id charset — a
/// summary naming an unfetchable resource is a shape claiming something exists
/// that cannot be read, which is [`summary`]'s own rule and is why it is restated
/// rather than relaxed.
pub fn served_summary(thread_id: &str, seq: u64, event: &ThreadEvent) -> Option<ResourceSummary> {
    let (served, _) = served_of(event);
    Some(ResourceSummary::new(
        body_id(thread_id, seq)?,
        served.len() as u64,
        BODY_TRANSPORTS,
    ))
}

/// The same question against a RAW payload, for the hydrate — which reads the
/// column and has no `ThreadEvent` in hand.
///
/// `Err` is a payload that would not parse: the store holds a row the shape
/// cannot read, which is `CellState::NotHydrated` with a reason and NOT an
/// absent body. `Ok(None)` is a kind that has no body, which is a complete
/// answer. The three-valued unknown, applied to one cell.
pub fn body_text_of_payload(kind: &str, payload_json: &str) -> Result<Option<String>, String> {
    if !matches!(kind, "user_message" | "assistant_text" | "thinking") {
        return Ok(None);
    }
    let v: serde_json::Value = serde_json::from_str(payload_json)
        .map_err(|e| format!("this event's payload would not parse: {e}"))?;
    match v.get("text").and_then(serde_json::Value::as_str) {
        Some(t) => Ok(Some(t.to_string())),
        // A body-bearing KIND whose payload carries no `text` is a row this
        // shape cannot read, not a message with no body.
        None => Err("this event's payload carries no `text`".to_string()),
    }
}

/// **THE SUMMARY: what exists, by which id, how big, and through which doors.**
///
/// Nothing else. No content, no excerpt, no first line — an excerpt would be the
/// content arriving on the frame the resource exists to keep it off, and it
/// would be a second source for bytes the door serves.
pub fn summary(thread_id: &str, seq: u64, text: &str) -> Option<ResourceSummary> {
    Some(ResourceSummary::new(
        body_id(thread_id, seq)?,
        text.len() as u64,
        BODY_TRANSPORTS,
    ))
}

/// The refusal an id this producer cannot resolve earns.
pub fn unknown(id: &str) -> Refusal {
    Refusal::new(
        codes::UNKNOWN,
        format!(
            "no message body is held under {id}. An id is read off a shape's summary and \
             never built; this store cannot distinguish an event never written from one \
             deleted, so it answers `unknown` rather than guessing `gone`"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_round_trips_and_a_dotted_thread_id_survives_it() {
        for tid in ["7f3e", "bob.example.com", "a-b_c~d:e@f+g", "0"] {
            let id = body_id(tid, 41).expect("spellable");
            assert_eq!(parse_body_id(&id), Some((tid, 41)));
        }
        // `split_once` instead of `rsplit_once` would read this as thread `cc2`.
        assert_eq!(
            parse_body_id("bob.example.com.9"),
            Some(("bob.example.com", 9))
        );
    }

    #[test]
    fn a_thread_id_the_door_could_not_accept_is_never_published() {
        for bad in ["", "a/b", "a%2fb", "a?b", "a&b=c", "a#b"] {
            assert_eq!(body_id(bad, 1), None, "{bad:?}");
            assert_eq!(summary(bad, 1, "hello"), None, "{bad:?}");
        }
    }

    #[test]
    fn only_one_spelling_of_a_seq_parses() {
        for bad in ["t.", "t.+7", "t.007", "t.-1", "t. 7", "t.7a", ".7", "7"] {
            assert_eq!(parse_body_id(bad), None, "{bad:?}");
        }
        assert_eq!(parse_body_id("t.0"), Some(("t", 0)));
    }

    #[test]
    fn a_summary_states_the_size_and_both_doors_and_nothing_else() {
        let s = summary("t", 3, "hello").expect("spellable");
        assert_eq!(s.id, "t.3");
        assert_eq!(s.bytes, 5);
        assert!(s.supports(Transport::Ws) && s.supports(Transport::Get));
        // THE WHOLE SUMMARY, serialised. Three fields: if an excerpt or a
        // content type or a transport RANKING ever appears here, this fails.
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            serde_json::json!({"id": "t.3", "bytes": 5, "transports": ["ws", "get"]})
        );
    }

    #[test]
    fn the_transport_list_does_not_depend_on_the_size() {
        let small = summary("t", 1, "a").unwrap();
        let big = summary("t", 2, &"a".repeat(4 * 1024 * 1024)).unwrap();
        assert_eq!(
            small.transports, big.transports,
            "a producer that pruned by size would be making the consuming side's call"
        );
    }

    #[test]
    fn a_kind_with_no_body_and_a_payload_that_will_not_parse_are_two_answers() {
        assert_eq!(body_text_of_payload("turn_ended", "{}"), Ok(None));
        assert_eq!(
            body_text_of_payload("user_message", r#"{"text":"hi"}"#),
            Ok(Some("hi".to_string()))
        );
        assert!(body_text_of_payload("user_message", "{not json").is_err());
        assert!(
            body_text_of_payload("user_message", r#"{"intent":"say"}"#).is_err(),
            "a body-bearing kind with no text is unreadable, not bodiless"
        );
    }
}
