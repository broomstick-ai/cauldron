//! # Managed subtrees: the two rules every face of `threads` applies (R9.UI1)
//!
//! Two operator rulings (relayed by the coordinator, 2026-10-18), written ONCE
//! here so the producer and every consumer answer them the same way. This
//! module is in the floor: no feature, no dependency beyond `std`, so the
//! threads daemon (`features = ["server"]`) and every face
//! (`default-features = false`) link the same code.
//!
//! * **UI-a — recent threads hide subagents.** A thread with a `parent_id` (a
//!   managed child at any depth) is never in a recents list:
//!   [`hidden_from_recents`].
//! * **UI-b — a parent's status rolls up from its whole subtree.** In order:
//!   1. the thread itself is active → active;
//!   2. else any descendant is active → active;
//!   3. else any descendant is parked → parked;
//!   4. else its own state.
//!
//!   [`rollup`] is the rule; [`rollup_in`] applies it over a roster whose
//!   entries carry their ancestors (`threads/non-idle`'s `ancestors`, which the
//!   daemon computes with [`ancestors`]). Only an active or parked descendant
//!   can move the answer, and every such thread is on the non-idle roster, so
//!   that roster is the whole input the rule needs.
//!
//! "Active" is the predicate the producer already publishes for the fleet
//! (`threads/non-idle`'s module doc, `seam::view::running`):
//! `executing || state ∈ {active, retrying}` — [`is_active`].

use std::collections::BTreeSet;

/// The state word of a parked thread, in the producer's vocabulary
/// (`threads::runtime::STATE_WORDS`).
pub const PARKED: &str = "parked";
/// The state word of a thread holding a lock and running.
pub const ACTIVE: &str = "active";
/// The state word of a thread retrying after a failure; it counts as active.
pub const RETRYING: &str = "retrying";

/// **UI-a.** Is this thread left out of every recents list? True exactly when
/// it has a parent, i.e. it is a managed subagent at any depth. An empty
/// string is not a parent.
pub fn hidden_from_recents(parent_id: Option<&str>) -> bool {
    parent_id.is_some_and(|p| !p.is_empty())
}

/// Is a thread in this state active? The producer's own fleet predicate.
pub fn is_active(state: &str, executing: bool) -> bool {
    executing || state == ACTIVE || state == RETRYING
}

/// Is a thread in this state parked?
pub fn is_parked(state: &str) -> bool {
    state == PARKED
}

/// **UI-b's answer for one thread**, and which rule produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rollup {
    /// Rule 1: the thread itself is active.
    SelfActive,
    /// Rule 2: the thread is not active, and a descendant is.
    SubtreeActive,
    /// Rule 3: nothing in the subtree is active, and a descendant is parked.
    SubtreeParked,
    /// Rule 4: nothing below it moves the answer; its own state stands.
    Own,
}

impl Rollup {
    /// The state word a face shows: `active` or `parked` when the subtree
    /// decided it, the thread's own word otherwise (so an active thread that
    /// is `retrying` still says `retrying`).
    pub fn word<'a>(&self, own: &'a str) -> &'a str {
        match self {
            Rollup::SubtreeActive => ACTIVE,
            Rollup::SubtreeParked => PARKED,
            Rollup::SelfActive | Rollup::Own => own,
        }
    }

    /// Does the thread show as active (rules 1 and 2)?
    pub fn active(&self) -> bool {
        matches!(self, Rollup::SelfActive | Rollup::SubtreeActive)
    }

    /// Does the thread show as parked? Rule 3, or rule 4 on a thread that is
    /// itself parked.
    pub fn parked(&self, own: &str) -> bool {
        match self {
            Rollup::SubtreeParked => true,
            Rollup::Own => is_parked(own),
            _ => false,
        }
    }
}

/// **UI-b, the rule.** `descendants` is every thread in the subtree below this
/// one (any depth) as `(state, executing)`; a descendant that is neither
/// active nor parked may be passed or left out, it changes nothing.
pub fn rollup<'d>(
    own_state: &str,
    own_executing: bool,
    descendants: impl IntoIterator<Item = (&'d str, bool)>,
) -> Rollup {
    if is_active(own_state, own_executing) {
        return Rollup::SelfActive;
    }
    let mut parked = false;
    for (state, executing) in descendants {
        if is_active(state, executing) {
            return Rollup::SubtreeActive;
        }
        parked |= is_parked(state);
    }
    if parked {
        Rollup::SubtreeParked
    } else {
        Rollup::Own
    }
}

/// **UI-b over a roster.** Each roster entry is `(ancestors, state,
/// executing)`; the entries whose `ancestors` name `id` are its descendants.
/// An entry for `id` itself is not its own descendant (its ancestors never
/// name it — [`ancestors`] is cycle-safe and excludes the start).
pub fn rollup_in<'r, A>(
    id: &str,
    own_state: &str,
    own_executing: bool,
    roster: impl IntoIterator<Item = (&'r A, &'r str, bool)>,
) -> Rollup
where
    A: AsRef<[String]> + ?Sized + 'r,
{
    rollup(
        own_state,
        own_executing,
        roster
            .into_iter()
            .filter(|(anc, _, _)| anc.as_ref().iter().any(|a| a == id))
            .map(|(_, s, e)| (s, e)),
    )
}

