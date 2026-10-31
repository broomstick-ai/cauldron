//! # A thread's status, rolled up over its whole subtree, and the ONE string a face shows (R9.TS1)
//!
//! Operator ruling (relayed by the coordinator, round 42), and his vocabulary is
//! binding: **a thread's status is `active`, `idle`, or an error state —
//! nothing else.** "done" is `idle`.
//!
//! * **`active`** — the thread is running itself, OR any descendant at any
//!   depth is running. This is [`crate::subtree::rollup`]'s rules 1 and 2 (the
//!   UI-b rollup, R9.UI1) applied to EVERY thread, subagents included, not only
//!   to the roots a face happens to draw.
//! * **`error`** — nothing in the subtree is running, and the thread ITSELF is
//!   in an error state. The error states, mapped from the failure states the
//!   producer already has, each with its label:
//!   - [`ERROR_PARKED`] — the derivation's `parked` (a held prompt that cannot
//!     run: a durable park row, or an admission-gate park);
//!   - [`ERROR_FAILED`] — the thread's last managed run reported `failed`;
//!   - [`ERROR_INTERRUPTED`] — its last managed run reported `interrupted`
//!     (ended with no result and not cancelled — "no end seen").
//!   A `cancelled` run is the operator's own act and is `idle`. **An error does
//!   NOT roll up**: a parent whose child failed is `idle` and its children
//!   summary says `1 error`. (`subtree::rollup`'s rule 3 rolled `parked` up;
//!   the operator's three-word ruling supersedes that step — a root with one
//!   old failed subagent must not read `error` for ever.)
//! * **`idle`** — everything else. The producer's `queued` (accepted, not yet
//!   delivered, nothing executing) is `idle`; `retrying` is `active`, because
//!   `subtree::is_active` — the producer's one fleet predicate — says so.
//!
//! **`active_descendants`** counts the descendants, at every depth, that are
//! RUNNING THEMSELVES. A descendant that is active only by rollup is not
//! counted: pinned by the operator's own case, root → idle child → running
//! grandchild reads `active (+1)` at the root AND at the child (the child is
//! not counted in the root's number, or the root would read `+2`).
//!
//! **[`display`] is the one function every face renders through**, and the
//! producer publishes its output on the `threads` shape (`rolled.display`) so a
//! face that is not Rust needs no logic at all. Same for [`children_summary`].
//!
//! In the floor: `std` only, no feature, like [`crate::subtree`].

use std::collections::{BTreeMap, BTreeSet};

use crate::subtree::{self, Rollup};

/// The status of a thread (or its subtree) that is running.
pub const ACTIVE: &str = "active";
/// The status of a thread whose subtree is neither running nor in error.
pub const IDLE: &str = "idle";
/// The status of a thread in an error state ITSELF, with nothing in its subtree running.
pub const ERROR: &str = "error";
/// The closed status vocabulary, in precedence order — the words the
/// `threads` shape declares for `rolled.status`.
pub const STATUS_WORDS: &[&str] = &[ACTIVE, IDLE, ERROR];

/// Error label: a parked thread (the producer's state word `parked`).
pub const ERROR_PARKED: &str = "parked";
/// Error label: the thread's last managed run reported `failed`.
pub const ERROR_FAILED: &str = "failed";
/// Error label: the thread's last managed run reported `interrupted`.
pub const ERROR_INTERRUPTED: &str = "interrupted";
/// Every error label, closed — the words the shape declares for `rolled.error`.
pub const ERROR_LABELS: &[&str] = &[ERROR_PARKED, ERROR_FAILED, ERROR_INTERRUPTED];

/// Which error state, if any, a thread's own word is. The word is the
/// producer's state word, or — for a thread whose state is otherwise `idle` —
/// its last managed run's outcome word (`failed`, `interrupted`), which the
/// producer substitutes before calling [`rolled_subtree`].
pub fn error_of(state: &str) -> Option<&'static str> {
    if subtree::is_parked(state) {
        Some(ERROR_PARKED)
    } else if state == ERROR_FAILED {
        Some(ERROR_FAILED)
    } else if state == ERROR_INTERRUPTED {
        Some(ERROR_INTERRUPTED)
    } else {
        None
    }
}

/// **THE ONE DISPLAY STRING.** `active (+X)` when the thread is active and X
/// descendants are running themselves; plain `active` when X is 0 — never
/// `(+0)`; `idle`; `error: <label>` (or bare `error` with no label).
pub fn display(status: &str, error: Option<&str>, active_descendants: u64) -> String {
    match status {
        ACTIVE if active_descendants > 0 => format!("{ACTIVE} (+{active_descendants})"),
        ACTIVE => ACTIVE.to_string(),
        ERROR => match error {
            Some(label) if !label.is_empty() => format!("{ERROR}: {label}"),
            _ => ERROR.to_string(),
        },
        _ => IDLE.to_string(),
    }
}

/// How a thread's DIRECT children stand, each by its own rolled-up status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChildCounts {
    pub active: u64,
    pub idle: u64,
    pub error: u64,
}

