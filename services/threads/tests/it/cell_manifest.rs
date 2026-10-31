//! # THE CELL MANIFEST, BOUND TO THE ROWS — the producer-side sentinel
//!
//! `shapewire::cell` declares what a shape CARRIES and checks the declaration's
//! well-formedness at compile time. What the chassis cannot check is the PAIRING:
//! that `Root::cells` emits exactly the names the manifest declares — the same
//! hole `MAX_RETAINED_ITEMS`/`Root::row_at` has, for the same reason (`Root`
//! exposes no cell types). This file is that check, one assertion per shape, in
//! the producer's own suite, and it is the POSITIVE SENTINEL for the whole
//! mechanism: a manifest that drifts from the row builder is red here before any
//! consumer vendors it.
//!
//! Three more things live here because they are the same question one layer
//! down — a declared vocabulary must be the vocabulary the code emits:
//!
//! * the five state words the `threads` and `threads/non-idle` shapes declare are the
//!   five `ThreadManager::thread_states` derives;
//! * the ledger kinds the `threads/log` shape declares are the arms of
//!   `ThreadEvent::kind`;
//! * the offline manifest tool lists every shape the daemon registers.
//!
//! **Every test here was forced red by the mutation named in its own docstring,
//! and the red was checked for the message before it was believed.**

use std::collections::BTreeSet;

use serde_json::Value;
use shapewire::cell::Cells;
use shapewire::fold::Root;
use shapewire::hub::Shape;
use threads::shape::Input;
use threads::shapes::{
    accounts::{Accounts, AccountsRoot},
    gauge_history::{GaugeHistory, GaugeHistoryRoot},
    heartbeat::{Heartbeat, HeartbeatRoot},
    log::{Log, LogRoot},
    boot::{Boot, BootRoot},
    narration::{Narration, NarrationRoot},
    recent::{Recent, RecentRoot},
    summary::{Summary, SummaryRoot},
    threads::{Threads, ThreadsRoot},
    warnings::{DaemonWarningsRoot, Warnings},
};

fn keys_of(v: &Value) -> BTreeSet<String> {
    v.as_object()
        .unwrap_or_else(|| panic!("a cells block is an object: {v}"))
        .keys()
        .cloned()
        .collect()
}

fn names(c: &Cells) -> BTreeSet<String> {
    c.names().iter().map(|s| s.to_string()).collect()
}

/// One shape's pairing: the `cells` block for a freshly reached key and the
/// `shared_cells` block, each against its manifest. Both directions of the set
/// difference are named, because *declared and not emitted* (a `pending`) and
/// *emitted and not declared* (a manifest with a hole) are different findings.
fn pair<S: Shape<Input>>(cells: Value, shared: Value) {
    let (declared, emitted) = (names(&S::CELLS), keys_of(&cells));
    assert_eq!(
        declared, emitted,
        "`{}`: the cell manifest is not the row's cell set.\n  declared and never emitted: {:?}\n  emitted and never declared: {:?}",
        S::NAME,
        declared.difference(&emitted).collect::<Vec<_>>(),
        emitted.difference(&declared).collect::<Vec<_>>(),
    );
    let (declared, emitted) = (names(&S::SHARED), keys_of(&shared));
    assert_eq!(
        declared, emitted,
        "`{}`: the shape-level manifest is not the `shared_cells` set.\n  declared and never emitted: {:?}\n  emitted and never declared: {:?}",
        S::NAME,
        declared.difference(&emitted).collect::<Vec<_>>(),
        emitted.difference(&declared).collect::<Vec<_>>(),
    );
}

