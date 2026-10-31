//! **A DOOR PER PAYLOAD, AND THE PROOF THAT AN APPLICATION CANNOT SEE WHICH
//! ONE.**
//!
//! The decision these tests drive is a pure function of mirrored metadata, so
//! not one of them opens a socket, starts a runtime or talks to a producer.
//! That is the evidence for [`Mirror::plan`]'s shape as much as it is a
//! convenience: a decision that needed a connection to test would be a
//! decision that could not live in this module.

use serde_json::{json, Value};

use super::*;
use crate::envelope::codes;

// ─────────────────────────────────────────────────────────────────────────────
// The harness
// ─────────────────────────────────────────────────────────────────────────────

const FOLD: &str = "2026-10-12T09:00:00Z";

/// A door that serves **the same bytes on both halves** and records which half
/// was asked.
///
/// Identical bytes is the whole design of this fake. If the two halves answered
/// different content, every *the application cannot tell* assertion below would
/// be proving that the bytes happened to match rather than that nothing else
/// leaked.
struct Recorder {
    bytes: Vec<u8>,
    asked: std::cell::RefCell<Vec<&'static str>>,
}

impl Recorder {
    fn holding(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            asked: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<&'static str> {
        self.asked.borrow().clone()
    }
}

impl ResourceDoor for Recorder {
    fn inline(&self, _id: &str) -> Result<Vec<u8>, Refusal> {
        self.asked.borrow_mut().push("ws");
        Ok(self.bytes.clone())
    }

    fn by_id(&self, _id: &str) -> Result<Vec<u8>, Refusal> {
        self.asked.borrow_mut().push("get");
        Ok(self.bytes.clone())
    }
}

