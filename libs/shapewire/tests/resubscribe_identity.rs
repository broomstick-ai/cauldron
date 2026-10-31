//! **THE HEADLINE PROOF OF RULE 2.5** — *"Recovery is re-subscribe. There is no
//! re-emit verb, no replay, no cursor."*
//!
//! The claim this file exists to make checkable, in the operator's terms: a
//! consumer that DISCONNECTS and RE-SUBSCRIBES ends up holding the same thing as
//! a consumer that never disconnected — so the cursor, the park buffer, the
//! `after_seq` repair and the end-of-repair handshake a face keeps in order to
//! heal its own state are not merely unnecessary, they are DELETABLE. That is
//! what makes `apps/tui/src/transport.rs`'s `Cursors` / `park` / `end_repair` /
//! `request_history` / `Purpose` / `Pending::History` retirable, and it is why
//! the proof is phrased as an EQUALITY between two mirrors rather than as a list
//! of things `reset` clears (which `src/mirror/tests.rs`'s
//! `reset_drops_everything_the_connection_held_and_keeps_the_contract` already
//! does, one mirror at a time, and which cannot see a disagreement).
//!
//! **Two mirrors, one producer story.** Mirror `NEVER` stays connected across
//! the whole story and folds every live frame. Mirror `DROPPED` folds the first
//! part, loses its socket, `reset()`s, and is handed the ONE export a plain
//! re-subscribe produces — the export that states the producer's state as it is
//! NOW. Nothing else. No second ask, no position, no range.
//!
//! **What must be identical, and it is the half an application renders:** the
//! key set, every row, and every cell's STATE KIND.
//!
//! **What is NOT identical, stated here rather than discovered later**, because a
//! proof that overclaims is worse than none:
//!
//! 1. **Provenance stamps.** `NEVER` learned `share` from a live frame at
//!    12:03; `DROPPED` read it off an export stamped 12:05. Both say `Known`,
//!    and they honestly disagree about WHEN THIS CONSUMER LEARNED IT. A
//!    consumer that wanted those equal would be asking the producer to lie.
//! 2. **Locally-witnessed vanishings.** A key that went `gone` while `NEVER`
//!    watched is in its `vanished()` list. `DROPPED` never saw the event and the
//!    re-subscribe export simply does not mention the key, so it reads
//!    `KeyState::Unknown` rather than `Gone`. That asymmetry is CORRECT — "it
//!    went away" is an event a connection witnesses, and a connection that did
//!    not witness it has no claim to it — and it is the reason the consumer seam
//!    keeps `Holding::{Held, Gone, Unknown, NotAsked}` as four words and not
//!    three. It is also why it does not block the deletion: nothing on the
//!    terminal face renders "a thread that vanished while I was disconnected".
//! 3. **Windowed `Sequence` items**, which rule 2.5 names outright: *"A consumer
//!    with a longer memory than a windowed cell loses items on re-subscribe BY
//!    DESIGN, and `covers_since` says so on the frame."* Asserted below as a
//!    STATED loss, never a silent one.
//!
//! Everything else being equal is the whole property, and `forced_red` at the
//! bottom of this file is the mutation that proves these assertions can fail.

use serde_json::{json, Value};
use shapewire::mirror::{KeyState, Mirror};

const SHAPE: &str = "accounts";

fn known(at: &str) -> Value {
    json!({"state": "known", "complete": true, "folded_at": at})
}

/// An `op: read` export — what a subscribe answers, and the ONLY thing a
/// re-subscribe is handed. Note what is absent and stays absent: any field
/// naming a position, a cursor, a sequence number or a range.
fn export(key: &str, as_of: &str, row: Value, cells: Value) -> Value {
    json!({
        "shape": SHAPE, "key": key, "op": "read",
        "as_of": as_of,
        "epoch": {"init_at": "2026-09-25T11:59:59Z", "reinit": 0, "lost": 0, "state": "known"},
        "cells": cells,
        "row": row,
    })
}

fn live(key: Option<&str>, at: &str, event: Value) -> Value {
    let mut v = json!({
        "shape": SHAPE, "op": "live", "at": at,
        "epoch": {"reinit": 0, "lost": 0, "state": "known"},
        "event": event,
    });
    if let Some(k) = key {
        v["key"] = json!(k);
    }
    v
}

