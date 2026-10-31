//! **DOES THE GOLDEN SUITE ACTUALLY BITE?**
//!
//! `tests/conformance.rs` locks what the reference answers. A lock that nothing
//! can break is decoration, and a BLESSED golden is especially easy to fool
//! yourself about: the expectations came out of the code they test, so of course
//! they match. This file is the forcing.
//!
//! Each test replays a transcript the suite already locks — the whole
//! transcript, in order, through the real `Held::fold` — with the FRAMES
//! transformed by one deliberate defect, and requires at least one locked
//! answer to disagree at the position it is locked at. Every defect is one a
//! hand-rolled store really commits; each is named in `docs/consumer-seam.md`
//! or `docs/shapes.md` as a thing a consumer got wrong. A green here would mean
//! the suite is blind to the exact class of defect it exists to catch.
//!
//! The replay is position-correct on purpose. An earlier version of this file
//! folded every frame and then compared against a MID-transcript expectation,
//! which made two tests agree by accident — a transcript's answers are
//! statements about a moment, and a harness that loses the moment is measuring
//! something else.
//!
//! ## And the real `Held::fold` was mutated too
//!
//! These transform the FRAMES, which proves the transcripts are sensitive to
//! the behaviour. The other half — that they are wired to the real function —
//! was done by hand, once, by editing `held.rs` itself; the reds are recorded
//! in `README.md` with their text. A mutation held permanently behind a feature
//! flag would be a second `fold` living in the reference, which is the one
//! thing the reference may not have.

use seam_reference::held::{Held, LinkState, Pending};
use seam_reference::{ledger, view};
use serde_json::Value;

// ─────────────────────────────────────────────────────────────────────────────
//  The replay, position-correct, with the frames passed through a transform
// ─────────────────────────────────────────────────────────────────────────────

/// Render one locked call. **The same renderer `conformance.rs` uses**, kept to
/// the calls the transcripts actually lock — a renderer that answered a
/// constant would make every test here pass, which is what
/// [`the_identity_transform_disagrees_with_nothing`] is for.
fn answer(h: &Held, call: &str, args: &[Value]) -> String {
    let s = |n: usize| args[n].as_str().unwrap_or_default().to_string();
    let u = |n: usize| args[n].as_u64().unwrap_or_default();
    match call {
        "link" => format!("{:?}", view::link(h)),
        "running" => format!("{:?}", view::running(h)),
        "running_count" => format!("{:?}", view::running_count(h)),
        "recent" => format!("{:?}", view::recent(h)),
        "accounts" => format!("{:?}", view::accounts(h)),
        "account_row" => format!("{:?}", view::account_row(h, &s(0))),
        "summary_truncated" => format!("{:?}", view::summary_truncated(h)),
        "holding" => format!("{:?}", view::holding(h, &s(0))),
        "status" => format!("{:?}", view::status(h, &s(0))),
        "status_transitions" => format!("{:?}", view::status_transitions(h, &s(0))),
        "liveness" => format!("{:?}", view::liveness(h, &s(0))),
        "positions" => format!("{:?}", view::positions(h, &s(0))),
        "settings" => format!("{:?}", view::settings(h, &s(0))),
        "account" => format!("{:?}", view::account(h, &s(0))),
        "header" => format!("{:?}", view::header(h, &s(0))),
        "title" => format!("{:?}", view::title(h, &s(0))),
        "app_payload" => format!("{:?}", view::app_payload(h, &s(0), &s(1))),
        "body_plan" => format!("{:?}", view::body_plan(h, &s(0))),
        "log_rows_len" => format!("{:?}", view::log_rows(h, &s(0)).map(|r| r.len())),
        "log_rows_kinds" => format!(
            "{:?}",
            view::log_rows(h, &s(0)).map(|r| r
                .iter()
                .map(|x| x.get("kind").and_then(Value::as_str).unwrap_or("?").to_string())
                .collect::<Vec<_>>())
        ),
        "log_covers_from" => format!("{:?}", view::log_covers_from(h, &s(0))),
        "ledger_rows_after" => format!(
            "{:?}",
            ledger::rows_after(h, &s(0), u(1))
                .iter()
                .map(|r| (r.seq, r.kind.clone(), r.elided))
                .collect::<Vec<_>>()
        ),
        "ledger_floor" => format!("{:?}", ledger::floor(h, &s(0))),
        "ledger_capacity" => format!("{:?}", ledger::capacity(h)),
        "ledger_open" => format!("{:?}", ledger::open(h)),
        "narration_len" => format!("{:?}", view::narration(h, &s(0), None).map(|l| l.len())),
        "settled" => format!("{:?}", view::settled(h, u(0))),
        "era" => format!("{:?}", h.era(&s(0))),
        "entry" => format!(
            "{:?}",
            h.book
                .get(&s(0), args.get(1).and_then(Value::as_str))
                .map(|e| e.status().clone())
        ),
        "rejections" => format!("{:?}", h.rejections),
        "keys_of" => format!("{:?}", h.book.keys_of(&s(0))),
        "mirror_keys" => format!("{:?}", h.mirror(&s(0)).map(|m| m.keys()).unwrap_or_default()),
        "mirror_dead" => format!("{:?}", h.mirror(&s(0)).map(|m| m.dead())),
        "mirror_refusal" => format!(
            "{:?}",
            h.mirror(&s(0)).and_then(|m| m.refusal()).map(str::to_string)
        ),
        "mirror_exports" => format!("{:?}", h.mirror(&s(0)).map(|m| m.exports())),
        "key_state" => format!("{:?}", h.mirror(&s(0)).map(|m| m.key_state(&s(1)))),
        "vanished" => format!("{:?}", h.mirror(&s(0)).map(|m| m.vanished()).unwrap_or_default()),
        other => panic!("fold_mutations: no renderer for `{other}`"),
    }
}