/// **EVERY SHAPE'S MANIFEST IS EXACTLY THE CELL SET ITS ROOT EMITS**, per-key
/// and shape-level, for all ten shapes this daemon registers.
///
/// The key is REACHED through each root's `Map::key`, which mints a default row
/// — so this is the cells block as the chassis would export it for a key that
/// has been named and not yet hydrated, which carries every cell name at
/// `not_hydrated` and is exactly the set a consumer's reads are routed against.
///
/// **THE MUTATION.** In `shapes/threads.rs`, add `("pending", Sc(Bool),
/// Required, [])` to `THREADS_CELLS`.
///
/// Forced red:
/// ```text
/// `threads`: the cell manifest is not the row's cell set.
///   declared and never emitted: ["pending"]
///   emitted and never declared: []
/// ```
#[test]
fn every_shape_emits_exactly_the_cells_it_declares() {
    let mut r = ThreadsRoot::default();
    r.rows.key("t".to_string());
    pair::<Threads>(r.cells("t").unwrap(), r.shared_cells());

    let mut r = AccountsRoot::default();
    r.rows.key("a".to_string());
    pair::<Accounts>(r.cells("a").unwrap(), r.shared_cells());

    let mut r = GaugeHistoryRoot::default();
    r.rows.key("a".to_string());
    pair::<GaugeHistory>(r.cells("a").unwrap(), r.shared_cells());

    let mut r = SummaryRoot::default();
    r.rows.key("t".to_string());
    pair::<Summary>(r.cells("t").unwrap(), r.shared_cells());

    let mut r = LogRoot::default();
    r.rows.key("t".to_string());
    pair::<Log>(r.cells("t").unwrap(), r.shared_cells());

    let mut r = RecentRoot::default();
    r.pools.key("fleet".to_string());
    pair::<Recent>(r.cells("fleet").unwrap(), r.shared_cells());

    let mut r = NarrationRoot::default();
    r.channels.key("info".to_string());
    pair::<Narration>(r.cells("info").unwrap(), r.shared_cells());

    let mut r = HeartbeatRoot::default();
    r.links.key("link".to_string());
    pair::<Heartbeat>(r.cells("link").unwrap(), r.shared_cells());

    let mut r = DaemonWarningsRoot::default();
    r.rows
        .key(threads::shapes::warnings::KEY_DAEMON.to_string());
    pair::<Warnings>(
        r.cells(threads::shapes::warnings::KEY_DAEMON).unwrap(),
        r.shared_cells(),
    );

    let mut r = BootRoot::default();
    r.rows.key(threads::shapes::boot::KEY_DAEMON.to_string());
    pair::<Boot>(
        r.cells(threads::shapes::boot::KEY_DAEMON).unwrap(),
        r.shared_cells(),
    );
}

/// **A ROW SLOT FOR A CELL THAT IS NOT KNOWN IS `null`, NEVER A NUMBER.**
///
/// `threads`' `queued`, `seen_seq` and `unread` read `.unwrap_or(0)` into the
/// row until 2026-10-08, so a key whose `cells` block said `not_hydrated` carried
/// a seen position of zero that nobody had stated — a producer substituting a
/// value for its own silence. The `cells` block is the truth; the row may not
/// contradict it. `accounts.registered` had the same `unwrap_or(false)`.
///
/// **THE MUTATION.** Restore `.copied().unwrap_or(0)` on `seen_seq` in
/// `ThreadCells::to_json`.
///
/// Forced red:
/// ```text
/// `threads.seen_seq` on a key nobody has hydrated must be null, not 0: 0
/// ```
#[test]
fn a_not_known_cell_emits_null_in_the_row_not_a_fabricated_value() {
    let mut r = ThreadsRoot::default();
    r.rows.key("t".to_string());
    let row = r.row("t").unwrap();
    for cell in ["queued", "seen_seq", "unread", "last_message_id", "newest", "context_id", "pages"] {
        assert!(
            row[cell].is_null(),
            "`threads.{cell}` on a key nobody has hydrated must be null, not {}",
            row[cell]
        );
        assert!(
            r.cells("t").unwrap()[cell]["state"] == "not_hydrated",
            "…and the cells block says so"
        );
    }
    let mut r = AccountsRoot::default();
    r.rows.key("a".to_string());
    assert!(
        r.row("a").unwrap()["registered"].is_null(),
        "`accounts.registered` on a key nobody has hydrated must be null, not false"
    );
}

/// **THE DECLARED STATE VOCABULARY IS THE DERIVATION'S.** `STATE_WORDS` is what
/// `threads.status.state` and `threads/non-idle`'s `threads[].state` publish; the words the
/// code actually produces are the five literals in `thread_states`' precedence
/// chain. A source scan, with a positive sentinel: it must find exactly five
/// words and `parked` must be one of them, so a scan that found nothing could
/// not pass.
///
/// **THE MUTATION.** Add `"pending"` to `STATE_WORDS`.
///
/// Forced red:
/// ```text
/// STATE_WORDS and the words `thread_states` derives differ: {"pending"}
/// ```
#[test]
fn the_state_vocabulary_is_the_derivation() {
    let src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/runtime.rs")).unwrap();
    let start = src
        .find("let (state, detail) = if executing {")
        .expect("the derivation's precedence chain");
    let end = src[start..].find("};").unwrap() + start;
    let chain = &src[start..end];
    let derived: BTreeSet<&str> = chain.split('"').skip(1).step_by(2).collect();
    assert_eq!(
        derived.len(),
        5,
        "the positive sentinel: five words in the chain: {derived:?}"
    );
    assert!(derived.contains("parked"), "{derived:?}");
    let declared: BTreeSet<&str> = threads::runtime::STATE_WORDS.iter().copied().collect();
    assert_eq!(
        declared,
        derived,
        "STATE_WORDS and the words `thread_states` derives differ: {:?}",
        declared
            .symmetric_difference(&derived)
            .collect::<BTreeSet<_>>()
    );
}