/// **A THREAD'S ANCESTORS, NEAREST FIRST** — the walk up `parent_of`,
/// cycle-safe (a repeated id, or the start itself, ends the walk), never
/// including `id`. `[]` for a root.
pub fn ancestors(id: &str, parent_of: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    seen.insert(id.to_string());
    let mut cur = parent_of(id);
    while let Some(p) = cur {
        if p.is_empty() || !seen.insert(p.clone()) {
            break;
        }
        cur = parent_of(&p);
        out.push(p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    // Rule 1, opposed: a parent that is itself active is active whatever its
    // children do, and the answer says rule 1 decided it (not rule 2).
    #[test]
    fn rule_1_parent_active_with_idle_descendants_is_active() {
        let r = rollup("active", true, [("idle", false), ("idle", false)]);
        assert_eq!(r, Rollup::SelfActive);
        assert!(r.active());
        // `retrying` with no lock is active too, and keeps its own word.
        let r = rollup("retrying", false, [("parked", false)]);
        assert_eq!((r, r.word("retrying")), (Rollup::SelfActive, "retrying"));
    }

    // Rule 2, opposed by rule 3: one active descendant beside a parked one is
    // active; remove the active one and the same subtree is parked.
    #[test]
    fn rule_2_an_active_descendant_beside_a_parked_one_is_active() {
        let r = rollup("idle", false, [("parked", false), ("active", true)]);
        assert_eq!((r, r.word("idle")), (Rollup::SubtreeActive, "active"));
        // executing alone (a lock held elsewhere) is active
        assert_eq!(rollup("idle", false, [("queued", true)]), Rollup::SubtreeActive);
    }

    #[test]
    fn rule_3_parked_only_is_parked() {
        let r = rollup("idle", false, [("parked", false), ("idle", false)]);
        assert_eq!((r, r.word("idle")), (Rollup::SubtreeParked, "parked"));
        assert!(r.parked("idle") && !r.active());
    }

    #[test]
    fn rule_4_nothing_below_gives_own_state() {
        let r = rollup("queued", false, [("idle", false), ("queued", false)]);
        assert_eq!((r, r.word("queued")), (Rollup::Own, "queued"));
        let r = rollup("idle", false, std::iter::empty());
        assert_eq!((r, r.word("idle")), (Rollup::Own, "idle"));
        assert!(!r.active() && !r.parked("idle"));
    }

    // Three deep, through a roster: p → c → g, where only the grandchild is
    // active and the child is not on the roster at all (it is idle). Both p
    // and c roll up to active; g itself is SelfActive; a sibling root r is
    // untouched.
    #[test]
    fn a_three_deep_subtree_rolls_up_through_an_idle_middle() {
        let parent: BTreeMap<&str, &str> = [("c", "p"), ("g", "c")].into_iter().collect();
        let anc = |id: &str| ancestors(id, |x| parent.get(x).map(|s| s.to_string()));
        assert_eq!(anc("g"), vec!["c".to_string(), "p".to_string()]);
        assert_eq!(anc("p"), Vec::<String>::new());
        let roster: Vec<(Vec<String>, &str, bool)> = vec![(anc("g"), "active", true)];
        let it = || roster.iter().map(|(a, s, e)| (a, *s, *e));
        assert_eq!(rollup_in("p", "idle", false, it()), Rollup::SubtreeActive);
        assert_eq!(rollup_in("c", "idle", false, it()), Rollup::SubtreeActive);
        assert_eq!(rollup_in("g", "active", true, it()), Rollup::SelfActive);
        assert_eq!(rollup_in("r", "idle", false, it()), Rollup::Own);
        // The same tree with the grandchild parked instead: parked at the top.
        let roster: Vec<(Vec<String>, &str, bool)> = vec![(anc("g"), "parked", false)];
        let it = || roster.iter().map(|(a, s, e)| (a, *s, *e));
        assert_eq!(rollup_in("p", "idle", false, it()), Rollup::SubtreeParked);
    }

    #[test]
    fn ancestors_is_cycle_safe() {
        let parent: BTreeMap<&str, &str> = [("a", "b"), ("b", "c"), ("c", "a")].into_iter().collect();
        let got = ancestors("a", |x| parent.get(x).map(|s| s.to_string()));
        assert_eq!(got, vec!["b".to_string(), "c".to_string()]);
    }

    #[test]
    fn ui_a_hides_exactly_threads_with_a_parent() {
        assert!(hidden_from_recents(Some("p")));
        assert!(!hidden_from_recents(None));
        assert!(!hidden_from_recents(Some("")));
    }
}
