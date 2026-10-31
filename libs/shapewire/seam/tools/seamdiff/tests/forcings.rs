//! **THE FORCINGS.** Every guard in `engine.rs` driven red once, by its own
//! finding, with the text asserted — because `core:writing-tools`' rule is that
//! a green nobody has forced to fail is not evidence, and the green this tool
//! produces is the entire claim of the seam kit.
//!
//! ## The forcings are OPPOSED, and that is the structural half
//!
//! Two of these tests cannot both be satisfied by a broken tool, which is the
//! point of writing them as a pair:
//!
//! * [`a_diff_that_permits_everything_is_caught`] — a tool that treated a fence
//!   as "ignore this whole file", or that reported nothing it could not align,
//!   passes a copy with a one-line drift. This test forces that drift and
//!   requires the finding.
//! * [`a_diff_that_permits_nothing_is_caught`] — a tool that simply compared
//!   the two texts byte-for-byte would reject the legitimate per-face fence
//!   body, which is the one difference that MUST be allowed. This test puts a
//!   different body in the same fence and requires silence.
//!
//! So a tool cannot pass this file by being more permissive, and cannot pass it
//! by being stricter. The reference diff has to be exactly as strict as §5's
//! table.
//!
//! ## The self-witnessing guard
//!
//! [`the_stray_fence_exclusion_is_not_vacuous`] is the one that matters most
//! and is the easiest to get wrong: a source-scanning guard whose exclusion
//! list protects nothing is a comment pretending to be a mechanism. It asserts
//! that every excluded path EXISTS and CONTAINS the marker, so the exclusion is
//! load-bearing by measurement.

use seamdiff::*;

// ─────────────────────────────────────────────────────────────────────────────
//  A minimal reference, so a forcing is one edit away from a green
// ─────────────────────────────────────────────────────────────────────────────

const REFERENCE: &str = "\
pub const A: u8 = 1;
// SEAM-DIVERGE(D2): the face's own two words.
pub const FACE: &str = \"reference\";
// SEAM-END
pub fn shared() -> u8 {
    A
}
";

/// A copy that differs only inside the fence — the legal case.
const GOOD_COPY: &str = "\
pub const A: u8 = 1;
// SEAM-DIVERGE(D2): the face's own two words.
pub const FACE: &str = \"tui\";
// SEAM-END
pub fn shared() -> u8 {
    A
}
";

fn findings(reference: &str, copy: &str) -> Vec<Finding> {
    compare_file("t.rs", reference, copy).1
}