/// **THE DECLARED LEDGER VOCABULARY IS THE MATCH.** `KINDS` is what
/// `threads/log.events[].kind` and `threads.newest.kind` publish; the words the
/// code produces are the arms of `ThreadEvent::kind`. Scanned with a positive
/// sentinel: at least forty arms, `task_progress` among them.
///
/// **THE MUTATION.** Delete `"task_progress"` from `KINDS`.
///
/// Forced red:
/// ```text
/// KINDS and the arms of `ThreadEvent::kind` differ: {"task_progress"}
/// ```
#[test]
fn the_ledger_kind_vocabulary_is_the_match() {
    let src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/event.rs")).unwrap();
    let start = src
        .find("    pub fn kind(&self) -> &'static str {\n        match self {\n            ThreadEvent::")
        .expect("ThreadEvent::kind's match");
    let end = src[start..].find("\n        }\n    }").unwrap() + start;
    let arms: BTreeSet<&str> = src[start..end]
        .lines()
        .filter(|l| l.contains("=> \""))
        .map(|l| l.split('"').nth(1).unwrap())
        .collect();
    assert!(
        arms.len() >= 40,
        "the positive sentinel: {} arms",
        arms.len()
    );
    assert!(arms.contains("task_progress"));
    let declared: BTreeSet<&str> = threads::event::KINDS.iter().copied().collect();
    assert_eq!(
        declared,
        arms,
        "KINDS and the arms of `ThreadEvent::kind` differ: {:?}",
        declared
            .symmetric_difference(&arms)
            .collect::<BTreeSet<_>>()
    );
}

/// **THE `level` VOCABULARY ON THE WIRE IS EXACTLY WHAT THE COUNTING LAYER CAN
/// PRODUCE.** `warnings`' `kinds.level` is declared a closed `Enum` over
/// `warnings::LEVELS`, and the Python board paints red once anything was an
/// `error` — a comparison against a WORD. `seamdiff`'s `WordNotDeclared` can only
/// check such a comparison against a vocabulary the producer declares, so an
/// undeclared third level would make `== "error"` a predicate that is false for
/// ever and reads, from above, like an answer.
///
/// Two halves, because a declaration and an emitter can drift either way: the
/// manifest's declared words ARE `LEVELS`, and `LEVELS` IS the arm set of
/// `WarningCounter::on_event`. The second half is scanned by const IDENTIFIER
/// rather than by literal, so a level added to the match without being added to
/// `LEVELS` is the red. Positive sentinel: at least two arms, `LEVEL_WARN` among
/// them.
///
/// **THE MUTATION**, two parts, because one part alone would not COMPILE and a
/// red that is a compile error is not a forced red: add
/// `pub const LEVEL_INFO: &str = "info";` beside the others, and
/// `Level::INFO => LEVEL_INFO,` to `on_event`'s match — without adding
/// `LEVEL_INFO` to `LEVELS`. That is exactly the drift this guard exists for: a
/// third level counted and emitted, and a vocabulary that never heard about it.
///
/// Forced red:
/// ```text
/// `LEVELS` and the arms of `WarningCounter::on_event` differ: {"LEVEL_INFO"}
///   left: {"LEVEL_ERROR", "LEVEL_WARN"}
///  right: {"LEVEL_ERROR", "LEVEL_INFO", "LEVEL_WARN"}
/// ```
///
/// Half one is forced separately by declaring the cell
/// `Enum(&["warn"])` instead of `Enum(crate::warnings::LEVELS)`:
/// ```text
/// the declared `level` vocabulary is not `warnings::LEVELS`
///   left: ["warn"]
///  right: ["warn", "error"]
/// ```
#[test]
fn the_warning_level_vocabulary_is_the_layer() {
    // HALF ONE: the declared words are the const, and the words are pinned.
    let kinds = threads::shapes::warnings::WARNINGS_CELLS
        .publish()
        .into_iter()
        .find(|c| c.name == "kinds")
        .expect("`warnings` declares a `kinds` cell");
    let level = kinds
        .fields
        .iter()
        .find(|f| f.name == "level")
        .expect("`kinds` declares a `level` field");
    assert_eq!(
        level.words,
        threads::warnings::LEVELS,
        "the declared `level` vocabulary is not `warnings::LEVELS`"
    );
    assert_eq!(
        threads::warnings::LEVELS,
        ["warn", "error"],
        "the words themselves, pinned: a face compares against these"
    );

    // HALF TWO: the const is the layer's arm set.
    let src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/warnings.rs")).unwrap();
    let start = src
        .find("let level = match *md.level() {")
        .expect("`on_event`'s level match");
    let end = src[start..].find("\n        };").unwrap() + start;
    let arms: BTreeSet<&str> = src[start..end]
        .lines()
        .filter_map(|l| l.split("=> ").nth(1))
        .map(|r| r.trim().trim_end_matches(','))
        .filter(|r| r.starts_with("LEVEL_"))
        .collect();
    assert!(
        arms.len() >= 2 && arms.contains("LEVEL_WARN"),
        "the positive sentinel: {arms:?}"
    );
    let declared: BTreeSet<&str> = {
        let d = src
            .find("pub const LEVELS: &[&str] = &[")
            .expect("the `LEVELS` declaration");
        let e = src[d..].find("];").unwrap() + d;
        src[d..e]
            .split(['[', ',', ']'])
            .map(str::trim)
            .filter(|t| t.starts_with("LEVEL_"))
            .collect()
    };
    assert_eq!(
        declared,
        arms,
        "`LEVELS` and the arms of `WarningCounter::on_event` differ: {:?}",
        declared.symmetric_difference(&arms).collect::<BTreeSet<_>>()
    );
}