/// The read surface an application actually renders, reduced to something
/// comparable. Deliberately EXCLUDES the two honest differences (stamps,
/// locally-witnessed vanishings) and includes everything else.
fn rendered(m: &Mirror) -> Vec<(String, Value, Vec<(String, bool)>)> {
    let mut out = Vec::new();
    for key in m.keys() {
        let held = m.held(&key).expect("a key in keys() is held");
        let mut cells: Vec<(String, bool)> = held
            .cells()
            .iter()
            .map(|(name, st)| (name.clone(), st.is_known()))
            .collect();
        cells.sort();
        out.push((key.clone(), held.row().clone(), cells));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// The proof
// ─────────────────────────────────────────────────────────────────────────────

/// **A MIRROR THAT RECONNECTED IS THE MIRROR THAT NEVER DID.** No cursor, no
/// `after_seq`, no re-emit verb — one `subscribe`, one export, done.
#[test]
fn a_reconnected_mirror_equals_one_that_never_disconnected() {
    // ── the story, told once, to two consumers ──────────────────────────────
    //
    // 12:00  export: sam share 0.25, work share 0.50     ← BOTH see this
    //        ·········· the socket under DROPPED dies here ··········
    // 12:01  live:   work goes `gone`
    // 12:02  live:   sam's row moves to 0.75
    // 12:03  live:   a new key `ada` is `added`; the consumer names it
    //
    // A plain re-subscribe at 12:05 therefore exports exactly: sam 0.75,
    // ada 0.10. `work` is not mentioned, because it is not there any more.
    //
    // **THE DROP IS PLACED BEFORE THE VANISHING ON PURPOSE, AND THAT IS THE
    // WHOLE DESIGN OF THIS TEST.** Put it after, and a `reset` that LEAKED its
    // held rows would still pass: the re-subscribe export covers every surviving
    // key and replaces each one, so the leak is invisible. It becomes visible
    // only for a key the producer has STOPPED exporting — the stale row for a
    // thread that went away while the face was disconnected, which is precisely
    // the defect `apps/tui/src/store.rs`'s `invalidate()` exists to prevent and
    // precisely the reason that function can be deleted rather than ported.
    // `forced_red` 1 in this crate's report mutates `reset` to leak and this
    // ordering is what catches it.

    let mut never = Mirror::new(SHAPE);
    let mut dropped = Mirror::new(SHAPE);

    for m in [&mut never, &mut dropped] {
        m.accept(&export(
            "sam",
            "2026-09-25T12:00:00Z",
            json!({"share": 0.25}),
            json!({"share": known("2026-09-25T12:00:00Z")}),
        ));
        m.accept(&export(
            "work",
            "2026-09-25T12:00:00Z",
            json!({"share": 0.50}),
            json!({"share": known("2026-09-25T12:00:00Z")}),
        ));
    }

    // `never` folds the rest of the story. `dropped` sees none of it.
    never.accept(&live(
        Some("work"),
        "2026-09-25T12:01:00Z",
        json!({"kind": "gone"}),
    ));
    never.accept(&live(
        Some("sam"),
        "2026-09-25T12:02:00Z",
        json!({"kind": "row", "row": {"share": 0.75}}),
    ));
    never.accept(&live(
        Some("ada"),
        "2026-09-25T12:03:00Z",
        json!({"kind": "added"}),
    ));
    // **AND THE CONSUMER NAMES THE NEW KEY, WHICH IS A `subscribe` AND NOT A
    // REPAIR.** `added` is membership only — "a consumer at `detail: rows`
    // upgrades by NAMING the key" (`Derived::Added`'s own comment) — so the
    // honest live story has an export here, not a bare `row` frame. Writing it
    // as a bare `row` is what the FIRST run of this test did, and it went red
    // with `ada`'s cell map empty on this side and populated on the other. That
    // red was correct and it was about the TEST: a live `row` for a key this
    // connection was never exported is documented as unreachable (`drive` gates
    // live frames on having had the export) and, if it happens anyway, is held
    // deliberately WITHOUT provenance rather than with a fabricated `Known` —
    // see `a_live_row_for_a_never_exported_key_has_no_provenance` below, which
    // is the guard that discovery earned.
    never.accept(&export(
        "ada",
        "2026-09-25T12:03:00Z",
        json!({"share": 0.10}),
        json!({"share": known("2026-09-25T12:03:00Z")}),
    ));

    // ── THE SOCKET DIED. THIS IS THE WHOLE RECOVERY. ────────────────────────
    //
    // One `reset`, then the export a plain `shapes/accounts/subscribe` answers.
    // There is deliberately NOTHING between these two lines: no position read
    // off the old mirror, no `after_seq` computed from the last frame, no
    // "re-emit what I missed" verb, no park buffer replayed afterwards. If the
    // consumer needed any of that, this test could not be written.
    dropped.reset();
    dropped.accept(&export(
        "sam",
        "2026-09-25T12:05:00Z",
        json!({"share": 0.75}),
        json!({"share": known("2026-09-25T12:05:00Z")}),
    ));
    dropped.accept(&export(
        "ada",
        "2026-09-25T12:05:00Z",
        json!({"share": 0.10}),
        json!({"share": known("2026-09-25T12:05:00Z")}),
    ));

    // ── identical, on the half a face renders ───────────────────────────────
    assert_eq!(
        rendered(&never),
        rendered(&dropped),
        "a reconnected mirror must hold what a never-disconnected one holds; \
         if this fails, the repair machinery is load-bearing after all"
    );

    // And spelled out, so a failure says WHICH of the three went wrong.
    assert_eq!(never.keys(), dropped.keys(), "the key set");
    assert_eq!(never.row("sam"), &json!({"share": 0.75}), "never: sam");
    assert_eq!(dropped.row("sam"), &json!({"share": 0.75}), "dropped: sam");
    assert_eq!(never.row("ada"), &json!({"share": 0.10}), "never: ada");
    assert_eq!(dropped.row("ada"), &json!({"share": 0.10}), "dropped: ada");
    assert!(never.cell("sam", "share").is_known());
    assert!(dropped.cell("sam", "share").is_known());

    // ── and the two differences, asserted rather than hoped ─────────────────

    // 1. Provenance HONESTLY disagrees. `never` learned 0.75 at 12:02 off a
    //    live frame; `dropped` read it at 12:05 off an export.
    assert_ne!(
        never.held("sam").unwrap().cell("share").folded_at(),
        dropped.held("sam").unwrap().cell("share").folded_at(),
        "the stamps must differ — equal stamps would mean the consumer was \
         told when the PRODUCER knew rather than when IT learned"
    );

    // 2. `work` went away. `never` watched it go; `dropped` was not there.
    assert_eq!(
        never.key_state("work"),
        KeyState::Gone,
        "never watched `work` vanish"
    );
    assert_eq!(
        dropped.key_state("work"),
        KeyState::Unknown,
        "a re-subscribe does not re-narrate a vanishing the new connection \
         never witnessed; `Unknown` and `Gone` are deliberately two words"
    );
    assert!(never.vanished().contains(&"work".to_string()));
    assert!(dropped.vanished().is_empty());
}

/// **THE SECOND HALF OF 2.5: THE EXPORT IS COMPLETE, SO NOTHING HAS TO ASK
/// AGAIN.** A re-subscribe that answered a PARTIAL row would be the one thing
/// that puts a cursor back in a consumer, so completeness is asserted as the
/// property it is: an export REPLACES, and a field the producer stopped stating
/// is a field the consumer stops showing — never a field it merges from before
/// the disconnect.
#[test]
fn the_re_subscribe_export_replaces_and_never_merges_across_the_drop() {
    let mut m = Mirror::new(SHAPE);
    m.accept(&export(
        "sam",
        "2026-09-25T12:00:00Z",
        json!({"share": 0.25, "disabled": true, "note": "held before the drop"}),
        json!({"share": known("2026-09-25T12:00:00Z")}),
    ));
    assert_eq!(m.row("sam")["note"], json!("held before the drop"));

    m.reset();
    m.accept(&export(
        "sam",
        "2026-09-25T12:05:00Z",
        json!({"share": 0.25}),
        json!({"share": known("2026-09-25T12:05:00Z")}),
    ));

    assert_eq!(
        m.row("sam"),
        &json!({"share": 0.25}),
        "a merge across the reconnect would carry `disabled` and `note` the \
         producer has stopped stating — the stale-field defect `reset` exists \
         to make impossible"
    );
}

/// **AND THE ONE LOSS RULE 2.5 ADMITS IS STATED ON THE FRAME, NOT SILENT.** A
/// windowed `Sequence` whose consumer has a longer memory than the window loses
/// items on re-subscribe BY DESIGN; what the rule owes is that the consumer can
/// SEE that it happened. `covers_since` is that sentence.
#[test]
fn a_windowed_sequence_loses_items_on_re_subscribe_and_says_so() {
    let mut m = Mirror::new(SHAPE);
    m.accept(&export(
        "sam",
        "2026-09-25T12:00:00Z",
        json!({"share": 0.25}),
        json!({"share": known("2026-09-25T12:00:00Z")}),
    ));
    for i in 0..5u64 {
        m.accept(&live(
            Some("sam"),
            "2026-09-25T12:01:00Z",
            json!({"kind": "transition", "cell": "moves", "item": {"n": i}}),
        ));
    }
    let before = m.held("sam").unwrap().items("moves").len();
    assert_eq!(before, 5, "five transitions folded");

    // The re-subscribe export carries the window the PRODUCER retained — here,
    // the last two — and says since when it covers.
    m.reset();
    let mut e = export(
        "sam",
        "2026-09-25T12:05:00Z",
        json!({"share": 0.25}),
        json!({"share": known("2026-09-25T12:05:00Z")}),
    );
    e["cells"]["moves"] = json!({
        "state": "known", "complete": false,
        "folded_at": "2026-09-25T12:05:00Z",
        "covers_since": "2026-09-25T12:01:00Z",
        "items": [{"n": 3}, {"n": 4}],
    });
    m.accept(&e);

    let after = m.held("sam").unwrap().items("moves");
    assert!(
        after.len() < before,
        "the window is narrower than the consumer's memory, by design"
    );
    // The loss must be STATED — `complete: false` plus a `covers_since` that
    // names the oldest stamp still retained. A `complete: true` here would be
    // the silent truncation rule 2.8 forbids.
    match m.held("sam").unwrap().cell("moves") {
        shapewire::wire::CellState::Known {
            complete,
            covers_since,
            ..
        } => {
            assert!(!complete, "the truncation must be stated, not silent");
            assert!(
                covers_since.is_some(),
                "`covers_since` is the sentence that bounds any count a \
                 consumer derives from a window it did not see all of"
            );
        }
        other => panic!("the re-subscribe export carried `moves`: {other:?}"),
    }
}

/// **THE GUARD THE FIRST RED EARNED.** A live `row` for a key this connection
/// was never exported holds the ROW and NO PROVENANCE — every cell reads
/// `NotHydrated`, never a fabricated `Known`.
///
/// It is written down because it is the exact shape of the defect the terminal
/// face's `Store` header claims to be free of ("absence is a fact"), one layer
/// down and in the crate that will own it; and because inventing `Known` here
/// would make this file's identity proof pass for the wrong reason — a mirror
/// that lies about provenance agrees with everything.
#[test]
fn a_live_row_for_a_never_exported_key_has_no_provenance() {
    let mut m = Mirror::new(SHAPE);
    m.accept(&live(
        Some("ghost"),
        "2026-09-25T12:01:00Z",
        json!({"kind": "row", "row": {"share": 0.99}}),
    ));
    assert_eq!(m.row("ghost"), &json!({"share": 0.99}), "the row is real");
    assert!(
        !m.cell("ghost", "share").is_known(),
        "and its provenance is not — `Known` here would be the lie the \
         three-valued state exists to prevent"
    );
    assert!(
        m.held("ghost").unwrap().cells().is_empty(),
        "no cell entry is invented for a key no export described"
    );
}

/// **THE NON-VACUOUS HALF OF THE VANISHED ASSERTION, AND IT WAS A FORCING THAT
/// FOUND IT.**
///
/// `a_reconnected_mirror_equals_one_that_never_disconnected` ends with
/// `assert!(dropped.vanished().is_empty())`, and in that test's story the drop
/// happens BEFORE the vanishing — so `dropped` never had a vanished key to
/// begin with and that assertion passes whatever `reset` does with the set.
/// Mutating `reset` to LEAK `vanished` left the whole file green, which is how
/// the vacuity was found rather than shipped. This test is the version with
/// something to lose: the mirror WITNESSES a vanishing, then resets, and the
/// witness must be gone.
///
/// Why it matters for the terminal face: `vanished()` is what distinguishes
/// `Holding::Gone` from `Holding::Unknown`, and a leaked witness would make a
/// thread read "it went away" forever across every future reconnect — a stale
/// terminal state, which is worse than a stale value because nothing repaints
/// it.
///
/// `src/mirror/tests.rs`'s `reset_drops_everything_the_connection_held_and_keeps_the_contract`
/// checks `keys`, `exported`, `exports`, `state`, `members`, `last_frame_at` and
/// `resolved` — not `vanished`. This closes that gap.
#[test]
fn reset_clears_a_vanishing_this_connection_witnessed() {
    let mut m = Mirror::new(SHAPE);
    m.accept(&export(
        "work",
        "2026-09-25T12:00:00Z",
        json!({"share": 0.50}),
        json!({"share": known("2026-09-25T12:00:00Z")}),
    ));
    m.accept(&live(
        Some("work"),
        "2026-09-25T12:01:00Z",
        json!({"kind": "gone"}),
    ));
    assert_eq!(m.key_state("work"), KeyState::Gone, "witnessed");
    assert!(m.vanished().contains(&"work".to_string()), "recorded");

    m.reset();

    assert!(
        m.vanished().is_empty(),
        "a witness belongs to the connection that witnessed it; carried across \
         a reconnect it becomes a terminal state nothing ever repaints"
    );
    assert_eq!(
        m.key_state("work"),
        KeyState::Unknown,
        "after the reset the honest answer is `Unknown` — nobody on THIS \
         connection has said anything about `work` yet"
    );
}