fn text(fs: &[Finding]) -> String {
    fs.iter()
        .map(|f| f.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

// ─────────────────────────────────────────────────────────────────────────────
//  The green, and the two opposed forcings
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_green_is_a_real_green() {
    assert_eq!(findings(REFERENCE, GOOD_COPY), Vec::new());
}

/// **A ONE-LINE DRIFT OUTSIDE A FENCE IS CAUGHT.** This is the headline.
#[test]
fn a_diff_that_permits_everything_is_caught() {
    let drifted = GOOD_COPY.replace("    A\n", "    A + 1\n");
    assert_ne!(drifted, GOOD_COPY, "the mutation did not apply");
    let fs = findings(REFERENCE, &drifted);
    assert!(
        matches!(fs.as_slice(), [Finding::OutsideDiffers { .. }]),
        "expected exactly one OutsideDiffers, got: {}",
        text(&fs)
    );
    let t = text(&fs);
    assert!(t.contains("the text outside the fences differs"), "{t}");
    assert!(t.contains("reference:6:     A"), "{t}");
    assert!(t.contains("copy:6:     A + 1"), "{t}");
}

/// **AND A DRIFT INSIDE A FENCE IS NOT.** A tool that reddened here would
/// forbid the one difference §5's table exists to permit, and the faces could
/// not both carry their own identity.
#[test]
fn a_diff_that_permits_nothing_is_caught() {
    // Three different bodies, all inside the same fence, all silent.
    for body in [
        "pub const FACE: &str = \"aui\";",
        "pub const FACE: &str = \"tui\";\nconst ENV_PREFIX: &str = \"TUI_THREADS\";",
        "// a face may even put nothing here but a comment",
    ] {
        let copy = format!(
            "pub const A: u8 = 1;\n// SEAM-DIVERGE(D2): the face's own two words.\n{body}\n// SEAM-END\npub fn shared() -> u8 {{\n    A\n}}\n"
        );
        assert_eq!(
            findings(REFERENCE, &copy),
            Vec::new(),
            "a legitimate fenced body was rejected: {body}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  Move the reference and the copy goes red — the direction of pressure
// ─────────────────────────────────────────────────────────────────────────────

/// `docs/consumer-seam.md` §7.1: *"the reference moves first, and an app that
/// drifts on its own is caught at its next `cargo test`"*. Both halves.
#[test]
fn moving_the_reference_reddens_a_copy_that_has_not_followed() {
    let moved = REFERENCE.replace("pub const A: u8 = 1;", "pub const A: u8 = 2;");
    assert_ne!(moved, REFERENCE, "the mutation did not apply");
    let fs = findings(&moved, GOOD_COPY);
    assert!(
        matches!(fs.as_slice(), [Finding::OutsideDiffers { .. }]),
        "a copy that did not follow the reference stayed green: {}",
        text(&fs)
    );
    // And it is green again once the copy follows.
    let followed = GOOD_COPY.replace("pub const A: u8 = 1;", "pub const A: u8 = 2;");
    assert_eq!(findings(&moved, &followed), Vec::new());
}

// ─────────────────────────────────────────────────────────────────────────────
//  The fence sequence — both halves
// ─────────────────────────────────────────────────────────────────────────────

/// A fence the reference declares and the copy DROPPED. The copy then silently
/// carries the reference's answer where it was supposed to give its own —
/// `FACE = "reference"` on a real face's subscribe, which rule 2.1 makes a lie
/// in the producer's ledger.
#[test]
fn a_dropped_fence_is_caught() {
    let copy = "pub const A: u8 = 1;\npub const FACE: &str = \"reference\";\npub fn shared() -> u8 {\n    A\n}\n";
    let fs = findings(REFERENCE, copy);
    let t = text(&fs);
    assert!(
        fs.iter()
            .any(|f| matches!(f, Finding::FenceSequence { .. })),
        "{t}"
    );
    assert!(t.contains("the fence ids differ from the reference's"), "{t}");
    assert!(t.contains("reference: [D2]"), "{t}");
    assert!(t.contains("copy:      []"), "{t}");
}

/// A fence the COPY invented, at an id that is on the allowed list — so this is
/// caught by the sequence rule and not by the id rule, which is why the two are
/// separate findings.
#[test]
fn an_invented_fence_is_caught_even_at_a_sanctioned_id() {
    let copy = GOOD_COPY.replace(
        "pub fn shared() -> u8 {",
        "// SEAM-DIVERGE(D4): a verb set I decided to fence after all.\npub fn extra() {}\n// SEAM-END\npub fn shared() -> u8 {",
    );
    let fs = findings(REFERENCE, &copy);
    let t = text(&fs);
    assert!(
        fs.iter()
            .any(|f| matches!(f, Finding::FenceSequence { .. })),
        "{t}"
    );
    assert!(t.contains("copy:      [D2 D4]"), "{t}");
}

#[test]
fn an_unsanctioned_id_is_refused() {
    let copy = GOOD_COPY.replace("SEAM-DIVERGE(D2)", "SEAM-DIVERGE(D99)");
    let fs = findings(REFERENCE, &copy);
    let t = text(&fs);
    assert!(
        fs.iter()
            .any(|f| matches!(f, Finding::IdNotAllowed { .. })),
        "{t}"
    );
    assert!(
        t.contains("D99 is not a sanctioned divergence id (allowed: D1 D2 D3 D4 D5 D6)"),
        "{t}"
    );
}

#[test]
fn a_fence_with_no_reason_is_refused() {
    let copy = GOOD_COPY.replace(
        "// SEAM-DIVERGE(D2): the face's own two words.",
        "// SEAM-DIVERGE(D2):",
    );
    let fs = findings(REFERENCE, &copy);
    let t = text(&fs);
    assert!(fs.iter().any(|f| matches!(f, Finding::NoReason { .. })), "{t}");
    assert!(
        t.contains("states no reason; §5's table is the list of reasons"),
        "{t}"
    );
}

#[test]
fn an_unclosed_fence_is_refused() {
    let copy = GOOD_COPY.replace("// SEAM-END\n", "");
    let fs = findings(REFERENCE, &copy);
    let t = text(&fs);
    assert!(fs.iter().any(|f| matches!(f, Finding::Unclosed { .. })), "{t}");
    assert!(t.contains("is never closed by a SEAM-END"), "{t}");
}

#[test]
fn an_unopened_end_is_refused() {
    let copy = format!("{GOOD_COPY}// SEAM-END\n");
    let fs = findings(REFERENCE, &copy);
    let t = text(&fs);
    assert!(fs.iter().any(|f| matches!(f, Finding::Unopened { .. })), "{t}");
    assert!(t.contains("SEAM-END with no fence open"), "{t}");
}

#[test]
fn a_nested_fence_is_refused() {
    let copy = GOOD_COPY.replace(
        "pub const FACE: &str = \"tui\";",
        "// SEAM-DIVERGE(D3): inside another fence.\npub const FACE: &str = \"tui\";",
    );
    let fs = findings(REFERENCE, &copy);
    let t = text(&fs);
    assert!(fs.iter().any(|f| matches!(f, Finding::Nested { .. })), "{t}");
    assert!(t.contains("fences do not nest"), "{t}");
}

// ─────────────────────────────────────────────────────────────────────────────
//  The budget
// ─────────────────────────────────────────────────────────────────────────────

/// **THE BUDGET BITES.** Build a copy whose fence is one code line over, and
/// require the finding with its arithmetic.
#[test]
fn the_budget_bites_at_exactly_the_stated_number() {
    let mut body = String::new();
    for i in 0..BUDGET {
        body.push_str(&format!("pub const L{i}: u8 = {};\n", i % 7));
    }
    let copy = format!(
        "pub const A: u8 = 1;\n// SEAM-DIVERGE(D2): a face that spent the lot.\n{body}// SEAM-END\npub fn shared() -> u8 {{\n    A\n}}\n"
    );
    let (parsed, fs) = compare_file("t.rs", REFERENCE, &copy);
    assert_eq!(fs, Vec::new(), "the copy should be clean apart from size");
    let b = budget(&[("t.rs".to_string(), parsed)]).expect("over budget");
    let t = b.to_string();
    assert!(
        t.contains(&format!(
            "the divergence budget is {BUDGET} code lines and this copy spends {BUDGET}"
        )),
        "{t}"
    );
    assert!(t.contains("t.rs  D2  60"), "{t}");
}

/// One line fewer is green — so the number in the message is the number that
/// bites, and the test above is not passing because of an off-by-something.
#[test]
fn one_line_under_the_budget_is_green() {
    let mut body = String::new();
    for i in 0..(BUDGET - 1) {
        body.push_str(&format!("pub const L{i}: u8 = {};\n", i % 7));
    }
    let copy = format!(
        "pub const A: u8 = 1;\n// SEAM-DIVERGE(D2): a face that spent nearly the lot.\n{body}// SEAM-END\npub fn shared() -> u8 {{\n    A\n}}\n"
    );
    let (parsed, fs) = compare_file("t.rs", REFERENCE, &copy);
    assert_eq!(fs, Vec::new());
    assert_eq!(budget(&[("t.rs".to_string(), parsed)]), None);
}

/// **COMMENTS AND BLANKS ARE FREE, AND DELIBERATELY.** A divergence must be
/// able to explain itself at length; a budget that charged for the explanation
/// would push the explanation out.
#[test]
fn a_fence_that_is_all_explanation_costs_nothing() {
    let mut body = String::new();
    for i in 0..500 {
        body.push_str(&format!("// line {i} of why this face is different\n"));
    }
    body.push('\n');
    let copy = format!(
        "pub const A: u8 = 1;\n// SEAM-DIVERGE(D2): a very well argued divergence.\n{body}// SEAM-END\npub fn shared() -> u8 {{\n    A\n}}\n"
    );
    let (parsed, fs) = compare_file("t.rs", REFERENCE, &copy);
    assert_eq!(fs, Vec::new());
    assert_eq!(budget(&[("t.rs".to_string(), parsed)]), None);
}

/// The budget is over the WHOLE copy, so a face cannot buy room by spreading
/// one divergence over three files.
#[test]
fn the_budget_is_not_per_file() {
    let third = BUDGET / 3 + 1;
    let mut parsed = Vec::new();
    for (n, file) in ["a.rs", "b.rs", "c.rs"].iter().enumerate() {
        let mut body = String::new();
        for i in 0..third {
            body.push_str(&format!("pub const L{n}_{i}: u8 = 0;\n"));
        }
        let copy = format!("// SEAM-DIVERGE(D2): a slice of one divergence.\n{body}// SEAM-END\n");
        let (p, fs) = parse(file, &copy);
        assert_eq!(fs, Vec::new());
        parsed.push((file.to_string(), p));
    }
    let b = budget(&parsed).expect("three files that each fit must not fit together");
    assert!(b.to_string().contains("a.rs  D2"), "{b}");
    assert!(b.to_string().contains("c.rs  D2"), "{b}");
}

// ─────────────────────────────────────────────────────────────────────────────
//  The real reference, and the self-witnessing guard
// ─────────────────────────────────────────────────────────────────────────────

fn seam_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the seam directory")
}

/// The reference's own fences are well-formed, sanctioned, and inside budget —
/// which is a precondition for every copy and not a given.
#[test]
fn the_reference_itself_is_well_formed_and_inside_budget() {
    let dir = seam_dir();
    let mut parsed = Vec::new();
    for file in FILES {
        let body = std::fs::read_to_string(dir.join(file))
            .unwrap_or_else(|e| panic!("{file}: {e}"));
        let (p, fs) = parse(file, &body);
        assert_eq!(fs, Vec::new(), "{file}: {}", text(&fs));
        parsed.push((file.to_string(), p));
    }
    assert_eq!(budget(&parsed), None);
    // And it is the two fences the table declares, in the files it names.
    let ids: Vec<(String, Vec<String>)> = parsed
        .iter()
        .map(|(f, p)| (f.clone(), p.ids()))
        .filter(|(_, ids)| !ids.is_empty())
        .collect();
    assert_eq!(
        ids,
        vec![
            ("link.rs".to_string(), vec!["D2".to_string()]),
            ("held.rs".to_string(), vec!["D3".to_string()]),
        ],
        "the reference's live fences are not the two `mod.rs`'s table declares"
    );
}

/// **THE EXCLUSION IS NOT VACUOUS**, asserted rather than claimed, by the same
/// STRICT detector the guard itself uses.
///
/// Fifteen source-scanning guards in this arc were satisfied by their own text,
/// including a ban satisfied by the comment explaining it. Two of this file's
/// own tests found that class in this engine on its first run: the reference's
/// `mod.rs` documents the marker in prose and the loose detector matched it,
/// and `SELF_EXCLUDED` carried `src/main.rs`, which contains no marker at all.
/// Both are closed — the detector is strict, and this test is what forbids an
/// exclusion that protects nothing.
///
/// Three assertions, and the third is the one that makes it a measurement:
///
/// 1. every excluded path EXISTS;
/// 2. the strict detector finds a marker in it, so excluding it does work;
/// 3. a walk with NO exclusions finds exactly the excluded set over the tools
///    tree — which is the counter-factual: without the list the guard would
///    fire, and with it the guard is clean.
#[test]
fn the_stray_fence_exclusion_is_not_vacuous() {
    let dir = seam_dir();
    for rel in SELF_EXCLUDED {
        let p = dir.join(rel);
        let body = std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("{rel} is excluded and does not exist: {e}"));
        assert!(
            strict_markers(&body) > 0,
            "{rel} is excluded from the stray-fence walk but the STRICT detector finds \
             no marker in it, so the exclusion protects nothing"
        );
    }

    // (3) the counter-factual: the unexcluded walk over the tools tree finds
    // exactly the files on the list, and finds at least one.
    let mut found = walk_everything(&dir.join("tools"));
    found.sort();
    let mut expected: Vec<String> = SELF_EXCLUDED
        .iter()
        .map(|r| dir.join(r).display().to_string())
        .collect();
    expected.sort();
    assert_eq!(
        found, expected,
        "the unexcluded walk and the exclusion list have parted company: either a file \
         grew a marker and is not excluded, or a path is excluded that needs no excluding"
    );
    assert!(
        !found.is_empty(),
        "the tools tree contains no marker at all, so this test proves nothing"
    );

    // And the real walk, with the exclusion in place: clean.
    assert_eq!(stray_fences(&dir), Vec::new());
}

/// The engine's own strictness, restated here so the non-vacuity proof measures
/// the same thing the guard does rather than something looser.
fn strict_markers(body: &str) -> usize {
    body.lines()
        .filter(|l| l.trim_start().starts_with("// SEAM-DIVERGE("))
        .count()
}

/// A deliberately exclusion-free walk, for the counter-factual above only.
fn walk_everything(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            out.extend(walk_everything(&p));
        } else if std::fs::read_to_string(&p)
            .map(|t| strict_markers(&t) > 0)
            .unwrap_or(false)
        {
            out.push(p.display().to_string());
        }
    }
    out
}

/// **AND THE STRICTNESS ITSELF IS FORCED.** A marker in prose, in a doc
/// comment, or with anything after the `SEAM-END` is not a marker — which is
/// what lets the reference's own documentation name the mechanism.
#[test]
fn a_marker_in_prose_is_not_a_marker() {
    let prose = "\
pub const A: u8 = 1;
//! Each LIVE row is a `// SEAM-DIVERGE(D<n>)` … `// SEAM-END` fence in the
/// source, and this doc comment must not open one.
// a bare mention of SEAM-DIVERGE( in running text, likewise
pub fn shared() -> u8 {
    A
}
";
    let (p, fs) = parse("t.rs", prose);
    assert_eq!(fs, Vec::new(), "{}", text(&fs));
    assert_eq!(p.fences.len(), 0, "prose opened a fence: {:?}", p.fences);
    assert_eq!(p.outside.len(), 7);
}

/// A marker in a seventh file buys silence rather than permission, so it is
/// refused. Forced in a temp tree, because the real one is clean.
#[test]
fn a_stray_fence_in_a_seventh_file_is_caught() {
    let dir = std::env::temp_dir().join(format!("seamdiff-stray-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mod.rs"), REFERENCE).unwrap();
    std::fs::write(
        dir.join("helpers.rs"),
        "// SEAM-DIVERGE(D2): snuck in here where nothing diffs.\nfn x() {}\n// SEAM-END\n",
    )
    .unwrap();
    let fs = stray_fences(&dir);
    let t = text(&fs);
    assert!(fs.iter().any(|f| matches!(f, Finding::StrayFence { .. })), "{t}");
    assert!(t.contains("helpers.rs:1"), "{t}");
    assert!(t.contains("outside the seven seam files"), "{t}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ─────────────────────────────────────────────────────────────────────────────
//  The inventory guard
// ─────────────────────────────────────────────────────────────────────────────

const INV: &str = r#"{
  "source_commit": "6445362abcdef0000000000000000000000000aa",
  "methods": ["threads/create", "threads/{id}/send", "shapes/{shape}/subscribe"],
  "shapes": [
    {"name": "threads", "version": 1, "serves": [1]},
    {"name": "threads/log", "version": 1, "serves": [1]}
  ]
}"#;

#[test]
fn the_inventory_guard_is_green_on_a_routed_surface() {
    let inv = parse_inventory(INV).unwrap();
    assert_eq!(
        check_inventory(
            &inv,
            &["threads/create".into(), "threads/{id}/send".into()],
            &["threads".into(), "threads/log".into()],
            &Contracts::uniform(1),
            "shapewire@6445362"
        ),
        Vec::new()
    );
}

/// **A METHOD THE PRODUCER DOES NOT ROUTE, CAUGHT — AT TWO, NOT AT ONE.**
///
/// This guard's first fixture was the AUI's two dot-form survivors,
/// `thread.replace` and `thread.unpend`: `docs/consumer-seam.md` §3.2 and M0
/// both predicted them, nothing was checking, and THIS TEST IS WHAT FOUND
/// THEM. They are now DELETED from every face, root and branch, along with
/// the `pending` field they guarded, so they are no longer available as
/// fixtures and quoting them here would re-seed the names the deletion
/// removed.
///
/// The substitutes are DELIBERATELY UNROUTABLE BY CONSTRUCTION rather than
/// merely absent today. Every real name on this wire is a SLASH PATH under a
/// resource family (`threads/…`, `accounts/…`, `shapes/…`, `daemon/…`); the
/// dot form is the retired spelling and `seamdiff.fixture` is not a resource
/// any producer could grow. So neither of these can quietly become real and
/// turn this test green for the wrong reason — which is the failure mode the
/// `thread.*` fixtures actually had, since they named a live defect and would
/// have gone green the day somebody implemented it.
///
/// **TWO names, not one, and that is the arbitration.** One unrouted method
/// cannot distinguish "the check fires" from "the check fires once and stops",
/// which is the shape every N>=2 rule in this kit exists for; `fs.len() == 2`
/// is the assertion that would catch a short-circuit.
#[test]
fn a_method_the_producer_does_not_route_is_caught() {
    let inv = parse_inventory(INV).unwrap();
    let fs = check_inventory(
        &inv,
        &[
            "seamdiff.fixture.unrouted-alpha".into(),
            "seamdiff.fixture.unrouted-beta".into(),
        ],
        &[],
        &Contracts::uniform(1),
        "shapewire@6445362",
    );
    let t = text(&fs);
    assert_eq!(fs.len(), 2, "{t}");
    assert!(t.contains("`seamdiff.fixture.unrouted-alpha` does not route"), "{t}");
    assert!(t.contains("`seamdiff.fixture.unrouted-beta` does not route"), "{t}");
    assert!(t.contains("The daemon answers `unknown_method`"), "{t}");
}

#[test]
fn a_shape_the_producer_does_not_serve_is_caught() {
    let inv = parse_inventory(INV).unwrap();
    let fs = check_inventory(&inv, &[], &["threads/projects".into()], &Contracts::uniform(1), "shapewire@6445362");
    let t = text(&fs);
    assert!(t.contains("the book names shape `threads/projects`, which the producer does not serve"), "{t}");
}

#[test]
fn a_contract_version_the_shape_does_not_serve_is_caught() {
    let inv = parse_inventory(INV).unwrap();
    let fs = check_inventory(&inv, &[], &["threads".into()], &Contracts::uniform(2), "shapewire@6445362");
    let t = text(&fs);
    assert!(t.contains("serves contract versions [1] and the seam declares 2"), "{t}");
    assert!(t.contains("rule 2.6 refuses that link"), "{t}");
}

/// **RULE 2.6 IS PER SHAPE, AND SO IS THIS CHECK.** Two shapes at two
/// different versions — `accounts` serving only 2 (its `threads` cell is gone),
/// `threads` serving only 1 — can only both be green under a table that states
/// each shape's own version. Opposed both ways, so no single integer passes:
/// everything-at-1 is refused on `accounts`, everything-at-2 on `threads`.
#[test]
fn each_shape_is_checked_at_its_own_contract_version() {
    let inv = parse_inventory(
        r#"{
  "source_commit": "6445362aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "methods": ["threads/create"],
  "shapes": [
    {"name": "threads", "version": 1, "serves": [1]},
    {"name": "accounts", "version": 2, "serves": [2]}
  ]
}"#,
    )
    .unwrap();
    let shapes: Vec<String> = vec!["threads".into(), "accounts".into()];
    let table = Contracts {
        default: 1,
        per_shape: vec![("threads".into(), 1), ("accounts".into(), 2)],
    };
    assert_eq!(check_inventory(&inv, &[], &shapes, &table, "shapewire@6445362"), Vec::new());

    let all_one = text(&check_inventory(&inv, &[], &shapes, &Contracts::uniform(1), "shapewire@6445362"));
    assert!(all_one.contains("`accounts` serves contract versions [2] and the seam declares 1"), "{all_one}");
    assert!(!all_one.contains("`threads`"), "{all_one}");

    let all_two = text(&check_inventory(&inv, &[], &shapes, &Contracts::uniform(2), "shapewire@6445362"));
    assert!(all_two.contains("`threads` serves contract versions [1] and the seam declares 2"), "{all_two}");
    assert!(!all_two.contains("`accounts`"), "{all_two}");
}

/// **AN UNREADABLE CONTRACT TABLE IS RED, ONE FINDING PER SHAPE.**
/// `contracts_of` answers version 0 for every shape when the table is missing
/// or does not parse, and no producer serves 0. Positive sentinel: the same
/// fixture WITH a readable table produces no finding, so the reds below are the
/// table's absence and not something else about the fixture.
///
/// **THE MUTATION.** `contracts_of` defaulting to `Contracts::uniform(1)`: the
/// fixture inventory serves 1, the missing table reads as 1, and the
/// "declares 0" assertions fire.
#[test]
fn an_unreadable_contract_table_is_a_finding_per_shape() {
    let inv = parse_inventory(INV).unwrap();
    let shapes: Vec<String> = vec!["threads".into(), "threads/log".into()];
    let readable = "pub const SHAPE_CONTRACT_VERSIONS: &[(&str, u32)] = &[\n    (\"threads\", 1),\n    (\"threads/log\", 1),\n];\npub const SHAPE_CONTRACT_DEFAULT: u32 = 1;\n";
    let missing = "pub const SHAPE_CONTRACT_DEFAULT: u32 = 1;\n";
    let malformed = "pub const SHAPE_CONTRACT_VERSIONS: &[(&str, u32)] = &[\n    (threads, one),\n];\npub const SHAPE_CONTRACT_DEFAULT: u32 = 1;\n";

    assert_eq!(
        check_inventory(&inv, &[], &shapes, &contracts_of(readable), "shapewire@6445362"),
        Vec::new(),
        "POSITIVE SENTINEL: the same fixture with a readable table is clean"
    );
    for (what, m) in [("missing", missing), ("malformed", malformed)] {
        let fs = check_inventory(&inv, &[], &shapes, &contracts_of(m), "shapewire@6445362");
        let t = text(&fs);
        assert_eq!(fs.len(), 2, "{what}: one finding per shape: {t}");
        assert!(t.contains("`threads` serves contract versions [1] and the seam declares 0"), "{what}: {t}");
        assert!(t.contains("`threads/log` serves contract versions [1] and the seam declares 0"), "{what}: {t}");
    }
}

#[test]
fn a_stamp_that_does_not_match_the_vendored_commit_is_caught() {
    let inv = parse_inventory(INV).unwrap();
    let fs = check_inventory(&inv, &[], &[], &Contracts::uniform(1), "shapewire@deadbee");
    let t = text(&fs);
    assert!(t.contains("SHAPE_VERSION says `shapewire@deadbee`"), "{t}");
    assert!(t.contains("vendor-chassis.sh is the only thing that may move either"), "{t}");
}

/// Right-to-left, because `threads/{id}/send` and `threads/create` have the
/// same segment count and only the tail distinguishes them. A left-to-right
/// matcher would route `threads/create` against `threads/{id}/send`.
#[test]
fn routing_is_right_to_left_like_the_producers() {
    let m: Vec<String> = vec!["threads/{id}/send".into(), "threads/create".into()];
    assert!(routes(&m, "threads/abc/send"));
    assert!(routes(&m, "threads/create"));
    assert!(!routes(&m, "threads/abc/sendx"));
    assert!(!routes(&m, "threads/abc/send/extra"));
}

#[test]
fn an_inventory_missing_a_key_is_an_error_and_not_an_empty_pass() {
    assert!(parse_inventory("{}").is_err());
    assert!(parse_inventory(r#"{"source_commit":"x","methods":[],"shapes":[]}"#).is_err());
    assert!(
        parse_inventory(r#"{"source_commit":"x","methods":["a"],"shapes":[]}"#).is_err(),
        "an empty shape list must not read as a surface with nothing wrong with it"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
//  The scanners, which are the guard's eyes
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn the_scanners_read_the_real_reference() {
    let dir = seam_dir();
    let commands = std::fs::read_to_string(dir.join("commands.rs")).unwrap();
    let templates = scan_templates(&commands, "TEMPLATES");
    // **THE PRODUCER'S OWN SPELLING OF THE SUBJECT SEGMENT**, which is what the
    // method tokens carry and what the `const` assertions beside `TEMPLATES` hold
    // this list to. It said `{id}` while the list was hand-written and the guard
    // matched it by wildcard; now one fact has one spelling.
    assert!(
        templates.contains(&"threads/{thread_id}/send".to_string()),
        "{templates:?}"
    );
    assert_eq!(templates.len(), 6, "{templates:?}");

    let held = std::fs::read_to_string(dir.join("held.rs")).unwrap();
    let shapes = scan_shapes(&held);
    for want in ["threads", "threads/log", "threads/summary", "accounts"] {
        assert!(shapes.contains(&want.to_string()), "{want} not in {shapes:?}");
    }

    let mod_rs = std::fs::read_to_string(dir.join("mod.rs")).unwrap();
    // PER SHAPE: the table parses, every shape the book holds has a row, and
    // `accounts` states 2 (its `threads` cell is gone, R2.S8) while the rest
    // state 1. The row count is the sentinel: a scanner that read nothing
    // would hand back an empty table and every lookup would be the default.
    let contracts = scan_contracts(&mod_rs).expect("SHAPE_CONTRACT_VERSIONS parses");
    assert_eq!(contracts.default, 1);
    // 9 since `accounts/gauge-history` got its row (R3.S2.10, HOLDS R15): the
    // book held it and the table did not, which is the red this loop exists for.
    assert_eq!(contracts.per_shape.len(), 9, "{contracts:?}");
    assert_eq!(contracts.of("accounts"), 2, "{contracts:?}");
    assert_eq!(contracts.of("threads"), 1, "{contracts:?}");
    for s in shapes
        .iter()
        .filter(|s| s.contains('/') || ["threads", "accounts", "narration", "heartbeat"].contains(&s.as_str()))
    {
        assert!(
            contracts.per_shape.iter().any(|(n, _)| n == s),
            "the book holds `{s}` and the contract table has no row for it: {contracts:?}"
        );
    }
    assert!(scan_const(&mod_rs, "SHAPE_VERSION")
        .unwrap()
        .starts_with("shapewire@"));
}

/// A scanner that found nothing would make every inventory check vacuously
/// green, so an empty read is a failure rather than a pass.
#[test]
fn an_empty_scan_is_not_mistaken_for_a_clean_surface() {
    assert_eq!(scan_templates("nothing here", "TEMPLATES"), Vec::<String>::new());
    let inv = parse_inventory(INV).unwrap();
    // Guard against the trap: with no templates the check is silent, which is
    // why the CLI reads the real file and this test states the dependency.
    assert_eq!(check_inventory(&inv, &[], &[], &Contracts::uniform(1), "shapewire@6445362"), Vec::new());
    assert!(
        !scan_templates(
            &std::fs::read_to_string(seam_dir().join("commands.rs")).unwrap(),
            "TEMPLATES"
        )
        .is_empty(),
        "the real reference must not scan empty, or the line above is the whole guard"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
//  The cell guard — `shapewire::cell`, routed the way methods are
// ─────────────────────────────────────────────────────────────────────────────
//
// The `pending` hole: a face read a cell no producer declared and a word no
// producer emitted, and both read as "not yet" for months. These forcings are
// the guard that makes the next one a red test. Every red below has a POSITIVE
// control beside it, because a scanner that found nothing would make every
// cell check vacuously green — which is the same shape of defect, one layer up.

/// An inventory WITH a manifest: `threads` declares `status` (with a closed
/// `state`), `liveness` and `newest`; `threads/log` declares `events`; the
/// shape-level `limits`. Written by hand so the forcings do not depend on the
/// producer's real manifest moving.
const INV_WITH_CELLS: &str = r#"{
  "source_commit": "6445362aa8b9e32c3cc085d6ece327243680a748",
  "methods": ["threads/create", "threads/{thread_id}/send"],
  "shapes": [
    {"name": "threads", "version": 1, "serves": [1],
     "cells": [
       {"name": "status", "dtype": "struct:StateLevel", "required": true,
        "fields": [{"name": "state", "dtype": "enum[\"active\", \"idle\"]", "required": true, "words": ["active", "idle"]},
                   {"name": "executing", "dtype": "scalar:Bool", "required": true}]},
       {"name": "liveness", "dtype": "struct:LivenessRow", "required": true,
        "fields": [{"name": "busy", "dtype": "scalar:Bool", "required": true}]},
       {"name": "newest", "dtype": "struct:NewestRow", "required": true,
        "fields": [{"name": "kind", "dtype": "enum[\"user_message\"]", "required": true, "words": ["user_message"]}]}
     ],
     "shared_cells": [{"name": "limits", "dtype": "struct:Limits", "required": true,
                       "fields": [{"name": "query_max_limit", "dtype": "scalar:Int", "required": true}]}]},
    {"name": "threads/log", "version": 1, "serves": [1],
     "cells": [{"name": "events", "dtype": "list<struct:LogRow>", "required": true,
                "fields": [{"name": "seq", "dtype": "cursor:event", "required": true}]}],
     "shared_cells": []}
  ]
}"#;

const HELD_CONSTS: &str = r#"
pub const THREADS: &str = "threads";
pub const LOG: &str = "threads/log";
pub const FLEET: &str = "fleet";
"#;

/// A `view.rs` that reads only what the inventory declares.
const VIEW_CLEAN: &str = r#"
pub fn status(h: &Held, id: &str) -> Fact<ThreadStatus> {
    cell(h, THREADS, id, "status", |v| {
        Some(ThreadStatus { state: v.get("state").and_then(Value::as_str)?.to_string(),
                            executing: v.get("executing").and_then(Value::as_bool)? })
    })
}
pub fn log_rows(h: &Held, id: &str) -> Fact<Vec<Value>> {
    items(h, LOG, id, "events")
}
pub fn limit(h: &Held) -> Fact<u64> {
    shared_cell(h, THREADS, "limits", |v| v.get("query_max_limit").and_then(Value::as_u64))
}
pub fn running(s: &ThreadStatus) -> bool {
    s.executing || matches!(s.state.as_str(), "active" | "idle")
}
pub fn newest_is_a_message(r: &Value) -> bool {
    r.get("kind").and_then(Value::as_str) == Some("user_message")
}
"#;

fn guard(view: &str, held: &str, inv: &str) -> Vec<Finding> {
    let inv = parse_inventory(inv).unwrap();
    check_cells(
        &inv,
        "view.rs",
        &scan_cell_reads(view),
        &scan_shape_consts(held),
        &scan_words(view),
        &scan_field_reads(view),
    )
}

/// **THE POSITIVE CONTROL.** A clean `view.rs` against an inventory that
/// declares everything it reads: no findings — and the scanners SAW the reads,
/// which is asserted first, because an empty scan would make the empty finding
/// list meaningless.
#[test]
fn the_cell_guard_is_green_on_a_view_that_reads_only_what_is_declared() {
    let reads = scan_cell_reads(VIEW_CLEAN);
    assert_eq!(reads.len(), 3, "{reads:?}");
    assert!(reads.iter().any(|r| r.shape_const == "THREADS" && r.cell == "status" && !r.shared));
    assert!(reads.iter().any(|r| r.shape_const == "LOG" && r.cell == "events" && !r.shared));
    assert!(reads.iter().any(|r| r.shape_const == "THREADS" && r.cell == "limits" && r.shared));
    let words: Vec<String> = scan_words(VIEW_CLEAN).into_iter().map(|l| l.text).collect();
    assert_eq!(words, vec!["active", "idle", "user_message"], "`==`, `== Some(` and `matches!` are all seen, in line order");
    let fields: Vec<String> = scan_field_reads(VIEW_CLEAN).into_iter().map(|l| l.text).collect();
    assert_eq!(fields, vec!["state", "executing", "query_max_limit", "kind"]);
    assert_eq!(guard(VIEW_CLEAN, HELD_CONSTS, INV_WITH_CELLS), Vec::new());
}

/// **A CELL THE PRODUCER DOES NOT DECLARE IS CAUGHT — AT TWO, NOT AT ONE**, one
/// per-key and one shape-level, so a short-circuit after the first would show.
/// This is `pending`, at the cell layer.
#[test]
fn a_cell_the_producer_does_not_declare_is_caught() {
    let view = format!(
        "{VIEW_CLEAN}\npub fn pending(h: &Held, id: &str) -> Fact<bool> {{\n    cell(h, THREADS, id, \"pending\", Value::as_bool)\n}}\n\
         pub fn fleet(h: &Held) -> Fact<u64> {{\n    shared_cell(h, THREADS, \"fleet_size\", Value::as_u64)\n}}\n"
    );
    let fs = guard(&view, HELD_CONSTS, INV_WITH_CELLS);
    let t = text(&fs);
    assert_eq!(fs.len(), 2, "{t}");
    assert!(t.contains("view.rs:") && t.contains(": reads per-key cell `pending` of `threads`, which the producer does not declare"), "{t}");
    assert!(t.contains("`threads` declares: status, liveness, newest"), "{t}");
    assert!(t.contains("reads as \"not yet\" and means \"never\""), "{t}");
    assert!(t.contains("WHAT IS MISSING: either the cell on the producer (`Shape::CELLS`, then re-run tools/make-inventory.py) or this read."), "{t}");
    assert!(t.contains("reads shape-level cell `fleet_size` of `threads`"), "{t}");
    assert!(t.contains("`threads` declares: limits"), "{t}");
}

/// **A WORD THE PRODUCER DOES NOT DECLARE IS CAUGHT**, in both spellings this
/// guard reads — `== "…"` and a `matches!` alternative. This is `pending`, at
/// the word layer: `self.state == "pending"` was a predicate that was false for
/// every status any producer ever folded.
#[test]
fn a_word_the_producer_does_not_declare_is_caught() {
    let view = format!(
        "{VIEW_CLEAN}\npub fn pending(s: &ThreadStatus) -> bool {{ s.state == \"pending\" }}\n\
         pub fn parked(s: &ThreadStatus) -> bool {{ matches!(s.state.as_str(), \"active\" | \"parked\") }}\n"
    );
    let fs = guard(&view, HELD_CONSTS, INV_WITH_CELLS);
    let t = text(&fs);
    assert_eq!(fs.len(), 2, "{t}");
    assert!(t.contains("view.rs:") && t.contains(": compares against the word `pending`, which no closed vocabulary the producer declares contains"), "{t}");
    assert!(t.contains(": compares against the word `parked`"), "{t}");
    assert!(t.contains("threads.status.state = [active, idle]"), "{t}");
    assert!(t.contains("threads.newest.kind = [user_message]"), "{t}");
    assert!(t.contains("false for ever"), "{t}");
    assert!(t.contains("WHAT IS MISSING: either the word on the producer (an `Enum` in its `Shape::CELLS`, then re-run tools/make-inventory.py) or this comparison."), "{t}");
}

/// **A FIELD THE PRODUCER DOES NOT DECLARE IS CAUGHT.** `.get("pending")` can
/// only ever answer `None`.
#[test]
fn a_field_the_producer_does_not_declare_is_caught() {
    let view = format!("{VIEW_CLEAN}\npub fn p(v: &Value) -> bool {{ v.get(\"pending\").is_some() }}\n");
    let fs = guard(&view, HELD_CONSTS, INV_WITH_CELLS);
    let t = text(&fs);
    assert_eq!(fs.len(), 1, "{t}");
    assert!(t.contains("view.rs:") && t.contains(": reads `.get(\"pending\")`, and no cell or field the producer declares on any shape is called `pending`"), "{t}");
    assert!(t.contains("can only ever answer `None`"), "{t}");
}

/// **A READ NAMING A SHAPE CONST `held.rs` DOES NOT DEFINE IS A FINDING**, not a
/// read the scanner skips.
#[test]
fn a_read_of_an_unknown_shape_const_is_caught() {
    let view = format!("{VIEW_CLEAN}\npub fn x(h: &Held) -> Fact<u64> {{ cell(h, PROJECTS, \"k\", \"n\", Value::as_u64) }}\n");
    let fs = guard(&view, HELD_CONSTS, INV_WITH_CELLS);
    let t = text(&fs);
    assert_eq!(fs.len(), 1, "{t}");
    assert!(t.contains("view.rs:") && t.contains(": reads a cell of `PROJECTS`, which is not a shape const held.rs declares"), "{t}");
}

/// **AN INVENTORY WITH NO MANIFEST IS A FINDING, NOT A PASS AND NOT A FAIL OF
/// EVERY READ.** The pre-manifest `INV` fixture, which declares no cells: a read
/// against it cannot be checked, and the guard says so once per shape rather
/// than passing the read (the hole) or refusing every read (a lie about the
/// read).
#[test]
fn an_inventory_with_no_manifest_is_a_finding_not_a_pass() {
    let fs = guard(VIEW_CLEAN, HELD_CONSTS, INV);
    let t = text(&fs);
    assert_eq!(fs.len(), 2, "{t}");
    assert!(t.contains("inventory.json carries no cell manifest for `threads`: it was taken before the producer declared its cells"), "{t}");
    assert!(t.contains("inventory.json carries no cell manifest for `threads/log`"), "{t}");
    assert!(t.contains("Re-run tools/make-inventory.py"), "{t}");
    // And an inventory that says `"cells": []` is a STATEMENT — nothing declared —
    // so a read against it is `CellNotDeclared`, not `ManifestUnstated`.
    let empty = INV.replace(
        r#"{"name": "threads", "version": 1, "serves": [1]}"#,
        r#"{"name": "threads", "version": 1, "serves": [1], "cells": [], "shared_cells": []}"#,
    );
    let fs = guard(VIEW_CLEAN, HELD_CONSTS, &empty);
    let t = text(&fs);
    assert!(t.contains("reads per-key cell `status` of `threads`, which the producer does not declare"), "{t}");
    assert!(t.contains("`threads` declares: nothing at this level"), "{t}");
}

/// **THE LITERAL SCANNERS FIND NOTHING IN THE REFERENCE ANY MORE, AND THAT IS THE
/// HANDOVER RATHER THAN A REGRESSION.**
///
/// This test used to assert that `view.rs` made at least sixteen cell reads the
/// scanners could see, because the scanners were the MECHANISM: a read spelled any
/// other way was a read nobody checked. `view.rs` now takes
/// `shapewire::surface::Carried` tokens out of the generated `inventory.rs`, so
/// there is no cell literal, no quoted word and no `.get("field")` left — and an
/// undeclared cell, word or field is refused by RUSTC, in the face's own build,
/// before this tool runs. `lib/shapewire/prove-surface-gate.sh` forces each of
/// those reds with the real message.
///
/// So the assertion that used to live here is now TWO assertions, and both are
/// kept because dropping either would leave a probe that cannot fail:
///
/// 1. The literal scanners find NOTHING, asserted positively. If a read ever comes
///    back as a literal, this is red and the guard below starts checking it again.
/// 2. The real `view.rs` names at least sixteen TOKENS, counted here, so "nothing
///    to check" cannot be confused with "nothing being read".
#[test]
fn the_cell_guard_reads_the_real_reference_and_it_routes_clean() {
    let dir = seam_dir();
    let view = std::fs::read_to_string(dir.join("view.rs")).unwrap();
    let held = std::fs::read_to_string(dir.join("held.rs")).unwrap();
    let inv_text = std::fs::read_to_string(dir.join("inventory.json")).unwrap();

    let reads = scan_cell_reads(&view);
    assert_eq!(
        reads,
        Vec::new(),
        "the reference reads cells through sealed tokens, not literals; a literal \
         here means a read the compiler stopped checking"
    );
    let words: Vec<String> = scan_words(&view).into_iter().map(|l| l.text).collect();
    assert!(
        words.is_empty(),
        "the reference compares against no quoted word: {words:?}"
    );

    // AND THE READS ARE STILL THERE — counted through the token form, so an empty
    // finding list cannot be read as an empty `view.rs`.
    let tokens = view.matches("inventory::").count();
    assert!(
        tokens >= 16,
        "the reference names at least sixteen generated consts: {tokens}"
    );
    for want in [
        "inventory::threads::STATUS",
        "inventory::threads::LIVENESS",
        "inventory::threads::SEEN_SEQ",
        "inventory::threads::STATUS_TRANSITIONS",
        "inventory::threads_log::EVENTS",
        "inventory::narration::LINES",
        "inventory::threads_recent_updates::THREADS",
        "inventory::threads_summary::TITLE",
        "inventory::threads_summary::TRUNCATED",
        "inventory::threads::status::state::ACTIVE",
        "inventory::threads_log::events::kind::TASK_PROGRESS",
    ] {
        assert!(view.contains(want), "the reference must name {want}");
    }
    assert!(
        !view.contains("::PENDING"),
        "and it names no `pending` anything; there is no const for one"
    );

    // The guard itself still runs and still says nothing, which is what a second
    // witness over an empty input should say.
    let inv = parse_inventory(&inv_text).unwrap();
    let threads = inv.shapes.iter().find(|s| s.name == "threads").unwrap();
    assert!(
        threads.cells.as_ref().is_some_and(|c| c.len() >= 14),
        "the real inventory carries the real manifest"
    );
    let fs = check_cells(
        &inv,
        "view.rs",
        &reads,
        &scan_shape_consts(&held),
        &scan_words(&view),
        &scan_field_reads(&view),
    );
    assert_eq!(fs, Vec::new(), "{}", text(&fs));
}

/// **THE JSON READER READS WHAT `make-inventory.py` WRITES**, nested manifest
/// included, and refuses what is not JSON rather than reading past it.
#[test]
fn the_json_reader_reads_a_nested_manifest_and_refuses_junk() {
    let inv = parse_inventory(INV_WITH_CELLS).unwrap();
    let threads = inv.shapes.iter().find(|s| s.name == "threads").unwrap();
    let cells = threads.cells.as_ref().unwrap();
    assert_eq!(cells.len(), 3);
    assert_eq!(cells[0].fields[0].words, vec!["active", "idle"]);
    assert_eq!(threads.shared_cells.as_ref().unwrap()[0].fields[0].name, "query_max_limit");
    let log = inv.shapes.iter().find(|s| s.name == "threads/log").unwrap();
    assert_eq!(log.shared_cells.as_ref().map(Vec::len), Some(0), "`[]` is an empty manifest, kept apart from none");
    assert!(parse_inventory(INV).unwrap().shapes[0].cells.is_none(), "no key is `None`");

    assert_eq!(parse_json(r#"{"a": [1, 2.5, "x\"yé", true, null, {}]}"#).unwrap().get("a").unwrap().as_arr().unwrap().len(), 6);
    assert!(parse_json("{").is_err());
    assert!(parse_json(r#"{"a": 1} x"#).is_err(), "trailing junk is an error");
    assert!(parse_inventory("not json").unwrap_err().contains("does not parse"));
}

// ─────────────────────────────────────────────────────────────────────────────
//  THE GENERATED INVENTORY — the one check the compiler cannot do
// ─────────────────────────────────────────────────────────────────────────────
//
// `seam/inventory.rs` is what the consumer COMPILES against; `inventory.json` is
// what a reviewer read. One run of `tools/make-inventory.py` writes both from one
// payload, so a disagreement between them is a HAND-EDIT — and a hand-edited
// `inventory.rs` is a consumer compiling against its own opinion of what the
// producer serves. The compiler cannot see it, because the JSON is not a build
// input of anything. These are the forcings for that.

/// A generated module, in the shape the generator actually emits, cut down to two
/// methods and two shapes. The JSON twin is [`GEN_JSON`] below, and the two AGREE
/// — which is what makes every mutation of one of them a forcing of exactly one
/// disagreement.
const GEN_RS: &str = r#"//! generated
#![allow(dead_code)]

use shapewire::surface::{Carried, Method};

pub const SOURCE_REPO: &str = "substrate-interface-layer";
pub const SOURCE_COMMIT: &str = "6445362abcdef0000000000000000000000000aa";
pub const SOURCE_COMMIT_SHORT: &str = "6445362";
pub const METHODS: &[&str] = &[
    "threads/create",
    "threads/{thread_id}/send",
];
pub const SHAPES: &[&str] = &["threads", "threads/log"];

pub mod methods {
    use super::Method;

    pub const THREADS_CREATE: Method<0> = Method::__served("threads/create");
    pub const THREADS_SEND: Method<1> = Method::__served("threads/{thread_id}/send");
}

pub mod threads {
    use super::Carried;

    pub const SHAPE: &str = "threads";

    pub const CELLS: &[&str] = &["status", "queued"];

    pub const SHARED: &[&str] = &["fleet_size"];

    pub const STATUS: Carried = Carried::__declared(SHAPE, "status", false);
    pub const QUEUED: Carried = Carried::__declared(SHAPE, "queued", false);
    pub const FLEET_SIZE: Carried = Carried::__declared(SHAPE, "fleet_size", true);

    pub mod status {
        pub const FIELDS: &[&str] = &["state"];
        pub const STATE: &str = "state";
        pub mod state {
            pub const WORDS: &[&str] = &["active", "idle"];
            pub const ACTIVE: &str = "active";
            pub const IDLE: &str = "idle";
        }
    }
}

pub mod threads_log {
    use super::Carried;

    pub const SHAPE: &str = "threads/log";

    pub const CELLS: &[&str] = &["events"];

    pub const SHARED: &[&str] = &[];

    pub const EVENTS: Carried = Carried::__declared(SHAPE, "events", false);
}
"#;

const GEN_JSON: &str = r#"{
  "source_commit": "6445362abcdef0000000000000000000000000aa",
  "methods": ["threads/create", "threads/{thread_id}/send"],
  "shapes": [
    {"name": "threads", "version": 1, "serves": [1],
     "cells": [{"name": "status"}, {"name": "queued"}],
     "shared_cells": [{"name": "fleet_size"}]},
    {"name": "threads/log", "version": 1, "serves": [1],
     "cells": [{"name": "events"}],
     "shared_cells": []}
  ]
}"#;

fn gen_findings(rs: &str, json: &str) -> Vec<Finding> {
    let inv = parse_inventory(json).expect("the json fixture parses");
    match parse_generated(rs) {
        Err(why) => vec![Finding::GeneratedMissing { why }],
        Ok(g) => check_generated(&inv, &g),
    }
}

/// **THE GREEN IS A REAL GREEN**, and it is the positive control for every red
/// below: the two fixtures describe one surface, so the guard says nothing.
#[test]
fn the_generated_guard_is_green_when_the_two_files_are_one_surface() {
    assert_eq!(gen_findings(GEN_RS, GEN_JSON), Vec::new());
}

/// **AND THE PARSER IS NOT SILENTLY EMPTY**, which is the one way this guard could
/// be a decoration: a `Generated` with no methods and no shapes agrees with
/// everything. Asserted on the fixture AND on the real generated file.
#[test]
fn the_generated_parser_reads_what_the_generator_writes() {
    let g = parse_generated(GEN_RS).unwrap();
    assert_eq!(g.source_commit, "6445362abcdef0000000000000000000000000aa");
    assert_eq!(g.methods, vec!["threads/create", "threads/{thread_id}/send"]);
    assert_eq!(
        g.method_tokens,
        vec!["threads/create", "threads/{thread_id}/send"],
        "the mints, read apart from the slice — a hand-added const is the whole \
         reason both are read"
    );
    assert_eq!(
        g.shapes,
        vec![
            GenShape {
                name: "threads".to_string(),
                cells: vec!["status".to_string(), "queued".to_string()],
                shared: vec!["fleet_size".to_string()],
                tokens: vec![
                    ("status".to_string(), false),
                    ("queued".to_string(), false),
                    ("fleet_size".to_string(), true),
                ],
            },
            GenShape {
                name: "threads/log".to_string(),
                cells: vec!["events".to_string()],
                shared: vec![],
                tokens: vec![("events".to_string(), false)],
            },
        ],
        "and `SHARED: &[]` reads as an empty list rather than a missing one"
    );

    // The REAL file, so a parser that drifted from the generator's output is a red
    // here and not a silent pass everywhere.
    let real = std::fs::read_to_string(seam_dir().join("inventory.rs"))
        .expect("the kit's generated inventory");
    let g = parse_generated(&real).expect("the real generated inventory parses");
    // **AND NOW NOT A LITERAL EITHER.** This read `52` and stayed red from the
    // day the two queue reads left the producer (50) — the same mistake the
    // paragraph below records for the shape count. The count is the committed
    // `inventory.json`'s own, read beside the generated file it must agree with.
    let json = std::fs::read_to_string(seam_dir().join("inventory.json"))
        .expect("the kit's committed inventory");
    let inv = parse_inventory(&json).expect("the committed inventory parses");
    // THE SENTINEL, asserted apart from the equality so two empty lists cannot
    // agree their way to green. **A FLOOR, NOT A LITERAL (R3.S2.10, HOLDS R15)**:
    // this read `== 50` and went red the day the producer grew (`agents/*`, 64
    // at the cauldron regeneration) — the third time a literal here has stood
    // between a correct regeneration and green. The equalities below are the
    // check; this line only rules out a degenerate read. 50 was the surface at
    // R2.S8, and a scrape that lost `WS_METHODS` leaves the chassis's 5.
    assert!(inv.methods.len() >= 50, "the committed inventory names {} methods; a whole \
        surface is at least the 50 it had at R2.S8", inv.methods.len());
    assert_eq!(g.methods.len(), inv.methods.len(), "the producer's whole method surface");
    assert_eq!(g.method_tokens.len(), inv.methods.len(), "and one mint for each of them");
    // **NOT A LITERAL, AND IT USED TO BE ONE.** This read `7` and went red the
    // moment the `warnings` shape landed as the eighth — with `left: 8, right: 7`
    // under the message "one module per shape it serves", which is a sentence
    // about a RELATIONSHIP and was being checked as a constant. The literal was
    // the defect, not the shape: every shape the producer ever adds would have
    // reddened this line, and the only available repairs are to retype the number
    // or to measure the thing the message claims.
    //
    // So it is measured against `inventory.json` — the producer's own stated
    // surface, and the file this `inventory.rs` was generated FROM in the same
    // run. The floor keeps it from going vacuous: a truncated or empty inventory
    // would otherwise make "the same count as the JSON" true for free.
    let inv_text = std::fs::read_to_string(seam_dir().join("inventory.json"))
        .expect("the kit's committed inventory");
    let inv = parse_inventory(&inv_text).expect("the real inventory parses");
    assert!(
        inv.shapes.len() >= 7,
        "the inventory names a plausible surface, so the comparison below is not \
         vacuous: {} shape(s)",
        inv.shapes.len()
    );
    assert_eq!(g.shapes.len(), inv.shapes.len(), "one module per shape it serves");
    let cell_tokens: usize = g.shapes.iter().map(|s| s.tokens.len()).sum();
    assert!(cell_tokens >= 40, "and one mint per declared cell: {cell_tokens}");
    assert_eq!(g.source_commit.len(), 40, "a full commit, not a short one");
}

/// **THE REAL PAIR ROUTES CLEAN.** The kit's own `inventory.rs` and
/// `inventory.json` are one surface at one commit — which is what one run of the
/// generator produces, and what a reviewer is entitled to assume.
#[test]
fn the_generated_guard_reads_the_real_kit_and_it_routes_clean() {
    let dir = seam_dir();
    let rs = std::fs::read_to_string(dir.join("inventory.rs")).expect("inventory.rs");
    let json = std::fs::read_to_string(dir.join("inventory.json")).expect("inventory.json");
    let fs = gen_findings(&rs, &json);
    assert_eq!(fs, Vec::new(), "{}", text(&fs));
}

/// **TWO COMMITS CANNOT COME OUT OF ONE RUN.**
#[test]
fn a_generated_inventory_at_another_commit_is_caught() {
    let rs = GEN_RS.replace(
        "pub const SOURCE_COMMIT: &str = \"6445362abcdef0000000000000000000000000aa\";",
        "pub const SOURCE_COMMIT: &str = \"deadbee0000000000000000000000000000000ff\";",
    );
    let fs = gen_findings(&rs, GEN_JSON);
    let t = text(&fs);
    assert_eq!(fs.len(), 1, "{t}");
    assert!(t.contains("inventory.rs was generated at `deadbee"), "{t}");
    assert!(t.contains("and inventory.json was taken at `6445362"), "{t}");
    assert!(t.contains("the consumer is compiling against a surface its own"), "{t}");
    assert!(t.contains("tools/make-inventory.py --rust-only"), "{t}");
}

/// **A METHOD IN THE RUST THAT THE PRODUCER DOES NOT SERVE — AT TWO, NOT AT ONE.**
///
/// This is the hand-edit that matters: it is the ONE way a dead verb can still
/// reach the wire after `Link::request` started demanding a token, because a const
/// somebody added to `inventory.rs` by hand compiles exactly like a generated one.
/// Two names rather than one, for the reason every N>=2 rule in this kit exists:
/// one cannot distinguish "the check fires" from "the check fires once and stops".
#[test]
fn a_method_added_to_the_generated_rust_by_hand_is_caught() {
    let rs = GEN_RS.replace(
        "    \"threads/{thread_id}/send\",\n",
        "    \"threads/{thread_id}/send\",\n    \"threads/{thread_id}/unpend\",\n    \
         \"threads/{thread_id}/replace\",\n",
    );
    let fs = gen_findings(&rs, GEN_JSON);
    let t = text(&fs);
    assert_eq!(fs.len(), 1, "{t}");
    assert!(t.contains("disagree about the method list"), "{t}");
    assert!(
        t.contains(
            "in inventory.rs and NOT in inventory.json: threads/{thread_id}/replace, \
             threads/{thread_id}/unpend"
        ),
        "{t}"
    );
    assert!(t.contains("is a hand-edit"), "{t}");
}

/// **AND THE OTHER DIRECTION: A METHOD THE PRODUCER SERVES AND THE RUST LOST.**
/// A face cannot send it, which is not dangerous — but it means the Rust is stale,
/// and a stale Rust is the state in which the next real method is unreachable.
#[test]
fn a_method_missing_from_the_generated_rust_is_caught() {
    let rs = GEN_RS.replace("    \"threads/create\",\n", "");
    let fs = gen_findings(&rs, GEN_JSON);
    let t = text(&fs);
    assert_eq!(fs.len(), 1, "{t}");
    assert!(
        t.contains("in inventory.json and NOT in inventory.rs: threads/create"),
        "{t}"
    );
}

/// **A CELL LIST THAT DISAGREES IS CAUGHT, PER SHAPE AND PER LEVEL** — and the
/// finding names which shape and which level, because `limits` is declared on two
/// shapes and a cell name alone would not say where to look.
#[test]
fn a_cell_list_that_disagrees_is_caught_naming_the_shape_and_the_level() {
    let rs = GEN_RS
        .replace(
            "pub const CELLS: &[&str] = &[\"status\", \"queued\"];",
            "pub const CELLS: &[&str] = &[\"status\", \"queued\", \"pending\"];",
        )
        .replace(
            "pub const SHARED: &[&str] = &[\"fleet_size\"];",
            "pub const SHARED: &[&str] = &[];",
        );
    let fs = gen_findings(&rs, GEN_JSON);
    let t = text(&fs);
    assert_eq!(fs.len(), 2, "one finding per level, not one per file: {t}");
    assert!(t.contains("disagree about `threads`'s per-key cells"), "{t}");
    assert!(t.contains("in inventory.rs and NOT in inventory.json: pending"), "{t}");
    assert!(t.contains("disagree about `threads`'s shape-level cells"), "{t}");
    assert!(
        t.contains("in inventory.json and NOT in inventory.rs: fleet_size"),
        "{t}"
    );
}

/// **A SHAPE MODULE THE JSON DOES NOT NAME IS CAUGHT, AND THE CELL COMPARISON
/// STOPS THERE.** Comparing manifests against a shape set that does not match
/// would report every shape twice and bury the one finding that explains the rest.
#[test]
fn a_shape_the_json_does_not_name_is_caught_and_stops_the_cell_pass() {
    let rs = GEN_RS.replace(
        "pub mod threads_log {",
        "pub mod warnings {
    pub const SHAPE: &str = \"warnings\";
    pub const CELLS: &[&str] = &[\"kinds\"];
    pub const SHARED: &[&str] = &[];
}

pub mod threads_log {",
    );
    let fs = gen_findings(&rs, GEN_JSON);
    let t = text(&fs);
    assert_eq!(fs.len(), 1, "{t}");
    assert!(t.contains("disagree about the shape list"), "{t}");
    assert!(t.contains("in inventory.rs and NOT in inventory.json: warnings"), "{t}");
}

/// **A FILE THAT IS NOT A GENERATED INVENTORY IS A FINDING, NOT A PASS.**
///
/// The three ways it can happen, each with its own message, because the
/// alternative is the worst outcome this guard has: an absent or unparsable
/// `inventory.rs` read as "nothing to disagree with".
#[test]
fn an_inventory_rs_that_is_not_one_is_caught_rather_than_skipped() {
    for (label, rs, needle) in [
        ("absent", "", "declares no SOURCE_COMMIT"),
        (
            "no method list",
            "pub const SOURCE_COMMIT: &str = \"x\";\n",
            "declares no METHODS",
        ),
        (
            "a method list with no mints",
            "pub const SOURCE_COMMIT: &str = \"x\";\npub const METHODS: &[&str] = &[\"a/b\"];\n",
            "mints no `Method` token, so nothing in it can be sent",
        ),
        (
            "no shape modules",
            "pub const SOURCE_COMMIT: &str = \"x\";\n\
             pub const METHODS: &[&str] = &[\"a/b\"];\n\
             pub mod methods { pub const A_B: Method<0> = Method::__served(\"a/b\"); }\n",
            "declares no shape modules",
        ),
    ] {
        let fs = gen_findings(rs, GEN_JSON);
        let t = text(&fs);
        assert_eq!(fs.len(), 1, "{label}: {t}");
        assert!(t.contains(needle), "{label}: {t}");
        assert!(t.contains("tools/make-inventory.py --rust-only"), "{label}: {t}");
    }
}

/// **AND A SHAPE MODULE WITH NO `SHAPE` CONST IS REFUSED RATHER THAN SKIPPED.**
/// Skipping it would mean a hand-edit could hide a whole shape's manifest from
/// this guard by deleting one line.
#[test]
fn a_shape_module_with_no_shape_const_is_refused() {
    let rs = GEN_RS.replace("    pub const SHAPE: &str = \"threads\";\n", "");
    let fs = gen_findings(&rs, GEN_JSON);
    let t = text(&fs);
    assert_eq!(fs.len(), 1, "{t}");
    assert!(t.contains("module `threads` declares no SHAPE const"), "{t}");
}

/// **A MINT ADDED WITHOUT TOUCHING THE SLICE BESIDE IT — THE EDIT THAT WOULD
/// RESURRECT A DEAD VERB, AND THE REASON BOTH ARE READ.**
///
/// `Link::request` demands a `Method`, and the only `Method`s are the consts of
/// this file. So the ONE way a face can send a method the producer does not serve
/// is a hand-added const here — and it compiles exactly like a generated one. A
/// guard that only compared the `METHODS` slice would pass this, because the
/// forger has no reason to touch the slice: nothing reads it.
///
/// Two names, not one, for the reason every N>=2 rule in this kit exists.
#[test]
fn a_method_token_minted_without_the_slice_is_caught() {
    let rs = GEN_RS.replace(
        "    pub const THREADS_SEND: Method<1> = Method::__served(\"threads/{thread_id}/send\");",
        "    pub const THREADS_SEND: Method<1> = Method::__served(\"threads/{thread_id}/send\");\n    \
         pub const THREADS_UNPEND: Method<1> = Method::__served(\"threads/{thread_id}/unpend\");\n    \
         pub const THREADS_REPLACE: Method<1> = Method::__served(\"threads/{thread_id}/replace\");",
    );
    let fs = gen_findings(&rs, GEN_JSON);
    let t = text(&fs);
    assert_eq!(fs.len(), 1, "the slice is untouched, so only the tokens differ: {t}");
    assert!(t.contains("disagree about the METHOD TOKENS"), "{t}");
    assert!(t.contains("the consts a consumer can actually name"), "{t}");
    assert!(
        t.contains(
            "in inventory.rs and NOT in inventory.json: threads/{thread_id}/replace, \
             threads/{thread_id}/unpend"
        ),
        "{t}"
    );
}

/// **AND THE SAME EDIT AT THE CELL LAYER, WHICH IS `pending` EXACTLY.**
///
/// `view::cell` demands a `Carried`, and the only `Carried`s are the consts of this
/// file. A hand-added `PENDING` const would let `view.rs` read a cell no producer
/// declares and compile — the whole hole this arc exists to close, reopened one
/// line at a time. The finding names the shape and the level, because a cell name
/// alone would not say where to look.
#[test]
fn a_cell_token_minted_without_the_slice_is_caught() {
    let rs = GEN_RS.replace(
        "    pub const QUEUED: Carried = Carried::__declared(SHAPE, \"queued\", false);",
        "    pub const QUEUED: Carried = Carried::__declared(SHAPE, \"queued\", false);\n    \
         pub const PENDING: Carried = Carried::__declared(SHAPE, \"pending\", false);",
    );
    let fs = gen_findings(&rs, GEN_JSON);
    let t = text(&fs);
    assert_eq!(fs.len(), 1, "the slice is untouched, so only the tokens differ: {t}");
    assert!(
        t.contains("disagree about `threads`'s per-key CELL TOKENS"),
        "{t}"
    );
    assert!(t.contains("in inventory.rs and NOT in inventory.json: pending"), "{t}");
}

/// **AND A SHAPE-LEVEL MINT IS KEPT APART FROM A PER-KEY ONE.** The level is on the
/// token, so a `Carried` minted at the wrong level is a read that would answer
/// `NotHydrated` for ever off the wrong block — and it is caught as two findings,
/// one per level, rather than cancelling out.
#[test]
fn a_cell_token_minted_at_the_wrong_level_is_caught_on_both_levels() {
    let rs = GEN_RS.replace(
        "    pub const FLEET_SIZE: Carried = Carried::__declared(SHAPE, \"fleet_size\", true);",
        "    pub const FLEET_SIZE: Carried = Carried::__declared(SHAPE, \"fleet_size\", false);",
    );
    let fs = gen_findings(&rs, GEN_JSON);
    let t = text(&fs);
    assert_eq!(fs.len(), 2, "{t}");
    assert!(t.contains("`threads`'s per-key CELL TOKENS"), "{t}");
    assert!(t.contains("in inventory.rs and NOT in inventory.json: fleet_size"), "{t}");
    assert!(t.contains("`threads`'s shape-level CELL TOKENS"), "{t}");
    assert!(t.contains("in inventory.json and NOT in inventory.rs: fleet_size"), "{t}");
}