/// **THE `reason` VOCABULARY ON THE WIRE IS EXACTLY WHAT THE DERIVATION CAN
/// PRODUCE.** `boot`'s `reason` cell is declared a closed `Enum` over
/// `update::CAUSES`, and a face paints a `fallback` differently from a
/// `new_binary` — a comparison against a WORD. `seamdiff`'s `WordNotDeclared` can
/// only check such a comparison against a vocabulary the producer declares, so an
/// undeclared fifth reason would make `== "fallback"` a predicate that is false
/// for ever and reads, from above, like an answer.
///
/// `warnings`' `the_warning_level_vocabulary_is_the_layer` is this test's model
/// and the argument is not restated; what differs is the emitter, which here is a
/// three-test derivation rather than a logging layer.
///
/// Two halves, because a declaration and an emitter drift either way: the
/// manifest's declared words ARE `CAUSES`, and `CAUSES` IS the set of reason
/// identifiers `UpdateGate::compute_boot_note` can return. The second half is
/// scanned by const IDENTIFIER rather than by literal, so a reason added to the
/// derivation without being added to `CAUSES` is the red. Positive sentinel: at
/// least three identifiers, `CAUSE_FALLBACK` among them — which also catches a
/// mis-scan, because a slice that missed the body would find none.
///
/// **THE MUTATION**, two parts, because one part alone would not COMPILE and a
/// red that is a compile error is not a forced red: add
/// `pub const CAUSE_CRASHED: &str = "crashed";` beside the others, and change
/// `compute_boot_note`'s final arm to `(CAUSE_CRASHED, "came back")` — without
/// adding `CAUSE_CRASHED` to `CAUSES`.
///
/// Forced red:
/// ```text
/// `CAUSES` and the reasons `compute_boot_note` can return differ: {"CAUSE_CRASHED", "CAUSE_UNKNOWN"}
/// ```
///
/// Half one is forced separately by declaring the cell
/// `Enum(&["unknown"])` instead of `Enum(crate::update::CAUSES)`:
/// ```text
/// the declared `reason` vocabulary is not `update::CAUSES`
/// ```
#[test]
fn the_boot_reason_vocabulary_is_the_derivation() {
    // HALF ONE: the declared words are the const, and the words are pinned.
    let reason = threads::shapes::boot::BOOT_CELLS
        .publish()
        .into_iter()
        .find(|c| c.name == "reason")
        .expect("`boot` declares a `reason` cell");
    assert_eq!(
        reason.words,
        threads::update::CAUSES,
        "the declared `reason` vocabulary is not `update::CAUSES`"
    );
    assert_eq!(
        threads::update::CAUSES,
        ["new_binary", "operator", "fallback", "unknown"],
        "the words themselves, pinned: a face compares against these"
    );

    // HALF TWO: the const is the derivation's reason set.
    let src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/update.rs")).unwrap();
    let start = src
        .find("fn compute_boot_note(")
        .expect("`compute_boot_note` was renamed — re-point this test");
    let end = src[start..]
        .find("\n    }\n")
        .expect("the end of `compute_boot_note` moved — re-point this test")
        + start;
    let body = &src[start..end];
    let returned: BTreeSet<&str> = body
        .match_indices("CAUSE_")
        .map(|(i, _)| {
            let t = &body[i..];
            let n = t
                .find(|c: char| !(c.is_ascii_uppercase() || c == '_'))
                .unwrap_or(t.len());
            &t[..n]
        })
        .collect();
    assert!(
        returned.len() >= 3 && returned.contains("CAUSE_FALLBACK"),
        "the positive sentinel: {returned:?}"
    );
    let declared: BTreeSet<&str> = {
        let d = src
            .find("pub const CAUSES: &[&str] = &[")
            .expect("the `CAUSES` declaration");
        let e = src[d..].find("];").unwrap() + d;
        src[d..e]
            .split(['[', ',', ']'])
            .map(str::trim)
            .filter(|t| t.starts_with("CAUSE_"))
            .collect()
    };
    assert_eq!(
        declared,
        returned,
        "`CAUSES` and the reasons `compute_boot_note` can return differ: {:?}",
        declared
            .symmetric_difference(&returned)
            .collect::<BTreeSet<_>>()
    );
}