/// Replay a transcript with `mutate` applied to every frame. `None` drops the
/// frame, which is itself one of the defects. Answers the disagreements, each
/// with the position it was locked at.
fn replay(name: &str, mutate: impl Fn(Value) -> Option<Value>) -> Vec<String> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("conformance")
        .join(format!("{name}.jsonl"));
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let mut h = Held::new();
    h.link = LinkState::Up {
        boot_id: "transcript".into(),
        since: "2026-10-07T00:00:00Z".parse().unwrap(),
    };
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("{name}:{}: {e}", i + 1));
        if let Some(f) = v.get("frame") {
            if let Some(f) = mutate(f.clone()) {
                h.fold(&f);
            }
        } else if let Some(r) = v.get("reset") {
            h.reset(r.as_str().unwrap_or("the socket died").to_string());
        } else if let Some(hd) = v.get("hold") {
            let shape: &'static str =
                Box::leak(hd["shape"].as_str().unwrap().to_string().into_boxed_str());
            h.book.hold(shape, hd.get("key").and_then(Value::as_str));
        } else if let Some(pr) = v.get("promise") {
            let verb: &'static str =
                Box::leak(pr["verb"].as_str().unwrap_or("send").to_string().into_boxed_str());
            h.promises.open(
                pr["id"].as_u64().unwrap(),
                Pending {
                    verb,
                    subject: pr.get("subject").and_then(Value::as_str).map(str::to_string),
                    asked_at: "2026-10-07T00:00:00Z".parse().unwrap(),
                    boot_id: "transcript".into(),
                },
            );
        } else if let Some(e) = v.get("expect") {
            let call = e["call"].as_str().unwrap();
            let args: Vec<Value> = e
                .get("args")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let want = e["value"].as_str().unwrap_or_default();
            let got = answer(&h, call, &args);
            if got != want {
                out.push(format!("{name}:{}: {call} want {want} got {got}", i + 1));
            }
        }
    }
    out
}

fn reds(name: &str, mutate: impl Fn(Value) -> Option<Value>) -> Vec<String> {
    let r = replay(name, mutate);
    for line in &r {
        println!("  RED {line}");
    }
    r
}

const T1: &str = "11111111-1111-1111-1111-111111111111";

// ─────────────────────────────────────────────────────────────────────────────
//  The opposing forcing, FIRST, because six "it must disagree" tests would all
//  pass against a harness that always disagrees.
// ─────────────────────────────────────────────────────────────────────────────

