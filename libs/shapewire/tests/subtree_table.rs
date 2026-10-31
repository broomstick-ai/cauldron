//! **UI-b AS A TABLE (R9.UI1b)** — `shapewire::subtree::rollup` over every one
//! of its four branches plus the precedence cases the operator's ruling turns
//! on. Floor-only: no feature, so it runs under `--no-default-features` too.
//!
//! The rule: own active → active; else any descendant active → active; else
//! any descendant parked → parked; else its own state.
//!
//! Every row names the rule that must decide it. A constant implementation
//! (always `Own`, always `SubtreeParked`, …) fails several rows; swapping
//! rules 2 and 3 fails every "active beside parked" row; dropping rule 1 fails
//! the own-active rows. Each forcing is recorded in R9.UI1b-report.md.

use shapewire::subtree::{ancestors, hidden_from_recents, rollup, rollup_in, Rollup};
use std::collections::BTreeMap;

type Row = (
    &'static str,                      // what the row proves
    (&'static str, bool),              // own (state, executing)
    &'static [(&'static str, bool)],   // descendants (state, executing)
    Rollup,                            // the ruled answer
    &'static str,                      // the word a face shows
);

const TABLE: &[Row] = &[
    // ── the four branches ───────────────────────────────────────────────
    ("rule 1: own active, idle below", ("active", true), &[("idle", false)], Rollup::SelfActive, "active"),
    ("rule 1: own retrying keeps its word", ("retrying", false), &[], Rollup::SelfActive, "retrying"),
    ("rule 2: own idle, a descendant active", ("idle", false), &[("active", true)], Rollup::SubtreeActive, "active"),
    ("rule 3: own idle, a descendant parked", ("idle", false), &[("parked", false)], Rollup::SubtreeParked, "parked"),
    ("rule 4: nothing below moves it", ("queued", false), &[("idle", false), ("queued", false)], Rollup::Own, "queued"),
    ("rule 4: no descendants at all", ("idle", false), &[], Rollup::Own, "idle"),
    ("rule 4: own parked, nothing below", ("parked", false), &[], Rollup::Own, "parked"),
    // ── precedence ──────────────────────────────────────────────────────
    ("own parked + descendant active → active", ("parked", false), &[("active", true)], Rollup::SubtreeActive, "active"),
    (
        "own idle + one descendant parked + another active → active",
        ("idle", false),
        &[("parked", false), ("active", true)],
        Rollup::SubtreeActive,
        "active",
    ),
    (
        "the same, active listed first → active",
        ("idle", false),
        &[("active", true), ("parked", false)],
        Rollup::SubtreeActive,
        "active",
    ),
    ("own active beats a parked descendant", ("active", true), &[("parked", false)], Rollup::SelfActive, "active"),
    ("a retrying descendant counts as active", ("idle", false), &[("parked", false), ("retrying", false)], Rollup::SubtreeActive, "active"),
    ("an executing descendant counts as active", ("idle", false), &[("queued", true), ("parked", false)], Rollup::SubtreeActive, "active"),
];

#[test]
fn ui_b_rollup_table_four_branches_and_precedence() {
    let mut wrong = Vec::new();
    for (what, (own, exec), below, want, word) in TABLE {
        let got = rollup(own, *exec, below.iter().copied());
        if got != *want || got.word(own) != *word {
            wrong.push(format!("{what}: got {got:?}/{:?}, want {want:?}/{word:?}", got.word(own)));
        }
    }
    assert!(wrong.is_empty(), "rows the rule answered wrongly:\n{}", wrong.join("\n"));
}

/// **GRANDCHILD-ONLY ACTIVE → ACTIVE, THROUGH THE ROSTER.** `p → c → g`, only
/// `g` is on the roster (active); `c` is idle and absent. Both `p` and `c` are
/// active by rule 2. Beside it a sibling `s` of `c` is parked and on the
/// roster: still active (rule 2 beats rule 3 at any depth). Opposed: the same
/// tree with `g` gone is parked at `p`, and `c` (whose subtree holds only the
/// gone `g`) is its own word.
#[test]
fn ui_b_grandchild_only_active_is_active_through_the_roster() {
    let parent: BTreeMap<&str, &str> = [("c", "p"), ("g", "c"), ("s", "p")].into_iter().collect();
    let anc = |id: &str| ancestors(id, |x| parent.get(x).map(|s| s.to_string()));
    let roster: Vec<(Vec<String>, &str, bool)> =
        vec![(anc("g"), "active", true), (anc("s"), "parked", false)];
    let it = || roster.iter().map(|(a, s, e)| (a, *s, *e));
    assert_eq!(rollup_in("p", "idle", false, it()), Rollup::SubtreeActive);
    assert_eq!(rollup_in("c", "idle", false, it()), Rollup::SubtreeActive);
    let roster: Vec<(Vec<String>, &str, bool)> = vec![(anc("s"), "parked", false)];
    let it = || roster.iter().map(|(a, s, e)| (a, *s, *e));
    assert_eq!(rollup_in("p", "idle", false, it()), Rollup::SubtreeParked);
    assert_eq!(rollup_in("c", "idle", false, it()), Rollup::Own);
}

/// **UI-a: A THREAD WITH A PARENT IS NEVER IN RECENTS**, at any depth; a root
/// (no parent, or an empty one) always may be.
#[test]
fn ui_a_a_thread_with_a_parent_is_hidden_and_a_root_is_not() {
    assert!(hidden_from_recents(Some("p")));
    assert!(hidden_from_recents(Some("grandparent-of-nothing")));
    assert!(!hidden_from_recents(None));
    assert!(!hidden_from_recents(Some("")));
}