/// A door that refuses both halves in one vocabulary, which is what the
/// producer's two doors actually do.
struct Refusing(&'static str);

impl ResourceDoor for Refusing {
    fn inline(&self, _id: &str) -> Result<Vec<u8>, Refusal> {
        Err(Refusal::new(codes::GONE, self.0))
    }

    fn by_id(&self, _id: &str) -> Result<Vec<u8>, Refusal> {
        Err(Refusal::new(codes::GONE, self.0))
    }
}

/// A `known` cell holding a summary of `bytes` bytes served on `doors`.
fn summary_cell(id: &str, bytes: u64, doors: &[&str]) -> (Value, Value) {
    (
        json!({"body": {"id": id, "bytes": bytes, "transports": doors}}),
        json!({"body": {"state": "known", "complete": true, "folded_at": FOLD}}),
    )
}

/// One mirror holding one key, `sam`, whose `body` cell is a summary.
fn holding(bytes: u64, doors: &[&str]) -> Mirror {
    let mut m = Mirror::new("accounts");
    let (row, cells) = summary_cell("abc.7", bytes, doors);
    m.accept(&json!({
        "shape": "accounts", "key": "sam", "op": "read", "as_of": FOLD,
        "epoch": {"init_at": FOLD, "reinit": 0, "lost": 0, "state": "known"},
        "row": row, "cells": cells,
    }));
    m
}

/// The words a refusal an application reads must never contain.
const TRANSPORT_WORDS: &[&str] = &[
    "websocket", "socket", "http", "rest", "inline", "frame", "url",
];

fn names_no_transport(text: &str, what: &str) {
    let lower = text.to_lowercase();
    for word in TRANSPORT_WORDS {
        assert!(
            !lower.contains(word),
            "{what} names a transport: {text:?} contains {word:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The decision
// ─────────────────────────────────────────────────────────────────────────────

/// The decision at the boundary: one summary, two sizes, two doors — **and the
/// choice is per PAYLOAD, not per shape and not per connection.**
///
/// The threshold is a private const, so this test cannot quote it. It brackets
/// it instead, which is also the only form of this test that survives the
/// number being changed.
#[test]
fn a_payload_that_fits_rides_the_open_socket_and_one_that_does_not_goes_by_id() {
    let both = ["ws", "get"];
    assert_eq!(
        holding(1, &both).plan("sam", "body").unwrap(),
        Fetch::Inline { id: "abc.7".into() },
        "a one-byte payload rides the socket that is already open"
    );
    assert_eq!(
        holding(u64::MAX, &both).plan("sam", "body").unwrap(),
        Fetch::ById { id: "abc.7".into() },
        "an absurd payload falls back to the by-id door"
    );

    // ONE mirror, two keys of one shape, one connection, both answers. A
    // decision that were really per shape or per connection could not do this.
    let mut m = Mirror::new("accounts");
    for (key, id, bytes) in [("small", "a.1", 1u64), ("large", "a.2", u64::MAX)] {
        let (row, cells) = summary_cell(id, bytes, &both);
        m.accept(&json!({
            "shape": "accounts", "key": key, "op": "read", "as_of": FOLD,
            "epoch": {"reinit": 0, "lost": 0, "state": "known"},
            "row": row, "cells": cells,
        }));
    }
    assert_eq!(
        m.plan("small", "body").unwrap(),
        Fetch::Inline { id: "a.1".into() }
    );
    assert_eq!(
        m.plan("large", "body").unwrap(),
        Fetch::ById { id: "a.2".into() }
    );
    // And the id either arm carries is readable without matching on the arm.
    assert_eq!(m.plan("large", "body").unwrap().id(), "a.2");
}

/// **THE RULING, AS A TEST.** The application calls one thing, it gets bytes,
/// the two calls took genuinely different doors, and nothing it was handed says
/// which.
#[test]
fn the_application_cannot_tell_which_door_carried_the_payload() {
    let content = b"the body of a message".to_vec();
    let n = content.len() as u64;

    // Same declared size, same content, same call. The ONE thing that differs
    // is which doors the producer declared, which forces the two paths apart
    // while leaving everything the application touches identical.
    let over_ws = holding(n, &["ws", "get"]);
    let ws_door = Recorder::holding(content.clone());
    let via_ws = over_ws.read(&ws_door, "sam", "body").expect("bytes");

    let over_get = holding(n, &["get"]);
    let get_door = Recorder::holding(content.clone());
    let via_get = over_get.read(&get_door, "sam", "body").expect("bytes");

    // 1. THE TWO READS GENUINELY TOOK DIFFERENT DOORS. Without this sentinel
    //    the rest of this test asserts that one path equals itself.
    assert_eq!(ws_door.asked(), vec!["ws"]);
    assert_eq!(get_door.asked(), vec!["get"]);
    assert_ne!(
        ws_door.asked(),
        get_door.asked(),
        "two operands, proven different before anything is compared"
    );

    // 2. AND THE RESULTS ARE INDISTINGUISHABLE. `read` answers `Vec<u8>`, so
    //    there is no field, wrapper or sibling value that could name a door:
    //    byte equality IS total equality of what the application received.
    assert_eq!(via_ws, via_get);
    assert_eq!(via_ws, content);

    // 3. AND A FAILURE DOES NOT LEAK IT EITHER. Both doors refuse in one
    //    vocabulary, so the application's branch is the same on either.
    let door = Refusing("deleted");
    let refused_ws = holding(n, &["ws", "get"]).read(&door, "sam", "body").unwrap_err();
    let refused_get = holding(n, &["get"]).read(&door, "sam", "body").unwrap_err();
    assert_eq!(refused_ws, refused_get, "one refusal, two doors");
    assert!(matches!(refused_ws, ContentError::Door(ref r) if r.code == codes::GONE));
}

/// **NO REFUSAL THIS MODULE MINTS NAMES A PROTOCOL.** The one that comes
/// closest echoes the PRODUCER's own words and adds none of its own.
#[test]
fn no_refusal_an_application_reads_names_a_door() {
    for (what, error) in [
        ("NoSummary", ContentError::NoSummary),
        (
            "NotKnown",
            ContentError::NotKnown {
                reason: "the payload would not parse".into(),
            },
        ),
        (
            "Integrity",
            ContentError::Integrity {
                declared: 1,
                got: 2,
            },
        ),
    ] {
        names_no_transport(&error.to_string(), what);
    }
    // `NoTransport` echoes the producer's own list verbatim, which is the one
    // place a door word may appear — and it appears because the PRODUCER said
    // it, not because this module did.
    let echoed = ContentError::NoTransport {
        bytes: 9,
        offered: vec!["ws".into()],
    }
    .to_string();
    assert!(echoed.contains("ws"), "{echoed}");
    assert!(echoed.contains('9'), "and it names the size: {echoed}");
    let invented = ContentError::NoTransport {
        bytes: 9,
        offered: vec![],
    }
    .to_string();
    assert!(
        invented.contains("nothing"),
        "a producer offering nothing says so in words: {invented}"
    );
    names_no_transport(&invented, "NoTransport with an empty list");
}

/// Deliver-or-refuse from the consuming end: the summary's `bytes` is the one
/// place a resource's length is stated, so a door that disagrees with it is
/// refused and the bytes are dropped.
#[test]
fn a_door_that_answers_the_wrong_length_is_an_integrity_refusal() {
    let m = holding(21, &["ws", "get"]);
    let short = Recorder::holding(b"too short".to_vec());
    assert_eq!(
        m.read(&short, "sam", "body"),
        Err(ContentError::Integrity {
            declared: 21,
            got: 9
        }),
        "bytes plus a discrepancy is not an answer"
    );
    assert_eq!(
        short.asked(),
        vec!["ws"],
        "and the door WAS asked — this is a check on the answer, not on the plan"
    );
    // The opposing arm: the same door answering the declared length delivers.
    let right = Recorder::holding(vec![0u8; 21]);
    assert_eq!(m.read(&right, "sam", "body"), Ok(vec![0u8; 21]));
}

/// A summary sitting in a cell that is not `Known` is the ABSENCE of a
/// statement, and nothing is asked for on the strength of it.
#[test]
fn a_summary_in_a_cell_that_is_not_known_is_refused_before_any_fetch() {
    let mut m = Mirror::new("accounts");
    let (row, _) = summary_cell("abc.7", 1, &["ws"]);
    m.accept(&json!({
        "shape": "accounts", "key": "sam", "op": "read", "as_of": FOLD,
        "epoch": {"reinit": 0, "lost": 0, "state": "known"},
        "row": row,
        "cells": {"body": {"state": "not_hydrated", "reason": "the payload would not parse"}},
    }));
    let door = Recorder::holding(b"x".to_vec());
    assert_eq!(
        m.read(&door, "sam", "body"),
        Err(ContentError::NotKnown {
            reason: "the payload would not parse".into()
        })
    );
    assert!(door.asked().is_empty(), "nothing was asked for");

    // And a dead shape takes every cell with it, summary or no summary.
    m.accept(&json!({
        "shape": "accounts", "op": "read", "as_of": "2026-10-12T09:01:00Z",
        "epoch": {"reinit": 0, "lost": 0, "state": "dead", "dead_reason": "the task panicked"},
    }));
    assert_eq!(
        m.read(&door, "sam", "body"),
        Err(ContentError::NotKnown {
            reason: "the task panicked".into()
        }),
        "and the refusal carries the producer's own reason"
    );
    assert!(door.asked().is_empty());
}

/// A cell that names nothing, and a cell whose value is not a summary, are one
/// answer: the shape did not say this exists.
#[test]
fn a_cell_that_names_no_resource_is_not_a_fetch() {
    let mut m = Mirror::new("accounts");
    m.accept(&json!({
        "shape": "accounts", "key": "sam", "op": "read", "as_of": FOLD,
        "epoch": {"reinit": 0, "lost": 0, "state": "known"},
        "row": {"share": 0.25, "body": {"size": 10}},
        "cells": {
            "share": {"state": "known", "complete": true, "folded_at": FOLD},
            "body": {"state": "known", "complete": true, "folded_at": FOLD},
        },
    }));
    assert_eq!(m.plan("sam", "share"), Err(ContentError::NoSummary));
    assert_eq!(
        m.plan("sam", "body"),
        Err(ContentError::NoSummary),
        "an object that is not a summary is not half a summary"
    );
    assert_eq!(
        m.plan("nobody", "body"),
        Err(ContentError::NotKnown {
            reason: UNSEEN.into()
        }),
        "and a key nothing exported is not hydrated, not empty"
    );
    // The opposing arm, so this is not just "every plan refuses": a cell that
    // DOES carry a summary plans.
    assert!(holding(1, &["ws"]).plan("sam", "body").is_ok());
}

/// Rule 2.6's obligation applied to the transport list: a word this build has
/// never heard of is IGNORED — not a parse failure, and not a door.
#[test]
fn an_unknown_door_word_is_ignored_rather_than_believed() {
    assert_eq!(
        holding(1, &["quic", "get"]).plan("sam", "body").unwrap(),
        Fetch::ById { id: "abc.7".into() },
        "the known door is used and the unknown word changes nothing"
    );
    assert_eq!(
        holding(1, &["quic"]).plan("sam", "body"),
        Err(ContentError::NoTransport {
            bytes: 1,
            offered: vec!["quic".into()]
        }),
        "a producer offering only doors this build lacks is refused, naming them"
    );
    assert_eq!(
        holding(1, &[]).plan("sam", "body"),
        Err(ContentError::NoTransport {
            bytes: 1,
            offered: vec![]
        }),
        "and a producer offering none at all is refused too"
    );
}

/// A payload above this chassis's own inline ceiling whose producer offers only
/// the socket is refused HERE, rather than asked for and refused a layer down.
#[test]
fn an_oversized_payload_with_only_the_socket_offered_is_refused_not_attempted() {
    let m = holding(u64::MAX, &["ws"]);
    let door = Recorder::holding(Vec::new());
    assert!(matches!(
        m.read(&door, "sam", "body"),
        Err(ContentError::NoTransport { .. })
    ));
    assert!(
        door.asked().is_empty(),
        "an attempt would be a round trip spent learning what the mirror already holds"
    );
    // The opposing arm, at the same door list: under the ceiling, `ws` alone is
    // enough and the socket IS asked.
    let small = holding(1, &["ws"]);
    let asked = Recorder::holding(vec![0u8; 1]);
    assert_eq!(small.read(&asked, "sam", "body"), Ok(vec![0u8]));
    assert_eq!(asked.asked(), vec!["ws"]);
}

/// **ONE DECISION FUNCTION, NOT TWO.** `read` must be `plan` plus a fetch. If
/// it ever decided on its own the two would drift, and an `async` consuming
/// half — which can only call `plan` — would take a different door from a
/// blocking one.
#[test]
fn the_one_call_and_the_plan_always_agree() {
    let cases: [(u64, &[&str]); 7] = [
        (1, &["ws", "get"]),
        (1, &["get"]),
        (1, &["ws"]),
        (u64::MAX, &["ws", "get"]),
        (u64::MAX, &["get"]),
        (u64::MAX, &["ws"]),
        (1, &[]),
    ];
    let mut planned_inline = 0;
    let mut planned_by_id = 0;
    let mut refused = 0;
    for (bytes, doors) in cases {
        let m = holding(bytes, doors);
        let door = Recorder::holding(vec![0u8; bytes.min(64) as usize]);
        let plan = m.plan("sam", "body");
        let _ = m.read(&door, "sam", "body");
        match (&plan, door.asked().first()) {
            (Ok(Fetch::Inline { .. }), Some(&"ws")) => planned_inline += 1,
            (Ok(Fetch::ById { .. }), Some(&"get")) => planned_by_id += 1,
            (Err(_), None) => refused += 1,
            other => panic!("plan and read disagree for {bytes}/{doors:?}: {other:?}"),
        }
    }
    // **AND THE THREE OUTCOMES WERE ALL REACHED.** Without this the match above
    // would pass on a `plan` that refused everything and a `read` that asked
    // nothing, which is a constant implementation.
    assert_eq!(
        (planned_inline, planned_by_id, refused),
        (2, 3, 2),
        "the seven cases must cover inline, by-id and refused"
    );
}

/// The inline door's payload is a WIRE FORMAT, so the chassis reads it. These
/// are the decoder's own terms, asserted against the encoding rather than
/// against the encoder — substrate's real encoder is behind the `server`
/// feature and is invisible to this build.
#[test]
fn the_inline_payload_decodes_strictly_or_not_at_all() {
    // Every padding case, which is where a hand-written decoder goes wrong.
    for (encoded, want) in [
        ("", &b""[..]),
        ("YQ==", b"a"),
        ("YWI=", b"ab"),
        ("YWJj", b"abc"),
        ("YWJjZA==", b"abcd"),
        ("YWJjZGU=", b"abcde"),
        ("/+8=", &[0xff, 0xef][..]),
    ] {
        let got = inline_bytes(&json!({"bytes": want.len(), "base64": encoded}))
            .unwrap_or_else(|e| panic!("{encoded:?}: {e:?}"));
        assert_eq!(got, want, "{encoded:?}");
    }

    // Non-UTF-8 bytes survive, which is the whole reason the encoding exists.
    let raw = vec![0u8, 1, 2, 0xff, 0xfe, 0x80];
    assert_eq!(
        inline_bytes(&json!({"bytes": raw.len(), "base64": "AAEC//6A"})).unwrap(),
        raw
    );

    // And a lenient decode would be deliver-and-warn one layer down. Wrong
    // length, a stray pad, a character outside the alphabet, all pad, and
    // padding in the middle of the string.
    for bad in ["YWJ", "YQ=", "Y!==", "====", "YQ==YQ=="] {
        assert!(
            inline_bytes(&json!({"base64": bad})).is_err(),
            "{bad:?} must not decode"
        );
    }
    assert!(
        inline_bytes(&json!({"bytes": 1})).is_err(),
        "a reply with no `base64` at all"
    );
    assert!(
        inline_bytes(&json!({"bytes": 99, "base64": "YQ=="})).is_err(),
        "a reply that disagrees with itself"
    );
    // A reply that states no length is believed, because the SUMMARY is where
    // the length is stated and `Mirror::read` checks it there.
    assert_eq!(inline_bytes(&json!({"base64": "YQ=="})).unwrap(), b"a");

    // **AND NOT ONE OF THESE REFUSALS NAMES THE DOOR IT CAME THROUGH.** They
    // reach an application as `ContentError::Door`, so they stand on the same
    // footing as the refusals `Mirror::read` mints — `base64` names an
    // encoding, not a transport.
    for bad in [
        json!({"bytes": 1}),
        json!({"base64": "Y!=="}),
        json!({"bytes": 99, "base64": "YQ=="}),
    ] {
        let refusal = inline_bytes(&bad).unwrap_err();
        names_no_transport(&refusal.message, "an inline decode refusal");
    }
}

/// The module doc's seam list and the number the sentence above it claims must
/// agree. That sentence said *five* over a six-item list in substrate for as
/// long as the list had six items.
///
/// **This probe reads the module's own source, and that is sound for exactly
/// one reason**: what is under test IS the prose. The claim is only that the
/// numeral and the list agree, and both of them are text in that file — so
/// grepping the text is measuring the thing itself, not a proxy for behaviour.
/// It would be unsound if it were asserting anything about what the code does.
#[test]
fn the_seam_list_matches_the_count_above_it() {
    let source = include_str!("../mirror.rs");
    let items = source
        .lines()
        .filter(|line| {
            let Some(rest) = line.strip_prefix("//! ") else {
                return false;
            };
            let mut chars = rest.chars();
            chars.next().is_some_and(|c| c.is_ascii_digit()) && rest[1..].starts_with(". ")
        })
        .count();
    assert_eq!(items, 7, "the seam list has {items} items");
    assert!(
        source.contains("exactly seven things"),
        "and the sentence above it must say seven"
    );
}