/// **THE CHILDREN SUMMARY** — `N active, M idle`, with `, K error` appended
/// only when K > 0. Replaces the old `N running, M done`.
pub fn children_summary(c: &ChildCounts) -> String {
    let mut s = format!("{} {ACTIVE}, {} {IDLE}", c.active, c.idle);
    if c.error > 0 {
        s.push_str(&format!(", {} {ERROR}", c.error));
    }
    s
}

/// One thread's rolled-up answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rolled {
    /// One of [`STATUS_WORDS`].
    pub status: &'static str,
    /// The error label when `status` is [`ERROR`] (the thread's own).
    pub error: Option<&'static str>,
    /// Descendants, every depth, that are running THEMSELVES.
    pub active_descendants: u64,
    /// `None` for a thread with no children.
    pub children: Option<ChildCounts>,
}

impl Rolled {
    /// [`display`] of this answer.
    pub fn display(&self) -> String {
        display(self.status, self.error, self.active_descendants)
    }
    /// [`children_summary`] of this answer, `None` with no children.
    pub fn children_summary(&self) -> Option<String> {
        self.children.as_ref().map(children_summary)
    }
}

/// **EVERY THREAD IN `top`'s SUBTREE, ROLLED UP** — `top` included.
///
/// `children_of` answers a thread's immediate children; `state_of` its own
/// producer state word and whether an execution holds it. The walk is
/// cycle-safe and visits each thread once (a repeated id is not walked again).
/// A thread's answer depends only on its own subtree, so the answer for any
/// thread in the map is the same as walking from that thread.
pub fn rolled_subtree(
    top: &str,
    children_of: impl Fn(&str) -> Vec<String>,
    state_of: impl Fn(&str) -> (String, bool),
) -> BTreeMap<String, Rolled> {
    // Discover, depth-first, recording each thread's children (unvisited only).
    let mut kids: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stack = vec![top.to_string()];
    seen.insert(top.to_string());
    while let Some(id) = stack.pop() {
        let mut mine = Vec::new();
        for k in children_of(&id) {
            if seen.insert(k.clone()) {
                stack.push(k.clone());
                mine.push(k);
            }
        }
        order.push(id.clone());
        kids.insert(id, mine);
    }
    let states: BTreeMap<String, (String, bool)> =
        order.iter().map(|id| (id.clone(), state_of(id))).collect();

    // Every thread's descendants, as `(state, executing)`, children before
    // parents (reverse discovery order is a valid post-order for a tree).
    let mut below: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut out: BTreeMap<String, Rolled> = BTreeMap::new();
    for id in order.iter().rev() {
        let mut desc: Vec<String> = Vec::new();
        for k in &kids[id] {
            desc.push(k.clone());
            desc.extend(below.get(k).cloned().unwrap_or_default());
        }
        let (own_state, own_exec) = &states[id];
        // THE EXISTING RULE, applied to this thread whatever its depth.
        let rule = subtree::rollup(
            own_state,
            *own_exec,
            desc.iter().map(|d| (states[d].0.as_str(), states[d].1)),
        );
        // Rules 1-2 decide `active`; rule 3 (a parked descendant) is
        // superseded — only the thread's OWN error makes it `error`.
        let error = if rule.active() { None } else { error_of(own_state) };
        let status = match (rule, error) {
            (Rollup::SelfActive | Rollup::SubtreeActive, _) => ACTIVE,
            (_, Some(_)) => ERROR,
            _ => IDLE,
        };
        let active_descendants = desc
            .iter()
            .filter(|d| subtree::is_active(&states[*d].0, states[*d].1))
            .count() as u64;
        let children = if kids[id].is_empty() {
            None
        } else {
            let mut c = ChildCounts::default();
            for k in &kids[id] {
                match out[k].status {
                    ACTIVE => c.active += 1,
                    ERROR => c.error += 1,
                    _ => c.idle += 1,
                }
            }
            Some(c)
        };
        out.insert(
            id.clone(),
            Rolled {
                status,
                error,
                active_descendants,
                children,
            },
        );
        below.insert(id.clone(), desc);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny forest: `(child, parent)` edges and `(id, state, executing)`.
    fn roll(edges: &[(&str, &str)], states: &[(&str, &str, bool)], top: &str) -> BTreeMap<String, Rolled> {
        let edges: Vec<(String, String)> =
            edges.iter().map(|(c, p)| (c.to_string(), p.to_string())).collect();
        let states: BTreeMap<String, (String, bool)> = states
            .iter()
            .map(|(i, s, e)| (i.to_string(), (s.to_string(), *e)))
            .collect();
        rolled_subtree(
            top,
            |id| edges.iter().filter(|(_, p)| p == id).map(|(c, _)| c.clone()).collect(),
            |id| states.get(id).cloned().unwrap_or(("idle".into(), false)),
        )
    }

    // THE OPERATOR'S TEST: root → idle child → running grandchild. Root and
    // child both read `active (+1)` — X counts threads running THEMSELVES, so
    // the child (active only by rollup) is not in the root's count.
    #[test]
    fn his_test_root_idle_child_running_grandchild() {
        let r = roll(&[("c", "r"), ("g", "c")], &[("g", "active", true)], "r");
        assert_eq!(r["r"].display(), "active (+1)");
        assert_eq!(r["c"].display(), "active (+1)", "the subagent gets the rollup");
        assert_eq!(r["g"].display(), "active");
        assert_eq!(r["r"].children_summary().as_deref(), Some("1 active, 0 idle"));
        assert_eq!(r["g"].children_summary(), None, "a leaf has no summary");
    }

    // A running leaf with no active descendants is `active`, never `(+0)`;
    // paired with a running parent of a running child, which DOES show `(+1)`.
    #[test]
    fn a_running_leaf_is_plain_active_never_plus_zero() {
        let r = roll(&[], &[("l", "active", true)], "l");
        assert_eq!(r["l"].display(), "active");
        assert!(!r["l"].display().contains("(+0)"));
        assert_eq!(display(ACTIVE, None, 0), "active");
        let r = roll(&[("k", "p")], &[("p", "active", true), ("k", "retrying", false)], "p");
        assert_eq!(r["p"].display(), "active (+1)");
    }

    #[test]
    fn all_idle_is_idle() {
        let r = roll(&[("a", "r"), ("b", "r"), ("c", "a")], &[("a", "queued", false)], "r");
        for id in ["r", "a", "b", "c"] {
            assert_eq!((id, r[id].status, r[id].display()), (id, IDLE, "idle".to_string()));
        }
        assert_eq!(r["r"].children_summary().as_deref(), Some("0 active, 2 idle"));
    }

    // Depth 4, three running at different depths (2, 3 and 4), with an idle
    // thread between: the right X at every level.
    #[test]
    fn depth_four_with_three_running_at_different_depths() {
        // r → a → b → c → d ; r → e ; a → f
        let edges = [("a", "r"), ("b", "a"), ("c", "b"), ("d", "c"), ("e", "r"), ("f", "a")];
        let states = [("b", "active", true), ("d", "retrying", false), ("e", "active", true)];
        let r = roll(&edges, &states, "r");
        let got: Vec<(&str, String)> =
            ["r", "a", "b", "c", "d", "e", "f"].iter().map(|i| (*i, r[*i].display())).collect();
        assert_eq!(
            got,
            vec![
                ("r", "active (+3)".to_string()),
                ("a", "active (+2)".to_string()),
                ("b", "active (+1)".to_string()),
                ("c", "active (+1)".to_string()),
                ("d", "active".to_string()),
                ("e", "active".to_string()),
                ("f", "idle".to_string()),
            ]
        );
        assert_eq!(r["r"].children_summary().as_deref(), Some("2 active, 0 idle"));
        assert_eq!(r["a"].children_summary().as_deref(), Some("1 active, 1 idle"));
    }

    // The summary's words are the operator's, and the old ones are gone.
    #[test]
    fn children_summary_says_active_and_idle_not_running_and_done() {
        let s = children_summary(&ChildCounts { active: 0, idle: 20, error: 0 });
        assert_eq!(s, "0 active, 20 idle");
        assert!(!s.contains("running") && !s.contains("done"), "{s}");
        assert_eq!(children_summary(&ChildCounts { active: 2, idle: 3, error: 1 }), "2 active, 3 idle, 1 error");
    }

    // Each error state has its label; an error does NOT roll up (the parent of
    // a parked child is idle and counts `1 error`); a running sibling makes the
    // parent active; a running thread is never `error` whatever its word.
    #[test]
    fn the_error_state_has_its_label() {
        let r = roll(&[("k", "p")], &[("k", "parked", false)], "p");
        assert_eq!((r["k"].status, r["k"].display()), (ERROR, "error: parked".to_string()));
        assert_eq!((r["p"].status, r["p"].display()), (IDLE, "idle".to_string()));
        assert_eq!(r["p"].children_summary().as_deref(), Some("0 active, 0 idle, 1 error"));
        let r = roll(&[("f", "p"), ("i", "p")], &[("f", "failed", false), ("i", "interrupted", false)], "p");
        assert_eq!(r["f"].display(), "error: failed");
        assert_eq!(r["i"].display(), "error: interrupted");
        assert_eq!(r["p"].children_summary().as_deref(), Some("0 active, 0 idle, 2 error"));
        assert_eq!(display(ERROR, None, 0), "error");
        let r = roll(&[("k", "p"), ("s", "p")], &[("k", "parked", false), ("s", "active", true)], "p");
        assert_eq!(r["p"].display(), "active (+1)");
        assert_eq!(r["p"].children_summary().as_deref(), Some("1 active, 0 idle, 1 error"));
    }

    // A cycle (only a hand-edited ledger) terminates and visits each id once.
    #[test]
    fn a_cycle_terminates() {
        let r = roll(&[("b", "a"), ("a", "b")], &[("b", "active", true)], "a");
        assert_eq!(r["a"].display(), "active (+1)");
        assert_eq!(r.len(), 2);
    }
}
