//! # THE MIRROR, DIFFERED AGAINST THE ONLY OTHER IMPLEMENTATION
//!
//! `mirror::Mirror` is a SECOND implementation of a contract whose only prior
//! implementation is `lib/threads/scripts/threads-app/threads_tui/shapes.py`
//! (427 lines of Python, one shape). Two implementations of one contract drift,
//! and ending that drift is the entire reason the mirror was written into the
//! chassis rather than into each consumer — so the only real evidence the twin
//! has not already drifted is to drive the SAME frame sequences through BOTH and
//! assert the same resulting state.
//!
//! **Every difference is a finding, not a thing to match.** Each scenario below
//! declares the JSON paths at which the two are EXPECTED to disagree, with which
//! one is right and why. Those paths are lifted out of both dumps before the
//! rest is compared for equality — and each one is separately asserted to
//! actually still differ, so a divergence that someone later fixes on either
//! side fails this test rather than passing it silently.
//!
//! ## Where the oracle lives, and why this is not a path dependency
//!
//! `shapewire` must gain no dependency on a producer crate: it is meant to
//! leave this workspace with a release. The oracle is a FILE in a producer's
//! tree, not a crate, and it is read by a python process this test spawns —
//! nothing is linked and no `Cargo.toml` changed. When that file is not there
//! (a released crate, a checkout without the TUI) the test falls back to
//! `tests/oracle/expected.json`, a committed transcript of what the Python
//! produced, and asserts against that instead. **Neither path is a skip.** When
//! the Python IS present, the transcript is also re-checked against it, so it
//! cannot rot into a golden that only agrees with itself.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use shapewire::mirror::Mirror;
use shapewire::wire::CellState;
use serde_json::{json, Map, Value};

// ─────────────────────────────────────────────────────────────────────────────
// The canonical dump — the same fields out of both implementations
// ─────────────────────────────────────────────────────────────────────────────

fn stamp(t: Option<shapewire::wire::Timestamp>) -> Value {
    match t {
        // Six fractional digits always, matching `drive_shapes_py.py::stamp`, so
        // that a difference in the dump is a difference in the STATE and never in
        // two libraries' default formatting.
        Some(t) => json!(t.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()),
        None => Value::Null,
    }
}

/// One cell, in `shapes.py::CellState`'s own field set: `complete`,
/// `since_init` and `covers_since` are QUALIFIERS on `Known`, not sibling
/// states, and the Python dataclass defaults them for the other two.
fn cell(c: &CellState) -> Value {
    match c {
        CellState::Known {
            complete,
            folded_at,
            since_init,
            covers_since,
        } => json!({
            "kind": "known",
            "complete": complete,
            "folded_at": stamp(Some(*folded_at)),
            "since_init": since_init,
            "covers_since": stamp(*covers_since),
            "reason": Value::Null,
        }),
        CellState::NotHydrated { reason } => json!({
            "kind": "not_hydrated", "complete": true, "folded_at": Value::Null,
            "since_init": false, "covers_since": Value::Null, "reason": reason,
        }),
        CellState::Dead { reason } => json!({
            "kind": "dead", "complete": true, "folded_at": Value::Null,
            "since_init": false, "covers_since": Value::Null, "reason": reason,
        }),
    }
}