/// **THE OFFLINE MANIFEST TOOL LISTS EVERY SHAPE THE DAEMON REGISTERS, IN
/// REGISTRATION ORDER.** The tool is what `make-inventory.py --declared` reads,
/// so a shape registered in `start_shapes` and not listed there would be an
/// inventory with a hole in it. Pinned against the registration calls in
/// `runtime.rs`, scanned, with the usual sentinel.
///
/// **THE MUTATION.** Delete the `Heartbeat` line from `shapes-manifest.rs`.
///
/// Forced red:
/// ```text
/// shapes-manifest lists [.., "narration"] and the daemon registers [.., "narration", "heartbeat"]
/// ```
#[test]
fn the_manifest_tool_lists_every_registered_shape() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_shapes-manifest"))
        .output()
        .expect("the manifest tool runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    let listed: Vec<String> = doc["shapes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();

    let src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/runtime.rs")).unwrap();
    let registered: Vec<String> = src
        .lines()
        .filter(|l| l.contains(".register(crate::shapes::"))
        .map(|l| {
            let module = l
                .split("crate::shapes::")
                .nth(1)
                .unwrap()
                .split("::")
                .next()
                .unwrap();
            match module {
                "accounts" => "accounts",
                "gauge_history" => "accounts/gauge-history",
                "threads" => "threads",
                "summary" => "threads/summary",
                "log" => "threads/log",
                "recent" => "threads/recent-updates",
                "recent_visits" => "threads/recent-visits",
                "nonidle" => "threads/non-idle",
                "narration" => "narration",
                "heartbeat" => "heartbeat",
                "warnings" => "warnings",
                "boot" => "boot",
                other => panic!("a shape module this test does not know: {other}"),
            }
            .to_string()
        })
        .collect();
    assert!(
        registered.len() >= 12,
        "the positive sentinel: {registered:?}"
    );
    assert_eq!(
        listed, registered,
        "shapes-manifest lists {listed:?} and the daemon registers {registered:?}"
    );
    // And every listed shape carries its manifest, in full.
    for s in doc["shapes"].as_array().unwrap() {
        let info: shapewire::wire::ShapeInfo = serde_json::from_value(s.clone()).unwrap();
        assert!(
            !info.cells.is_empty(),
            "`{}` declares no per-key cell",
            info.name
        );
    }
}