/// Every transcript, replayed through the IDENTITY transform, agrees with its
/// lock at every position. So the harness is not simply reporting disagreement
/// — and this is the test that fails if `answer` returns a constant, if
/// `replay` loses a directive, or if a transcript's expectations were blessed
/// at the wrong moment.
#[test]
fn the_identity_transform_disagrees_with_nothing() {
    for name in ["edges", "parent_era", "promise_reconnect", "one_thread", "absent_key"] {
        let r = replay(name, Some);
        assert!(
            r.is_empty(),
            "the identity replay of {name} disagreed with its own lock:\n{}",
            r.join("\n")
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  M1 — an export that MERGES instead of replacing
// ─────────────────────────────────────────────────────────────────────────────

/// The mutation a hand-rolled store actually commits, and the one
/// `tests/resubscribe_identity.rs` records as having left a sibling's file
/// GREEN until a test was written for it: an export must REPLACE a key's row
/// and its provenance. Modelled by dropping a cell the later export carries, as
/// a merging store effectively does by never clearing what the new export
/// omits.
#[test]
fn merging_an_export_instead_of_replacing_it_goes_red() {
    let r = reds("edges", |mut f| {
        if f["op"] == "read" {
            if let Some(row) = f.get_mut("row").and_then(|r| r.as_object_mut()) {
                row.remove("unread");
            }
            if let Some(c) = f.get_mut("cells").and_then(|r| r.as_object_mut()) {
                c.remove("unread");
            }
        }
        Some(f)
    });
    assert!(!r.is_empty(), "the suite did not notice an export that lost a cell");
    assert!(r.iter().any(|x| x.contains("positions")), "{r:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
//  M2 — a transition folded into the level cell
// ─────────────────────────────────────────────────────────────────────────────

/// **W6, VERBATIM.** `docs/consumer-seam.md` §8: *"`shapes.py` appending
/// transitions into `row[cell]`. Wrong for `threads`; the Rust mirror holds them
/// apart."* The seam inherits the right behaviour from the vendored mirror, and
/// the whole reason to vendor it is that this cannot be re-derived wrongly — so
/// the suite has to be able to tell.
#[test]
fn folding_a_transition_as_a_row_goes_red() {
    let r = reds("edges", |mut f| {
        if f["event"]["kind"] == "transition" {
            let item = f["event"]["item"].clone();
            f["event"] = serde_json::json!({
                "kind": "row",
                "row": {"thread_id": T1, "status_transitions": item},
            });
        }
        Some(f)
    });
    assert!(!r.is_empty(), "the suite did not notice transitions folded as a row");
    assert!(r.iter().any(|x| x.contains("status_transitions")), "{r:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
//  M3 — a `gone` read as "not loaded yet"
// ─────────────────────────────────────────────────────────────────────────────

/// `KeyState`'s own doc: *"A UI that cannot tell removed from not yet loaded
/// either shows a stale row for ever or blanks a row that is merely slow."*
/// Modelled by dropping every `gone`, which is what a store with two values
/// instead of three effectively does.
#[test]
fn dropping_a_gone_goes_red() {
    let r = reds("edges", |f| {
        if f["event"]["kind"] == "gone" {
            None
        } else {
            Some(f)
        }
    });
    assert!(!r.is_empty(), "the suite did not notice a dropped `gone`");
    assert!(r.iter().any(|x| x.contains("vanished") || x.contains("key_state")), "{r:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
//  M4 — a frame LOST on a word this consumer does not know
// ─────────────────────────────────────────────────────────────────────────────

/// `mirror.rs`'s leniency contract: an unknown `event.kind`, `members.state`,
/// `op` or cell-state word is stripped and COUNTED rather than losing the
/// frame — because a frame thrown away takes its `promises` and `subscribes`
/// with it. Modelled by dropping any frame carrying an unknown word, which is
/// what `serde_json::from_value::<CellState>` would have done unaided.
#[test]
fn losing_a_frame_on_an_unknown_word_goes_red() {
    let r = reds("edges", |f| {
        let raw = f.to_string();
        if raw.contains("a_kind_from_a_newer_producer")
            || raw.contains("a_state_word_from_the_future")
        {
            None
        } else {
            Some(f)
        }
    });
    assert!(
        !r.is_empty(),
        "the suite did not notice frames dropped on a word this consumer does not know"
    );
    assert!(r.iter().any(|x| x.contains("rejections")), "{r:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
//  M5 — a promise resolved off the reply instead of off the frame
// ─────────────────────────────────────────────────────────────────────────────

/// Rule 2.4's whole point: *a command acknowledges, never answers with a
/// fact*, so a promise is settled by the FRAME that carries its id. Modelled by
/// stripping `promises` off every frame — a consumer that settled on the reply
/// would never read them, and `view::settled` would say `Pending` for ever,
/// including across the reconnect where the re-offered id is the only evidence
/// the command took effect.
#[test]
fn ignoring_a_frames_promises_goes_red() {
    let r = reds("promise_reconnect", |mut f| {
        f.as_object_mut().unwrap().remove("promises");
        Some(f)
    });
    assert!(
        !r.is_empty(),
        "the suite did not notice a frame's promises being ignored"
    );
    assert!(r.iter().any(|x| x.contains("settled")), "{r:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
//  M6 — the parent-era question collapsed
// ─────────────────────────────────────────────────────────────────────────────

/// `view::era`'s three answers are the only thing that lets a face holding a
/// parent AND a child say whether an inference across the two is from this era.
/// Modelled by stripping `parent_epoch`, which collapses `ParentMovedOn` into
/// `NoParent` — *looks fine*.
#[test]
fn stripping_parent_epoch_goes_red() {
    let r = reds("parent_era", |mut f| {
        if let Some(e) = f.get_mut("epoch").and_then(|e| e.as_object_mut()) {
            e.remove("parent_epoch");
        }
        Some(f)
    });
    assert!(!r.is_empty(), "the suite did not notice `parent_epoch` going missing");
    assert!(r.iter().any(|x| x.contains("era")), "{r:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
//  M7 — the window read off the wrong end
// ─────────────────────────────────────────────────────────────────────────────

/// `threads/log`'s `Sequence` is a window whose ORDER is its meaning (rule
/// 2.2): *"`off` then `on` is a move; the reverse is the opposite move"*.
/// Modelled by reversing the exported items — a face that assembled a reply out
/// of a reversed window would speak the paragraph backwards, and this is the
/// transcript that stops it being a silent change.
#[test]
fn reversing_the_log_window_goes_red() {
    let r = reds("parent_era", |mut f| {
        if f["shape"] == "threads/log" {
            if let Some(items) = f["row"]["events"].as_array().cloned() {
                f["row"]["events"] = Value::Array(items.into_iter().rev().collect());
            }
        }
        Some(f)
    });
    assert!(!r.is_empty(), "the suite did not notice a reversed window");
    assert!(
        r.iter()
            .any(|x| x.contains("log_rows_kinds") || x.contains("ledger_rows_after")),
        "{r:?}"
    );
}