/// **THE ROW AS A READER SEES IT**, which is where the two implementations are
/// compared.
///
/// `shapes.py` appends a transition item straight into `row[cell]`; this mirror
/// holds appended items apart and merges them at read time, and ONLY where the
/// row field is absent or already a list. So the Python `row` is compared
/// against this, and the one shape where the two cannot agree — `threads`, whose
/// transition cell is `status` while its row spells the level `status` and the
/// log `status_transitions` — is a declared divergence.
fn merged_row(h: &shapewire::mirror::Held) -> Value {
    let mut row = match h.row() {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    for c in h.transitioned() {
        let mergeable = !matches!(row.get(&c), Some(v) if !v.is_array());
        if mergeable {
            row.insert(c.clone(), Value::Array(h.items(&c)));
        }
    }
    Value::Object(row)
}

fn dump(m: &Mirror, moved: &[bool]) -> Value {
    let mut held = Map::new();
    for key in m.keys() {
        let h = m.held(&key).expect("a listed key is held");
        held.insert(
            key.clone(),
            json!({
                "row": merged_row(h),
                "as_of": stamp(h.as_of()),
                "at": stamp(h.at()),
                "dead_reason": h.dead_reason(),
                "cells": h.cells().iter().map(|(n, c)| (n.clone(), cell(c)))
                                  .collect::<Map<_, _>>(),
            }),
        );
    }
    json!({
        "name": m.name(),
        "state": match m.state() {
            shapewire::wire::ShapeState::Known => "known",
            shapewire::wire::ShapeState::NotHydrated => "not_hydrated",
            shapewire::wire::ShapeState::Dead => "dead",
        },
        "dead_reason": m.dead_reason(),
        "init_at": stamp(m.epoch().init_at),
        "reinit": m.epoch().reinit,
        "lost": m.epoch().lost,
        "exported": m.exported(),
        "exports": m.exports(),
        "last_frame_at": stamp(m.last_frame_at()),
        "shared": m.shared(),
        "shared_cells": m.shared_cells().iter().map(|(n, c)| (n.clone(), cell(c)))
                         .collect::<Map<_, _>>(),
        "keys": m.keys(),
        "held": held,
        "moved": moved,
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// The scenarios
// ─────────────────────────────────────────────────────────────────────────────

struct Scenario {
    name: &'static str,
    shape: &'static str,
    frames: Vec<Value>,
    /// `(json pointer, which one is right and why)`. Lifted out of both dumps
    /// before the rest is compared, and separately asserted to still differ.
    diverges: Vec<(&'static str, &'static str)>,
}

fn export(shape: &str, key: &str, as_of: &str, row: Value, cells: Value) -> Value {
    json!({
        "type": "shape", "shape": shape, "key": key, "op": "read", "as_of": as_of,
        "epoch": {"init_at": "2026-09-25T11:59:59Z", "reinit": 0, "lost": 0, "state": "known"},
        "row": row, "cells": cells,
    })
}

fn live(shape: &str, key: Option<&str>, at: &str, event: Value) -> Value {
    let mut v = json!({
        "type": "shape", "shape": shape, "op": "live", "at": at,
        "epoch": {"reinit": 0, "lost": 0, "state": "known"}, "event": event,
    });
    if let Some(k) = key {
        v["key"] = json!(k);
    }
    v
}

fn known(at: &str) -> Value {
    json!({"state": "known", "complete": true, "folded_at": at})
}

fn scenarios() -> Vec<Scenario> {
    vec![
        // ── 1. The one-line rule, and nothing else ──────────────────────────
        Scenario {
            name: "hydrate_replace_and_leave",
            shape: "accounts",
            frames: vec![
                export(
                    "accounts", "sam", "2026-09-25T12:00:00Z",
                    json!({"share": 0.25, "disabled": false, "threads": []}),
                    json!({
                        "share": known("2026-09-25T12:00:00Z"),
                        "threads": {"state": "known", "complete": false,
                                    "folded_at": "2026-09-25T12:00:00Z",
                                    "since_init": true,
                                    "covers_since": "2026-09-25T11:00:00Z"},
                        "credential": {"state": "not_hydrated", "reason": "no login dir"},
                    }),
                ),
                export(
                    "accounts", "kim", "2026-09-25T12:00:01Z",
                    json!({"share": 0.75}), json!({"share": known("2026-09-25T12:00:01Z")}),
                ),
                // An export REPLACES: `disabled` is gone and must stop showing.
                export(
                    "accounts", "sam", "2026-09-25T12:03:00Z",
                    json!({"share": 0.5}), json!({"share": known("2026-09-25T12:03:00Z")}),
                ),
                live("accounts", Some("kim"), "2026-09-25T12:04:00Z",
                     json!({"kind": "row", "row": {"share": 0.9}})),
                live("accounts", Some("kim"), "2026-09-25T12:05:00Z", json!({"kind": "gone"})),
                // A `gone` for a key nobody holds. **A DECLARED DIVERGENCE since
                // 2026-10-02** - see `diverges` below.
                live("accounts", Some("ghost"), "2026-09-25T12:06:00Z", json!({"kind": "gone"})),
            ],
            diverges: vec![(
                "/moved/5",
                "A `gone` FOR A KEY NOTHING WAS HELD FOR MOVES THE RUST MIRROR AND \
                 NOT THE PYTHON ONE, AND THE RUST ONE IS RIGHT.\n\n\
                 `shapes.py` has two places a `gone` can land - the held dict and \
                 the roster - so for a key in neither it genuinely changes nothing \
                 and `False` is honest about ITS model. The Rust mirror has a \
                 third: `KeyState`, which separates *the producer removed this \
                 key* from *nobody has said anything about this key* \
                 (`mirror::KeyState`). This frame moves that reader from `Unknown` \
                 to `Gone`, which is precisely the fact a consumer waiting for a \
                 row it will never receive needs, so `moved: true` is the honest \
                 answer here.\n\n\
                 It is a divergence in CAPABILITY, not in correctness: the Rust \
                 mirror holds a fact the reference cannot represent. Saying the \
                 same `gone` twice still moves nothing on both sides.",
            )],
        },
        // ── 2. The death notice ─────────────────────────────────────────────
        Scenario {
            name: "death_keeps_every_row",
            shape: "accounts",
            frames: vec![
                {
                    let mut e = export(
                        "accounts", "sam", "2026-09-25T12:00:00Z",
                        json!({"share": 0.25}), json!({"share": known("2026-09-25T12:00:00Z")}),
                    );
                    e["shared"] = json!({"warn_below_days": 3});
                    e["shared_cells"] = json!({"warn_below_days": known("2026-09-25T12:00:00Z")});
                    e
                },
                export("accounts", "kim", "2026-09-25T12:00:01Z", json!({"share": 0.75}), json!({})),
                json!({
                    "type": "shape", "shape": "accounts", "op": "read",
                    "as_of": "2026-09-25T12:10:00Z",
                    "epoch": {"init_at": "2026-09-25T11:00:00Z", "reinit": 1, "lost": 2,
                              "state": "dead", "dead_reason": "the shape task panicked"},
                }),
            ],
            diverges: vec![],
        },
        // ── 3. A row with no export, and provenance that must not be invented ─
        Scenario {
            name: "unexported_row_and_unknown_cell_word",
            shape: "accounts",
            frames: vec![
                live("accounts", Some("stranger"), "2026-09-25T12:01:00Z",
                     json!({"kind": "row", "row": {"share": 0.1}})),
                export(
                    "accounts", "sam", "2026-09-25T12:02:00Z", json!({"share": 0.25}),
                    json!({"share": {"state": "estimated",
                                     "folded_at": "2026-09-25T12:02:00Z"}}),
                ),
                // A shape-state word neither side has ever heard of. BOTH keep
                // the state they had; only this mirror also counts it.
                json!({
                    "type": "shape", "shape": "accounts", "op": "read",
                    "as_of": "2026-09-25T12:03:00Z",
                    "epoch": {"reinit": 0, "lost": 0, "state": "quiescing"},
                }),
            ],
            diverges: vec![(
                "/held/sam/cells/share/reason",
                "cosmetic and ours: both refuse to read an unknown word as Known and both say \
                 which word it was; `shapes.py` quotes it with Python's `repr` ('estimated') and \
                 this mirror with Rust's `Debug` (\"estimated\"). Neither is wrong; matching the \
                 other language's quoting would be the only reason to change either.",
            )],
        },
        // ── 4. The transition stamp ─────────────────────────────────────────
        Scenario {
            name: "transition_stamped_from_the_frame",
            shape: "accounts",
            frames: vec![
                export(
                    "accounts", "sam", "2026-09-25T12:00:00Z",
                    json!({"moves": [{"from": "idle", "to": "busy",
                                      "at": "2026-09-25T11:00:00+00:00"}]}),
                    json!({"moves": known("2026-09-25T12:00:00Z")}),
                ),
                live("accounts", Some("sam"), "2026-09-25T12:01:00Z",
                     json!({"kind": "transition", "cell": "moves",
                            "item": {"from": "busy", "to": "idle"}})),
                // An item that brought its own stamp keeps it, on both sides.
                live("accounts", Some("sam"), "2026-09-25T12:02:00Z",
                     json!({"kind": "transition", "cell": "moves",
                            "item": {"to": "busy", "at": "2026-09-25T09:00:00+00:00"}})),
                // A transition for a key with no export. `shapes.py` drops the
                // item and answers `False`; this mirror drops it too and says
                // `no_export` on the ack, which is the difference rule 2.3 is
                // about and which no state dump can show.
                live("accounts", Some("ghost"), "2026-09-25T12:03:00Z",
                     json!({"kind": "transition", "cell": "moves", "item": {"to": "idle"}})),
            ],
            diverges: vec![(
                "/held/sam/row/moves/1/at",
                "OURS. The graft exists to make an appended item indistinguishable from an \
                 exported one, and `Sequence::to_json` (fold.rs:490) writes a retained item's \
                 stamp with `to_rfc3339()` — `2026-09-25T12:01:00+00:00`. `shapes.py` grafts the \
                 FRAME's `at` string verbatim, which chrono serialises as `...Z`, so in the \
                 Python mirror the two are still told apart by their spelling.",
            )],
        },
        // ── 5. THE LANDMINE ─────────────────────────────────────────────────
        Scenario {
            name: "threads_status_level_versus_log",
            shape: "threads",
            frames: vec![
                export(
                    "threads", "t1", "2026-09-25T12:00:00Z",
                    json!({"status": {"level": "running", "since": "2026-09-25T11:00:00Z"},
                           "status_transitions": [{"to": "running",
                                                   "at": "2026-09-25T11:00:00+00:00"}]}),
                    json!({"status": known("2026-09-25T12:00:00Z"),
                           "status_transitions": known("2026-09-25T12:00:00Z")}),
                ),
                live("threads", Some("t1"), "2026-09-25T12:01:00Z",
                     json!({"kind": "transition", "cell": "status",
                            "item": {"from": "running", "to": "idle"}})),
            ],
            diverges: vec![(
                "/held/t1/row/status",
                "OURS, and this is the landmine a verbatim port would have walked into. The \
                 transition frame names the CELL `status` (threads.rs:591); the row spells the \
                 LEVEL as `status` (:418) and the LOG as `status_transitions` (:419). \
                 `shapes.py:346-349` appends into `row[cell]`, so it REPLACES the held level \
                 object with a one-item list on the first transition frame. The chassis contract \
                 is silent on how a transition's cell name maps to a row field and the mapping is \
                 each shape's choice, so this mirror holds appended items apart and merges only \
                 where the row field is absent or already a list.",
            )],
        },
        // ── 6. What the reference mirror cannot see at all ──────────────────
        Scenario {
            name: "membership_promises_and_subscribes",
            shape: "threads",
            frames: vec![
                json!({
                    "type": "shape", "shape": "threads", "op": "read",
                    "as_of": "2026-09-25T12:00:00Z", "subscribes": [9],
                    "epoch": {"reinit": 0, "lost": 0, "state": "known"},
                    "members": {"state": "known", "keys": ["a", "b"],
                                "complete": false, "next": "c"},
                }),
                json!({
                    "type": "shape", "shape": "threads", "op": "read",
                    "as_of": "2026-09-25T12:00:01Z", "subscribes": [9], "complete": true,
                    "epoch": {"reinit": 0, "lost": 0, "state": "known"},
                    "members": {"state": "known", "keys": ["c"], "complete": true},
                }),
                live("threads", Some("d"), "2026-09-25T12:01:00Z", json!({"kind": "added"})),
                {
                    let mut e = export("threads", "a", "2026-09-25T12:02:00Z",
                                       json!({"account": "default"}), json!({}));
                    e["promises"] = json!([7, 9]);
                    e
                },
                {
                    let mut e = export("threads", "a", "2026-09-25T12:03:00Z",
                                       json!({"account": "default"}), json!({}));
                    // The RE-OFFER: an export is a carrier of last resort for
                    // every id still owed, so 7 and 9 come round again.
                    e["promises"] = json!([7, 9, 11]);
                    e
                },
            ],
            diverges: vec![
                (
                    "/moved/2",
                    "OURS. `added` is a `kind` `shapes.py` predates entirely, so `_live` falls \
                     through to `return False` and the arrival is lost; this mirror writes it \
                     into the roster. `shapes.py` reads neither `members` nor `added` nor \
                     `promises` nor `subscribes` (shape-surface.md 8 finding 16) — the reference \
                     is behind the wire it is the reference for.",
                ),
            ],
        },
    ]
}

// ─────────────────────────────────────────────────────────────────────────────
// Running both sides
// ─────────────────────────────────────────────────────────────────────────────

fn rust_dump(s: &Scenario) -> Value {
    let mut m = Mirror::new(s.shape);
    let moved: Vec<bool> = s.frames.iter().map(|f| m.accept(f).moved).collect();
    dump(&m, &moved)
}

fn app_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../services/threads/scripts/threads-app")
}

fn python_dump(s: &Scenario) -> Option<Value> {
    let app = app_dir();
    if !app.join("threads_tui/shapes.py").exists() {
        return None;
    }
    let scratch = std::env::temp_dir().join(format!("mirror-oracle-{}-{}", std::process::id(), s.name));
    std::fs::write(
        &scratch,
        serde_json::to_vec(&json!({"shape": s.shape, "frames": s.frames})).unwrap(),
    )
    .expect("write the scenario");
    let driver = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/oracle/drive_shapes_py.py");
    let out = Command::new("python3")
        .arg(&driver)
        .arg(&app)
        .arg(&scratch)
        .output()
        .ok()?;
    let _ = std::fs::remove_file(&scratch);
    assert!(
        out.status.success(),
        "the oracle is PRESENT and did not run — that is a broken test, not a missing one.\n\
         {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    Some(serde_json::from_slice(&out.stdout).expect("the oracle's dump is JSON"))
}

fn transcript() -> BTreeMap<String, Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/oracle/expected.json");
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        panic!("the committed oracle transcript is the fallback and it must exist: {path:?}: {e}")
    });
    serde_json::from_slice(&bytes).expect("the transcript is JSON")
}

/// Remove the value at a JSON pointer, returning it. Used to lift a declared
/// divergence out of both dumps before the rest is compared.
fn take(v: &mut Value, pointer: &str) -> Option<Value> {
    let (parent, last) = pointer.rsplit_once('/')?;
    let target = if parent.is_empty() {
        v
    } else {
        v.pointer_mut(parent)?
    };
    match target {
        Value::Object(m) => m.remove(last),
        Value::Array(a) => last.parse::<usize>().ok().and_then(|i| {
            if i < a.len() {
                Some(a.remove(i))
            } else {
                None
            }
        }),
        _ => None,
    }
}

/// **THE DIFFERENTIAL.** Same frames, both mirrors, same state — or a declared
/// difference with a reason and a verdict.
#[test]
fn the_rust_mirror_and_the_python_reference_agree_frame_for_frame() {
    let committed = transcript();
    let mut live_runs = 0usize;

    for s in scenarios() {
        let mine = rust_dump(&s);
        let theirs = match python_dump(&s) {
            Some(v) => {
                live_runs += 1;
                assert_eq!(
                    committed.get(s.name),
                    Some(&v),
                    "`{}`: the committed transcript no longer matches what `shapes.py` actually \
                     produces. Regenerate it — `tests/oracle/regenerate.sh` — rather than \
                     leaving a golden that only agrees with itself.",
                    s.name
                );
                v
            }
            None => committed
                .get(s.name)
                .unwrap_or_else(|| panic!("`{}` is not in the committed transcript", s.name))
                .clone(),
        };

        let mut mine_trimmed = mine.clone();
        let mut theirs_trimmed = theirs.clone();
        for (pointer, verdict) in &s.diverges {
            let a = take(&mut mine_trimmed, pointer);
            let b = take(&mut theirs_trimmed, pointer);
            assert_ne!(
                a, b,
                "`{}` declares a divergence at `{pointer}` that is no longer there. \
                 If it was fixed, delete the declaration; the reason it was declared was:\n{verdict}",
                s.name
            );
        }
        assert_eq!(
            mine_trimmed, theirs_trimmed,
            "`{}`: the two mirrors disagree somewhere UNDECLARED. Every difference is a finding: \
             work out which one is right, fix that one, and declare it here if it is meant to \
             stand.\n  rust:   {mine}\n  python: {theirs}",
            s.name
        );
    }

    // Whichever path ran, something real was compared.
    assert!(
        live_runs == scenarios().len() || live_runs == 0,
        "the oracle ran for some scenarios and not others, which means the comparison is partly \
         against a transcript and partly against the file — and a failure could be either"
    );
}

/// The scenarios, as JSON, for `tests/oracle/regenerate.sh`. Ignored by default:
/// it asserts nothing and exists so the transcript is generated FROM the test's
/// own scenarios rather than from a second copy of them.
#[test]
#[ignore = "a generator, not an assertion — see tests/oracle/regenerate.sh"]
fn dump_scenarios() {
    let all: BTreeMap<&str, Value> = scenarios()
        .iter()
        .map(|s| (s.name, json!({"shape": s.shape, "frames": s.frames})))
        .collect();
    println!("{}", serde_json::to_string_pretty(&all).unwrap());
}

/// The things the reference mirror cannot see at all, asserted on this side
/// alone — there is no Python state to compare them against, which is the point.
#[test]
fn the_mirror_holds_what_the_reference_is_blind_to() {
    let s = scenarios()
        .into_iter()
        .find(|s| s.name == "membership_promises_and_subscribes")
        .unwrap();
    let mut m = Mirror::new(s.shape);
    let receipts: Vec<_> = s.frames.iter().map(|f| m.accept(f)).collect();

    // Membership: three pages' worth of keys plus one `added`, and the walk
    // closed on the page that said so.
    assert_eq!(m.members().keys(), vec!["a", "b", "c", "d"]);
    assert!(m.members().complete());
    assert_eq!(m.members().resume_from(), None);
    // ...and the first page's hole was nameable while the walk was open.
    assert_eq!(receipts[0].complete, None);

    // Rule 2.7: the subscribe that asked is matched by the export that answers.
    assert_eq!(
        m.subscribe(9),
        shapewire::mirror::SubscribeStatus::Complete
    );

    // Rule 2.4: the re-offer resolves only what was still owed.
    assert_eq!(receipts[3].promises, vec![7, 9]);
    assert_eq!(receipts[4].promises, vec![11]);
    assert_eq!(m.resolved_promises(), vec![7, 9, 11]);

    // And a key exported is a key that exists, even with no membership frame.
    assert!(m.members().contains("a"));
}