/// **THE COMMITTED SEAM INVENTORY CARRIES EXACTLY THE CELLS THE PRODUCER
/// DECLARES, SHAPE BY SHAPE** (R4.S13, ledger 408 / R4.S12-3).
///
/// `libs/shapewire/seam/inventory.json` is what every face checks itself
/// against and what `inventory.rs` — a consumer's BUILD INPUT — is generated
/// from. It is taken by `make-inventory.py --declared`, which reads this very
/// binary. Nothing compared the two before this: `seam_reference.rs` checks
/// `inventory.rs` against `inventory.json`, and the tests above check the
/// manifest against the rows, so a branch that added a cell to a shape and
/// never re-took the inventory was green everywhere while every face compiled
/// against a surface missing that cell. Red here means: re-run
/// `libs/shapewire/seam/tools/make-inventory.py --declared` and commit it.
///
/// Both directions of the set difference are named per shape, for `cells` and
/// `shared_cells`, and then the declarations themselves (dtype, required,
/// fields, words) are compared, because a cell whose dtype moved is as much a
/// re-take as one that was added.
///
/// **THE MUTATIONS** (both forced red, R4.S13 report): add a cell to
/// `Heartbeat::CELLS` and its row without re-taking the inventory; and, the
/// other direction, delete a cell from `inventory.json` by hand.
#[test]
fn the_seam_inventory_carries_every_cell_the_manifest_declares() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_shapes-manifest"))
        .output()
        .expect("the manifest tool runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let declared: Value = serde_json::from_slice(&out.stdout).unwrap();

    let inv_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../libs/shapewire/seam/inventory.json"
    );
    let inv: Value = serde_json::from_str(
        &std::fs::read_to_string(inv_path).unwrap_or_else(|e| panic!("{inv_path}: {e}")),
    )
    .unwrap_or_else(|e| panic!("{inv_path} is JSON: {e}"));

    // shape name -> (cells, shared_cells), each as name -> declaration.
    type Side = std::collections::BTreeMap<String, (Vec<Value>, Vec<Value>)>;
    fn side(doc: &Value, what: &str) -> Side {
        doc["shapes"]
            .as_array()
            .unwrap_or_else(|| panic!("{what} has a `shapes` array"))
            .iter()
            .map(|s| {
                let cells = |k: &str| -> Vec<Value> {
                    s[k].as_array()
                        .unwrap_or_else(|| panic!("{what}: `{}` has no `{k}` array", s["name"]))
                        .clone()
                };
                (
                    s["name"].as_str().unwrap().to_string(),
                    (cells("cells"), cells("shared_cells")),
                )
            })
            .collect()
    }
    let (decl, taken) = (side(&declared, "shapes-manifest"), side(&inv, "inventory.json"));

    // NON-VACUOUS FIRST: both sides carry the whole registered surface, with cells.
    for (what, s) in [("shapes-manifest", &decl), ("inventory.json", &taken)] {
        assert!(s.len() >= 12, "{what} carries only {} shape(s): {:?}", s.len(), s.keys());
        assert!(
            s.values().all(|(c, _)| !c.is_empty()),
            "{what}: a shape with no per-key cell would compare equal to anything empty"
        );
    }

    let names = |v: &[Value]| -> BTreeSet<String> {
        v.iter().map(|c| c["name"].as_str().unwrap().to_string()).collect()
    };
    let mut findings = Vec::new();
    let (dk, tk): (BTreeSet<_>, BTreeSet<_>) = (decl.keys().collect(), taken.keys().collect());
    if dk != tk {
        findings.push(format!(
            "shape sets differ — declared, not in inventory: {:?}; in inventory, not declared: {:?}",
            dk.difference(&tk).collect::<Vec<_>>(),
            tk.difference(&dk).collect::<Vec<_>>()
        ));
    }
    for (shape, (dc, ds)) in &decl {
        let Some((tc, ts)) = taken.get(shape) else { continue };
        for (which, d, t) in [("cells", dc, tc), ("shared_cells", ds, ts)] {
            let (dn, tn) = (names(d), names(t));
            if dn != tn {
                findings.push(format!(
                    "`{shape}` {which}: declared, not in inventory: {:?}; in inventory, not declared: {:?}",
                    dn.difference(&tn).collect::<Vec<_>>(),
                    tn.difference(&dn).collect::<Vec<_>>()
                ));
                continue;
            }
            for c in d {
                let n = &c["name"];
                let tc = t.iter().find(|x| &x["name"] == n).unwrap();
                if c != tc {
                    findings.push(format!(
                        "`{shape}` {which} `{}`: declared {c} but the inventory carries {tc}",
                        n.as_str().unwrap()
                    ));
                }
            }
        }
    }
    assert!(
        findings.is_empty(),
        "libs/shapewire/seam/inventory.json is not the surface the producer declares \
         ({} finding(s)). Re-take it: libs/shapewire/seam/tools/make-inventory.py --declared, \
         and commit the result.\n  {}",
        findings.len(),
        findings.join("\n  ")
    );
}
