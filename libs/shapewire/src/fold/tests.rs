//! # Tests for `fold`
//!
//! **Substrate's `fold.rs` has no tests at all** — 1,299 lines, 0 occurrences
//! of every test-declaring spelling, and `cargo test -p shapewire
//! --no-default-features --features fold --lib -- fold::` reports `0 passed …
//! 85 filtered out` there. So nothing in this file was carried, and the laws the
//! module is built on are asserted here for the first time in either tree.
//!
//! Three disciplines shape the file, and each of them exists because of a way
//! this kind of test suite is known to go green while proving nothing.
//!
//! **1. Every law claim opens with a SENTINEL.** `assert_commutes(a, b)` first
//! asserts `a != b`. A commutativity test over two equal operands is the claim
//! that `==` is reflexive, it passes against any implementation whatsoever, and
//! it reads exactly like coverage. The sentinel is checked before the law, so a
//! future edit that makes the operands agree fails loudly instead of quietly
//! becoming decorative.
//!
//! **2. Every law triple is paired with a SEMANTIC WITNESS, because the three
//! laws cannot tell one lattice from its dual.** `Max` satisfies every law
//! `Min` satisfies and holds the opposite value, so inverting a comparison
//! inside a `join` leaves a law suite entirely green —
//! `the_three_laws_cannot_tell_a_lattice_from_its_dual` asserts that in so many
//! words. Therefore no type's laws stand alone here: each is accompanied by at
//! least one test that pins what that specific lattice MEANS, and it is the pair
//! that has content.
//!
//! **2b. The harness itself is measured before it measures anything** — see
//! `the_law_harness_is_capable_of_failing` and
//! `the_sentinel_refuses_a_law_claim_over_equal_operands`. The first draft of
//! this file claimed an inert `join` satisfied all three laws; the harness came
//! back red, because an empty `join` body is not "nothing happens" but "keep the
//! left operand", which is not commutative. The claim was wrong and the
//! instrument said so, which is the whole reason the instrument gets tested
//! first.
//!
//! **3. The laws are asserted PER TYPE.** The three helpers are shared, the
//! tests are not: every lattice gets its own four named tests, so a report can
//! carry a law table with one row per type and a reader can see which cell is
//! unasserted rather than trusting one generic test to have covered everything.
//!
//! Two more conventions worth knowing before reading:
//!
//! * **`N` is stated in the name** where a claim needs an N — `_at_n2`,
//!   `_at_n3`, `_at_capacity_plus_one`. Associativity at N = 2 is not
//!   associativity, and an eviction test at N = capacity evicts nothing.
//! * **A refusal test is never alone.** Every "it refuses X" has an "it accepts
//!   not-X" beside it, so an implementation that refuses everything fails too.

use super::*;

use std::collections::BTreeSet;
use std::fmt::Debug;

use serde_json::json;

// ─────────────────────────────────────────────────────────────────────────────
// Stamps
// ─────────────────────────────────────────────────────────────────────────────

fn at(s: &str) -> Timestamp {
    s.parse().expect("a test stamp must parse")
}

/// A stamp `minute` minutes into a fixed hour, so an ordering reads as an
/// ordering at the call site.
fn m(minute: u32) -> Timestamp {
    at(&format!("2026-10-12T09:{minute:02}:00Z"))
}

fn mins(n: i64) -> ChronoDuration {
    ChronoDuration::minutes(n)
}

// ─────────────────────────────────────────────────────────────────────────────
// The law harness
//
// Each helper ASSERTS rather than answering a bool. A helper that observes a
// failure and returns it leaves the caller free to ignore it, and a suite built
// on one is green by construction.
// ─────────────────────────────────────────────────────────────────────────────

/// **IDEMPOTENCE** — `join(a, a) == a`. A duplicate delivery of one input must
/// cost nothing, because a reconnect replays and a producer cannot promise it
/// will not.
#[track_caller]
fn assert_idempotent<T: Join + PartialEq + Debug>(a: &T) {
    let mut doubled = a.clone();
    doubled.join(a);
    assert_eq!(
        &doubled, a,
        "IDEMPOTENCE: join(a, a) must be a — a duplicate delivery moved the value"
    );
}

/// **COMMUTATIVITY AT N = 2** — `join(a, b) == join(b, a)`. Two operands is the
/// smallest N at which the claim exists at all.
#[track_caller]
fn assert_commutes<T: Join + PartialEq + Debug>(a: &T, b: &T) {
    assert_ne!(
        a, b,
        "SENTINEL: a commutativity claim over two EQUAL operands proves only that == is reflexive"
    );
    let mut ab = a.clone();
    ab.join(b);
    let mut ba = b.clone();
    ba.join(a);
    assert_eq!(
        ab, ba,
        "COMMUTATIVITY at N=2: join(a, b) must equal join(b, a) — arrival order reached the value"
    );
}

/// **ASSOCIATIVITY AT N = 3** — `join(join(a, b), c) == join(a, join(b, c))`.
/// Three operands is the smallest N at which the claim exists: with two there is
/// only one grouping.
#[track_caller]
fn assert_associates<T: Join + PartialEq + Debug>(a: &T, b: &T, c: &T) {
    assert_ne!(a, b, "SENTINEL: associativity needs three DIFFERENT operands");
    assert_ne!(b, c, "SENTINEL: associativity needs three DIFFERENT operands");
    assert_ne!(a, c, "SENTINEL: associativity needs three DIFFERENT operands");

    let mut left = a.clone();
    left.join(b);
    left.join(c);

    let mut bc = b.clone();
    bc.join(c);
    let mut right = a.clone();
    right.join(&bc);

    assert_eq!(
        left, right,
        "ASSOCIATIVITY at N=3: the grouping reached the value — a mirror that folded two \
         inputs before a third would disagree with one that did not"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// The lattices under test
//
// `Min` is the module's own and the only one substrate's tree instantiates
// (`lib/threads/src/shapes/nonidle.rs:168`, `Semilattice<Min<Timestamp>>`). The
// rest are declared HERE, in the test module, for two reasons: the module doc
// names a max and an or as the other intended shapes, and a trait that only its
// own crate implements has not been shown to be implementable from outside.
// ─────────────────────────────────────────────────────────────────────────────

/// A high-water mark. Last-seen loses, largest wins.
#[derive(Debug, Clone, PartialEq)]
struct Max(f64);

impl Join for Max {
    fn join(&mut self, other: &Self) {
        if other.0 > self.0 {
            self.0 = other.0;
        }
    }
}

/// A latch: once true, true.
#[derive(Debug, Clone, PartialEq)]
struct Latch(bool);

impl Join for Latch {
    fn join(&mut self, other: &Self) {
        self.0 = self.0 || other.0;
    }
}

/// **A grow-only set** — the "set-like" lattice, and the one whose join is a
/// union rather than a choice between two operands.
#[derive(Debug, Clone, PartialEq)]
struct GrowSet(BTreeSet<&'static str>);

impl GrowSet {
    fn of(items: &[&'static str]) -> Self {
        GrowSet(items.iter().copied().collect())
    }
}

impl Join for GrowSet {
    fn join(&mut self, other: &Self) {
        for item in &other.0 {
            self.0.insert(item);
        }
    }
}

/// **A `Join` WITH AN EMPTY BODY.** Not "nothing happens" — KEEP THE LEFT
/// OPERAND, which is idempotent and associative and not commutative. A cell
/// built on it keeps whatever arrived first and silently discards every later
/// contribution. It exists to be asserted about, in
/// `the_law_harness_is_capable_of_failing` and
/// `a_cell_over_an_inert_join_silently_keeps_only_the_first_contribution`.
#[derive(Debug, Clone, PartialEq)]
struct Inert(u64);

impl Join for Inert {
    fn join(&mut self, _other: &Self) {}
}

// ─────────────────────────────────────────────────────────────────────────────
// L1. `Min<Timestamp>` — the one instantiation that exists in either tree
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn min_timestamp_join_is_idempotent() {
    assert_idempotent(&Min(m(5)));
}

#[test]
fn min_timestamp_join_commutes_at_n2() {
    assert_commutes(&Min(m(5)), &Min(m(9)));
}

#[test]
fn min_timestamp_join_associates_at_n3() {
    assert_associates(&Min(m(9)), &Min(m(5)), &Min(m(17)));
}

/// **THE SEMANTIC WITNESS for `Min`**, and the test that an inert join fails.
///
/// All six orderings of three distinct instants must land on the earliest one.
/// A join that does nothing passes every law above and fails this; so does a
/// join that takes the later operand; so does one that overwrites.
#[test]
fn min_timestamp_keeps_the_earliest_of_three_in_all_six_orders() {
    let instants = [m(17), m(5), m(9)];
    let earliest = Min(m(5));

    for first in 0..3 {
        for second in 0..3 {
            for third in 0..3 {
                if first == second || second == third || first == third {
                    continue;
                }
                let mut held = Min(instants[first]);
                held.join(&Min(instants[second]));
                held.join(&Min(instants[third]));
                assert_eq!(
                    held, earliest,
                    "first-seen must win whatever order the three arrive in \
                     (order {first}, {second}, {third})"
                );
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// L2. `Min<u64>` — a second instantiation, so the generic is not proved by one
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn min_u64_join_is_idempotent() {
    assert_idempotent(&Min(7u64));
}

#[test]
fn min_u64_join_commutes_at_n2() {
    assert_commutes(&Min(7u64), &Min(2u64));
}

#[test]
fn min_u64_join_associates_at_n3() {
    assert_associates(&Min(7u64), &Min(2u64), &Min(11u64));
}

#[test]
fn min_u64_keeps_the_smallest_and_never_moves_up() {
    let mut held = Min(7u64);
    held.join(&Min(2u64));
    assert_eq!(held, Min(2), "the smaller operand must win");
    held.join(&Min(11u64));
    assert_eq!(held, Min(2), "a larger operand must not move a Min");
}

// ─────────────────────────────────────────────────────────────────────────────
// L3. A high-water mark
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn max_join_is_idempotent() {
    assert_idempotent(&Max(1.5));
}

#[test]
fn max_join_commutes_at_n2() {
    assert_commutes(&Max(1.5), &Max(4.25));
}

#[test]
fn max_join_associates_at_n3() {
    assert_associates(&Max(1.5), &Max(4.25), &Max(0.5));
}

#[test]
fn max_keeps_the_largest_and_never_moves_down() {
    let mut held = Max(1.5);
    held.join(&Max(4.25));
    assert_eq!(held, Max(4.25));
    held.join(&Max(0.5));
    assert_eq!(held, Max(4.25), "a smaller operand must not move a high-water mark");
}

// ─────────────────────────────────────────────────────────────────────────────
// L4. A latch
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn latch_join_is_idempotent() {
    assert_idempotent(&Latch(true));
    assert_idempotent(&Latch(false));
}

#[test]
fn latch_join_commutes_at_n2() {
    assert_commutes(&Latch(false), &Latch(true));
}

#[test]
fn latch_join_associates_at_n3() {
    // Three DIFFERENT operands are impossible for a two-valued lattice, so the
    // claim is made over the three two-element groupings instead and the helper
    // is deliberately not used — a sentinel that cannot hold must not be
    // pretended through.
    let groupings = [
        [Latch(false), Latch(false), Latch(true)],
        [Latch(false), Latch(true), Latch(false)],
        [Latch(true), Latch(false), Latch(false)],
    ];
    for [a, b, c] in &groupings {
        let mut left = a.clone();
        left.join(b);
        left.join(c);
        let mut bc = b.clone();
        bc.join(c);
        let mut right = a.clone();
        right.join(&bc);
        assert_eq!(left, right, "ASSOCIATIVITY at N=3 over {a:?} {b:?} {c:?}");
        assert_eq!(left, Latch(true), "one true anywhere must latch the cell");
    }
}

#[test]
fn latch_never_unlatches() {
    let mut held = Latch(true);
    held.join(&Latch(false));
    assert_eq!(held, Latch(true), "a false must not clear a latch");
}

// ─────────────────────────────────────────────────────────────────────────────
// L5. A grow-only set — the lattice whose join is a union
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn grow_set_join_is_idempotent() {
    assert_idempotent(&GrowSet::of(&["a", "b"]));
}

#[test]
fn grow_set_join_commutes_at_n2() {
    assert_commutes(&GrowSet::of(&["a"]), &GrowSet::of(&["b"]));
}

#[test]
fn grow_set_join_associates_at_n3() {
    assert_associates(
        &GrowSet::of(&["a"]),
        &GrowSet::of(&["b"]),
        &GrowSet::of(&["c"]),
    );
}

#[test]
fn grow_set_unions_rather_than_choosing_an_operand() {
    let mut held = GrowSet::of(&["a"]);
    held.join(&GrowSet::of(&["b"]));
    assert_eq!(
        held,
        GrowSet::of(&["a", "b"]),
        "a union keeps BOTH operands — a lattice that chose one of them would \
         still pass all three laws"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// L6. The harness measured, before anything is measured with it
//
// Three tests, and the first draft of this section got its own claim wrong,
// which is the argument for having it. The draft asserted that an inert join
// satisfies all three laws; the harness came back red on commutativity, because
// a join with an empty body is not "nothing happens", it is KEEP THE LEFT
// OPERAND — idempotent and associative, and emphatically not commutative. The
// three tests below are what the harness actually does, measured.
// ─────────────────────────────────────────────────────────────────────────────

/// **THE LAW HARNESS IS CAPABLE OF FAILING, AND HERE IS IT FAILING.**
///
/// A search that finds nothing has to be shown capable of finding something, and
/// a law suite is a search for a counter-example. `Inert::join` has an empty
/// body, which makes it keep whichever operand is on the left: idempotence and
/// associativity hold, and commutativity cannot. So the helper that would have
/// to notice does notice.
#[test]
fn the_law_harness_is_capable_of_failing() {
    assert_idempotent(&Inert(1));
    assert_associates(&Inert(1), &Inert(2), &Inert(3));

    let fell = std::panic::catch_unwind(|| assert_commutes(&Inert(1), &Inert(2))).is_err();
    assert!(
        fell,
        "assert_commutes must REFUSE a keep-the-left-operand join; if this passes, \
         every commutativity claim in this file is decorative"
    );
}

/// **THE SENTINEL IS LIVE.** Every law helper opens by asserting its operands
/// differ, and that guard is itself guarded: handed two equal operands, the
/// helper must refuse rather than report a green.
#[test]
fn the_sentinel_refuses_a_law_claim_over_equal_operands() {
    let fell = std::panic::catch_unwind(|| assert_commutes(&Min(m(5)), &Min(m(5)))).is_err();
    assert!(
        fell,
        "a commutativity claim over two EQUAL operands must be refused, not passed — \
         it proves only that == is reflexive"
    );

    let also_fell =
        std::panic::catch_unwind(|| assert_associates(&Min(m(5)), &Min(m(5)), &Min(m(9)))).is_err();
    assert!(also_fell, "and the same for associativity");
}

/// **WHAT THE THREE LAWS CANNOT SEE: A DIFFERENT LATTICE.**
///
/// `Max` is `Min`'s dual. It satisfies every one of the three laws — the harness
/// above asserts exactly that, type by type — and a cell built on it holds the
/// opposite value. So the laws are a floor and not a verdict: they rule out
/// implementations that are not lattices at all, and they cannot rule out the
/// wrong lattice.
///
/// That is why no type's law triple stands alone in this file, and it is also
/// the shape of the forcing that matters most on this module: inverting the
/// comparison inside a `join` leaves every law green, so only the per-type
/// semantic witness can catch it.
#[test]
fn the_three_laws_cannot_tell_a_lattice_from_its_dual() {
    // Both are lawful. (Asserted here as well as in L1 and L3 deliberately: the
    // point of this test is the CONJUNCTION — lawful, and still opposite.)
    assert_idempotent(&Min(2u64));
    assert_commutes(&Min(2u64), &Min(7u64));
    assert_associates(&Min(2u64), &Min(7u64), &Min(11u64));
    assert_idempotent(&Max(2.0));
    assert_commutes(&Max(2.0), &Max(7.0));
    assert_associates(&Max(2.0), &Max(7.0), &Max(11.0));

    let mut keeps_first = Min(7u64);
    keeps_first.join(&Min(2));
    keeps_first.join(&Min(11));
    let mut keeps_highest = Max(7.0);
    keeps_highest.join(&Max(2.0));
    keeps_highest.join(&Max(11.0));

    assert_eq!(keeps_first, Min(2), "the semantic witness for Min");
    assert_eq!(keeps_highest, Max(11.0), "the semantic witness for Max");
    assert_ne!(
        keeps_first.0 as f64, keeps_highest.0,
        "two fully lawful lattices over the same three inputs disagree about \
         everything, and no law above can tell them apart"
    );
}

/// **AND WHAT AN INERT JOIN COSTS A CELL**, which is the thing the laws were
/// being asked to protect against: every contribution after the first is
/// silently discarded.
#[test]
fn a_cell_over_an_inert_join_silently_keeps_only_the_first_contribution() {
    let mut cell: Semilattice<Inert> = Semilattice::new();
    cell.emit(m(1), Inert(1));
    cell.emit(m(2), Inert(2));
    assert_eq!(
        cell.now(),
        Some(&Inert(1)),
        "the second contribution is gone, and the cell state says Known either way"
    );
    assert!(
        cell.state().is_known(),
        "which is the hazard: nothing in the cell's own provenance records the loss"
    );
}


// ─────────────────────────────────────────────────────────────────────────────
// L7. The `Semilattice` CELL — the laws read through the thing a shape holds
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn semilattice_over_min_is_idempotent_under_redelivery() {
    let mut once: Semilattice<Min<Timestamp>> = Semilattice::new();
    once.emit(m(5), Min(m(5)));

    let mut twice: Semilattice<Min<Timestamp>> = Semilattice::new();
    twice.emit(m(5), Min(m(5)));
    twice.emit(m(5), Min(m(5)));

    assert_eq!(
        once.now(),
        twice.now(),
        "IDEMPOTENCE through the cell: a duplicate delivery must not move the held value"
    );
}

#[test]
fn semilattice_over_min_commutes_at_n2() {
    let (early, late) = (Min(m(5)), Min(m(9)));
    assert_ne!(early, late, "SENTINEL: the two inputs must differ");

    let mut forwards: Semilattice<Min<Timestamp>> = Semilattice::new();
    forwards.emit(m(5), early.clone());
    forwards.emit(m(9), late.clone());

    let mut backwards: Semilattice<Min<Timestamp>> = Semilattice::new();
    backwards.emit(m(9), late);
    backwards.emit(m(5), early);

    assert_eq!(
        forwards.now(),
        backwards.now(),
        "COMMUTATIVITY at N=2 through the cell"
    );
}

#[test]
fn semilattice_over_min_converges_at_n3_in_all_six_orders() {
    let inputs = [m(17), m(5), m(9)];
    for first in 0..3 {
        for second in 0..3 {
            for third in 0..3 {
                if first == second || second == third || first == third {
                    continue;
                }
                let mut cell: Semilattice<Min<Timestamp>> = Semilattice::new();
                cell.emit(inputs[first], Min(inputs[first]));
                cell.emit(inputs[second], Min(inputs[second]));
                cell.emit(inputs[third], Min(inputs[third]));
                assert_eq!(
                    cell.now(),
                    Some(&Min(m(5))),
                    "a replay, a duplicate and a re-hydrate must converge \
                     (order {first}, {second}, {third})"
                );
            }
        }
    }
}

/// **`read` IS A SET AND `emit` IS A JOIN**, and the difference is visible only
/// in a case where the two would disagree — a read that moves the value the
/// WRONG way for the lattice. A `read` implemented as a join could not do this
/// and would hold `m(5)`.
#[test]
fn semilattice_read_replaces_where_emit_would_have_joined() {
    let mut cell: Semilattice<Min<Timestamp>> = Semilattice::new();
    cell.emit(m(5), Min(m(5)));
    assert_eq!(cell.now(), Some(&Min(m(5))), "the live value is held first");

    cell.read(m(20), Min(m(9)));
    assert_eq!(
        cell.now(),
        Some(&Min(m(9))),
        "the read is the authority: it REPLACES, even moving a Min forward"
    );
}

#[test]
fn semilattice_take_dirty_reports_once_and_then_clears() {
    let mut cell: Semilattice<Min<Timestamp>> = Semilattice::new();
    assert!(!cell.take_dirty(), "a cell nothing happened in is not dirty");
    cell.emit(m(5), Min(m(5)));
    assert!(cell.take_dirty(), "an emit dirties the cell");
    assert!(
        !cell.take_dirty(),
        "and the flag is TAKEN: a second drain must not re-publish the same value"
    );
}

#[test]
fn semilattice_mark_dead_replaces_a_known_state_and_carries_the_reason() {
    let mut cell: Semilattice<Min<Timestamp>> = Semilattice::new();
    cell.emit(m(5), Min(m(5)));
    assert!(
        cell.state().is_known(),
        "POSITIVE ARM: the cell must be Known before it is killed, or a constant \
         Dead would pass this test"
    );

    cell.mark_dead("the shape task panicked");
    assert_eq!(
        cell.state(),
        &CellState::Dead {
            reason: "the shape task panicked".to_string()
        },
        "dead, with the reason it was given"
    );
    assert_eq!(
        cell.now(),
        Some(&Min(m(5))),
        "the last value it managed to compute is still readable — Dead is a \
         statement about the fold, not an erasure"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// L8. `Counter` — the cell that is deliberately NOT idempotent
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn counter_add_is_deliberately_not_idempotent() {
    let mut once = Counter::new();
    once.add(m(1), 1);

    let mut twice = Counter::new();
    twice.add(m(1), 1);
    twice.add(m(1), 1);

    assert_ne!(
        once.now(),
        twice.now(),
        "a counter ACCUMULATES: two deliveries of one event are two increments, \
         which is why it is the only cell here that cannot be re-derived by replay"
    );
    assert_eq!(twice.now(), 2);
}

#[test]
fn counter_add_commutes_at_n2() {
    let mut forwards = Counter::new();
    forwards.add(m(1), 2);
    forwards.add(m(2), 5);

    let mut backwards = Counter::new();
    backwards.add(m(1), 5);
    backwards.add(m(2), 2);

    assert_eq!(forwards.now(), backwards.now(), "COMMUTATIVITY at N=2");
    assert_eq!(forwards.now(), 7);
}

#[test]
fn counter_add_associates_at_n3() {
    let grouped_left = {
        let mut c = Counter::new();
        c.add(m(1), 2);
        c.add(m(2), 5);
        let subtotal = c.now();
        let mut d = Counter::new();
        d.add(m(3), subtotal);
        d.add(m(4), 11);
        d.now()
    };
    let grouped_right = {
        let mut c = Counter::new();
        c.add(m(1), 5);
        c.add(m(2), 11);
        let subtotal = c.now();
        let mut d = Counter::new();
        d.add(m(3), 2);
        d.add(m(4), subtotal);
        d.now()
    };
    assert_eq!(
        grouped_left, grouped_right,
        "ASSOCIATIVITY at N=3: pre-aggregating two of three inputs must not change the total"
    );
    assert_eq!(grouped_left, 18);
}

#[test]
fn counter_is_since_init_by_construction() {
    let mut c = Counter::new();
    assert_eq!(
        c.state(),
        &CellState::NotHydrated { reason: None },
        "POSITIVE ARM: before any add it is not Known at all"
    );
    c.add(m(3), 4);
    assert_eq!(
        c.state(),
        &CellState::Known {
            complete: true,
            folded_at: m(3),
            since_init: true,
            covers_since: None,
        },
        "a cell with NO hydrate entry can only ever be since_init, and the export must say so"
    );
}

#[test]
fn counter_take_dirty_reports_once_and_then_clears() {
    let mut c = Counter::new();
    assert!(!c.take_dirty());
    c.add(m(1), 1);
    assert!(c.take_dirty());
    assert!(!c.take_dirty(), "the flag is TAKEN, not read");
}

// ─────────────────────────────────────────────────────────────────────────────
// R. `Register` — last writer wins, and the ordered variant that does not
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn register_emit_sets_the_value_and_the_stamp() {
    let mut cell: Register<&str> = Register::new();
    assert_eq!(cell.now(), None, "POSITIVE ARM: nothing is held to begin with");

    cell.emit(m(5), "busy");
    assert_eq!(cell.now(), Some(&"busy"));
    assert_eq!(
        cell.state(),
        &CellState::Known {
            complete: true,
            folded_at: m(5),
            since_init: false,
            covers_since: None,
        }
    );
}

#[test]
fn register_emit_is_last_writer_wins_by_arrival() {
    let mut cell: Register<&str> = Register::new();
    cell.emit(m(5), "idle");
    cell.emit(m(9), "busy");
    assert_eq!(cell.now(), Some(&"busy"), "the later arrival wins");

    // And the arrival stamp is what decides, not the value: an input stamped
    // EARLIER still wins if it arrives later. This cell has no ordering key.
    cell.emit(m(1), "idle");
    assert_eq!(
        cell.now(),
        Some(&"idle"),
        "a plain Register orders by ARRIVAL — this is exactly what emit_ordered exists to fix"
    );
    assert_eq!(cell.state().folded_at(), Some(m(1)));
}

#[test]
fn register_emit_ordered_discards_an_observation_that_is_not_newer() {
    let mut gauge: Register<u64> = Register::new();
    gauge.emit_ordered(m(10), m(2), 20);
    assert_eq!(gauge.now(), Some(&20), "POSITIVE ARM: the first reading lands");

    // The late-arriving older reading: observed at minute 1, delivered now.
    gauge.emit_ordered(m(11), m(1), 99);
    assert_eq!(
        gauge.now(),
        Some(&20),
        "a reading whose ordering key is older must be DISCARDED, not applied"
    );
    assert_eq!(
        gauge.state().folded_at(),
        Some(m(10)),
        "and the discard is total: the cell did not even advance its stamp"
    );
}

#[test]
fn register_emit_ordered_accepts_an_observation_that_is_newer() {
    let mut gauge: Register<u64> = Register::new();
    gauge.emit_ordered(m(10), m(2), 20);
    gauge.emit_ordered(m(11), m(3), 99);
    assert_eq!(
        gauge.now(),
        Some(&99),
        "OPPOSING ARM: a genuinely newer observation must be applied, so an \
         implementation that discards everything fails"
    );
}

#[test]
fn register_emit_ordered_refuses_a_repeat_of_the_held_key() {
    let mut gauge: Register<&str> = Register::new();
    gauge.emit_ordered(m(10), m(2), "first");
    gauge.emit_ordered(m(11), m(2), "second");
    assert_eq!(
        gauge.now(),
        Some(&"first"),
        "IDEMPOTENCE under re-delivery: the same ordering key twice is not news, \
         so the held value stands"
    );
}

#[test]
fn register_emit_ordered_takes_the_first_value_whatever_its_key_says() {
    // The discard needs BOTH halves of its condition: an un-Known cell has
    // nothing to prefer over the arriving value, however old its key.
    let mut gauge: Register<u64> = Register::new();
    gauge.not_hydrated("the gauge read failed");
    gauge.emit_ordered(m(10), at("2000-01-01T00:00:00Z"), 7);
    assert_eq!(
        gauge.now(),
        Some(&7),
        "a cell that is not Known must take the value rather than refuse it in \
         favour of nothing"
    );
}

#[test]
fn register_emit_ordered_converges_at_n2_whatever_the_delivery_order() {
    let mut forwards: Register<u64> = Register::new();
    forwards.emit_ordered(m(10), m(2), 20);
    forwards.emit_ordered(m(11), m(3), 30);

    let mut backwards: Register<u64> = Register::new();
    backwards.emit_ordered(m(10), m(3), 30);
    backwards.emit_ordered(m(11), m(2), 20);

    assert_eq!(
        forwards.now(),
        backwards.now(),
        "COMMUTATIVITY at N=2 for the ordered variant: the delivery order of two \
         observations must not reach the value"
    );
    assert_eq!(forwards.now(), Some(&30));
}

#[test]
fn register_emit_ordered_converges_at_n3_in_all_six_orders() {
    let observations = [(m(2), 20u64), (m(3), 30), (m(1), 10)];
    for first in 0..3 {
        for second in 0..3 {
            for third in 0..3 {
                if first == second || second == third || first == third {
                    continue;
                }
                let mut gauge: Register<u64> = Register::new();
                for (nth, which) in [first, second, third].into_iter().enumerate() {
                    let (order, value) = observations[which];
                    gauge.emit_ordered(m(20 + nth as u32), order, value);
                }
                assert_eq!(
                    gauge.now(),
                    Some(&30),
                    "ASSOCIATIVITY at N=3: three observations delivered in any order \
                     converge on the newest ({first}, {second}, {third})"
                );
            }
        }
    }
}

#[test]
fn register_read_partial_says_the_picture_behind_the_value_is_short() {
    let mut whole: Register<&str> = Register::new();
    whole.read(m(5), "value");
    let mut short: Register<&str> = Register::new();
    short.read_partial(m(5), "value");

    assert_eq!(whole.now(), short.now(), "SENTINEL: the VALUES agree …");
    assert_ne!(
        whole.state(),
        short.state(),
        "… so the only thing distinguishing a partial read is the cell state, and \
         it must actually differ"
    );
    assert_eq!(
        short.state(),
        &CellState::Known {
            complete: false,
            folded_at: m(5),
            since_init: false,
            covers_since: None,
        }
    );
}

#[test]
fn register_not_hydrated_carries_a_reason_and_is_not_merely_empty() {
    let fresh: Register<&str> = Register::new();
    assert_eq!(
        fresh.state(),
        &CellState::NotHydrated { reason: None },
        "POSITIVE ARM: an untouched cell is NotHydrated with NO reason"
    );

    let mut failed: Register<&str> = Register::new();
    failed.not_hydrated("the store errored");
    assert_eq!(
        failed.state(),
        &CellState::NotHydrated {
            reason: Some("the store errored".to_string())
        },
        "a hydrate that FAILED is distinguishable from one that never ran, by the reason"
    );
    assert_ne!(
        fresh.state(),
        failed.state(),
        "and the two states must not compare equal, or the distinction is cosmetic"
    );
}

#[test]
fn register_mark_dead_replaces_a_known_state_and_keeps_the_last_value() {
    let mut cell: Register<&str> = Register::new();
    cell.emit(m(5), "busy");
    assert!(cell.state().is_known(), "POSITIVE ARM: Known before the kill");

    cell.mark_dead("the shape task panicked");
    assert_eq!(
        cell.state(),
        &CellState::Dead {
            reason: "the shape task panicked".to_string()
        }
    );
    assert_eq!(
        cell.now(),
        Some(&"busy"),
        "last-known, not erased — a consumer must be able to render the stale value"
    );
}

#[test]
fn register_take_dirty_reports_once_and_then_clears() {
    let mut cell: Register<&str> = Register::new();
    assert!(!cell.take_dirty(), "POSITIVE ARM: an untouched cell is clean");
    cell.emit(m(5), "busy");
    assert!(cell.take_dirty(), "an emit dirties it");
    assert!(!cell.take_dirty(), "and the flag is TAKEN rather than read");

    for (label, mutate) in [
        ("read", 1u8),
        ("read_partial", 2),
        ("not_hydrated", 3),
    ] {
        let mut c: Register<&str> = Register::new();
        match mutate {
            1 => c.read(m(5), "v"),
            2 => c.read_partial(m(5), "v"),
            _ => c.not_hydrated("why"),
        }
        assert!(c.take_dirty(), "{label} must dirty the cell too");
        assert!(!c.take_dirty(), "{label} must then be clear");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// M. `Map` — two sets, two state words, and the membership events
// ─────────────────────────────────────────────────────────────────────────────

type Rows = Map<String, Register<u64>>;

fn k(s: &str) -> String {
    s.to_string()
}

#[test]
fn map_key_mints_a_row_and_makes_the_key_a_member() {
    let mut rows: Rows = Map::new();
    assert!(!rows.is_member(&k("a")), "POSITIVE ARM: not a member yet");
    assert!(!rows.contains(&k("a")), "POSITIVE ARM: no row yet");

    rows.key(k("a")).emit(m(5), 1);

    assert!(rows.contains(&k("a")), "the row exists");
    assert!(
        rows.is_member(&k("a")),
        "HOLDING IMPLIES MEMBERSHIP — a root with a row for a key is asserting \
         that key exists, and there is no second call to forget"
    );
    assert_eq!(rows.take_added(), vec![k("a")], "and the key joined the pool");
}

#[test]
fn map_member_records_existence_without_minting_a_row() {
    let mut rows: Rows = Map::new();
    rows.member(k("a"));

    assert!(rows.is_member(&k("a")), "the key exists …");
    assert!(
        !rows.contains(&k("a")),
        "… and NO row was minted, which is the whole of the PerKey mechanism"
    );
    assert_eq!(rows.len(), 0, "the held set is still empty");
    assert_eq!(rows.members_len(), 1, "the membership set is not");
    assert_eq!(rows.take_added(), vec![k("a")]);
    assert_eq!(
        rows.take_dirty_keys().len(),
        0,
        "and nothing was dirtied: a membership statement is not a row frame"
    );
}

#[test]
fn map_added_fires_once_per_key_however_often_it_is_restated_at_n3() {
    let mut rows: Rows = Map::new();
    rows.member(k("a"));
    rows.member(k("a"));
    rows.member(k("a"));
    rows.key(k("a"));

    assert_eq!(
        rows.take_added(),
        vec![k("a")],
        "IDEMPOTENCE of a membership statement: three restatements and a row mint \
         are one `added`. A ledger folding 50 events a second would otherwise \
         publish 50 added frames a second for keys everybody already holds"
    );
}

#[test]
fn map_membership_insert_commutes_at_n2_and_associates_at_n3() {
    let built = |order: [&str; 3]| -> Vec<String> {
        let mut rows: Rows = Map::new();
        for id in order {
            rows.member(k(id));
        }
        rows.members().cloned().collect()
    };

    let canonical = built(["a", "b", "c"]);
    assert_eq!(canonical, vec![k("a"), k("b"), k("c")], "key order, always");

    for order in [
        ["a", "c", "b"],
        ["b", "a", "c"],
        ["b", "c", "a"],
        ["c", "a", "b"],
        ["c", "b", "a"],
    ] {
        assert_eq!(
            built(order),
            canonical,
            "the membership set is order-insensitive at N=3: {order:?}"
        );
    }
}

#[test]
fn map_is_member_and_contains_answer_two_different_questions() {
    let mut rows: Rows = Map::new();
    rows.member(k("exists-only"));
    rows.key(k("held")).emit(m(5), 1);

    assert_ne!(
        rows.is_member(&k("exists-only")),
        rows.contains(&k("exists-only")),
        "SENTINEL: the two predicates must genuinely disagree about SOME key, or \
         this test is comparing a thing with itself"
    );
    assert!(rows.is_member(&k("exists-only")) && !rows.contains(&k("exists-only")));
    assert!(rows.is_member(&k("held")) && rows.contains(&k("held")));
    assert!(!rows.is_member(&k("never")) && !rows.contains(&k("never")));
}

#[test]
fn map_remove_drops_the_row_and_the_membership_and_yields_exactly_one_gone() {
    let mut rows: Rows = Map::new();
    rows.key(k("a")).emit(m(5), 1);
    rows.take_added();
    rows.take_dirty_keys();

    assert!(rows.remove(&k("a")).is_some(), "the row comes back out");
    assert!(!rows.contains(&k("a")), "the row is gone");
    assert!(!rows.is_member(&k("a")), "and so is the membership");
    assert_eq!(
        rows.take_removed(),
        vec![k("a")],
        "EXACTLY ONE gone, for a key that was both held and a member"
    );
    assert_eq!(
        rows.take_dirty_keys().len(),
        0,
        "and the key is no longer dirty: a removed key must not also publish a row"
    );
}

#[test]
fn map_remove_of_a_member_with_no_row_still_yields_exactly_one_gone() {
    let mut rows: Rows = Map::new();
    rows.member(k("a"));
    rows.take_added();

    assert!(rows.remove(&k("a")).is_none(), "there was no row to return");
    assert_eq!(
        rows.take_removed(),
        vec![k("a")],
        "a PerKey key can exist without being held, and it can still LEAVE — \
         one gone, not zero and not two"
    );
}

#[test]
fn map_remove_of_a_key_that_was_never_there_yields_nothing() {
    let mut rows: Rows = Map::new();
    rows.key(k("a"));
    rows.take_added();

    assert!(rows.remove(&k("never")).is_none());
    assert!(
        rows.take_removed().is_empty(),
        "OPPOSING ARM: nothing happened, so nothing is published — an \
         implementation that always pushes a gone fails here"
    );
}

#[test]
fn map_remove_retracts_an_added_that_has_not_been_drained_yet() {
    let mut rows: Rows = Map::new();
    rows.member(k("a"));
    rows.member(k("b"));
    rows.remove(&k("a"));

    assert_eq!(
        rows.take_added(),
        vec![k("b")],
        "a key that arrived and left inside one Tx was never news, so its added \
         is retracted rather than published beside its own gone"
    );
    assert_eq!(rows.take_removed(), vec![k("a")]);
}

#[test]
fn map_get_and_get_mut_reach_a_key_without_dirtying_it() {
    let mut rows: Rows = Map::new();
    rows.key(k("a")).emit(m(5), 1);
    rows.take_dirty_keys();

    assert!(rows.get(&k("a")).is_some());
    assert!(rows.get_mut(&k("a")).is_some());
    assert!(
        rows.take_dirty_keys().is_empty(),
        "the drain path collects what a fold already published; reading through \
         it must not publish anything itself"
    );

    rows.key(k("a"));
    assert_eq!(
        rows.take_dirty_keys().into_iter().collect::<Vec<_>>(),
        vec![k("a")],
        "OPPOSING ARM: key() DOES dirty, so an implementation that never dirties fails"
    );
}

#[test]
fn map_rows_and_membership_carry_independent_states() {
    // The PerKey case the second state word exists for: zero rows held
    // COMPLETELY, beside a membership the producer cannot enumerate at all.
    let mut rows: Rows = Map::new();
    rows.mark_read(m(5), true);
    rows.mark_members_not_hydrated("the ledger cannot enumerate its namespace");

    assert_ne!(
        rows.state(),
        rows.members_state(),
        "SENTINEL: one state word could not say both things, so the two must \
         actually be able to disagree"
    );
    assert_eq!(
        rows.state(),
        &CellState::Known {
            complete: true,
            folded_at: m(5),
            since_init: false,
            covers_since: None,
        },
        "the ROW set is complete: zero rows, known"
    );
    assert_eq!(
        rows.members_state(),
        &CellState::NotHydrated {
            reason: Some("the ledger cannot enumerate its namespace".to_string())
        }
    );
}

#[test]
fn map_mark_members_read_records_a_short_read_as_short() {
    let mut whole: Rows = Map::new();
    whole.mark_members_read(m(5), true);
    let mut short: Rows = Map::new();
    short.mark_members_read(m(5), false);

    assert_ne!(
        whole.members_state(),
        short.members_state(),
        "a roster read that hit its own limit must be distinguishable from one \
         that did not, or a consumer counting the roster is counting a floor it \
         cannot see"
    );
}

#[test]
fn map_mark_since_init_is_whole_and_since_init() {
    let mut rows: Rows = Map::new();
    rows.mark_since_init(m(5));
    assert_eq!(
        rows.state(),
        &CellState::Known {
            complete: true,
            folded_at: m(5),
            since_init: true,
            covers_since: None,
        }
    );
}

#[test]
fn map_mark_dead_kills_both_state_words() {
    let mut rows: Rows = Map::new();
    rows.key(k("a")).emit(m(5), 1);
    rows.mark_read(m(5), true);
    rows.mark_members_read(m(5), true);
    assert!(
        rows.state().is_known() && rows.members_state().is_known(),
        "POSITIVE ARM: both are Known before the kill, so a constant Dead fails this test"
    );

    rows.mark_dead("the fold is gone");

    assert_eq!(
        rows.state(),
        &CellState::Dead {
            reason: "the fold is gone".to_string()
        },
        "the ROW set is dead"
    );
    assert_eq!(
        rows.members_state(),
        &CellState::Dead {
            reason: "the fold is gone".to_string()
        },
        "AND THE MEMBERSHIP IS DEAD TOO — the task that was maintaining the \
         roster is the task that died, so a roster read off this root afterwards \
         is history and has to say so"
    );
    assert!(
        rows.contains(&k("a")),
        "the rows themselves are last-known, not erased"
    );
}

#[test]
fn map_take_added_take_removed_and_take_dirty_keys_all_clear() {
    let mut rows: Rows = Map::new();
    rows.key(k("a"));
    rows.member(k("b"));
    rows.remove(&k("a"));

    assert!(!rows.take_added().is_empty(), "there was something to drain");
    assert!(
        rows.take_added().is_empty(),
        "and a SECOND drain is empty — added is taken, not read"
    );
    assert!(!rows.take_removed().is_empty());
    assert!(rows.take_removed().is_empty(), "removed is taken too");

    rows.key(k("c"));
    assert!(!rows.take_dirty_keys().is_empty());
    assert!(rows.take_dirty_keys().is_empty(), "and so are the dirty keys");
}

#[test]
fn map_entries_mut_reaches_every_held_row() {
    let mut rows: Rows = Map::new();
    rows.key(k("a")).emit(m(1), 1);
    rows.key(k("b")).emit(m(1), 2);
    rows.member(k("c"));

    let mut seen: Vec<String> = Vec::new();
    for (id, cell) in rows.entries_mut() {
        seen.push(id.clone());
        cell.mark_dead("sweeping");
    }
    assert_eq!(
        seen,
        vec![k("a"), k("b")],
        "entries_mut walks the HELD rows in key order and does not invent one \
         for the member that has none"
    );
    assert!(matches!(
        rows.get(&k("a")).map(Register::state),
        Some(CellState::Dead { .. })
    ));
}

// ─────────────────────────────────────────────────────────────────────────────
// S. `Sequence` — a log, bounded twice, under the stamp rule
// ─────────────────────────────────────────────────────────────────────────────

/// A dual-bounded log: four items, one hour.
fn log4() -> Sequence<u64> {
    Sequence::new(4, mins(60))
}

fn held(seq: &Sequence<u64>) -> Vec<u64> {
    seq.now().copied().collect()
}

#[test]
fn sequence_emit_appends_and_leaves_the_item_pending() {
    let mut log = log4();
    assert!(log.is_empty(), "POSITIVE ARM: empty to begin with");

    assert!(log.emit(m(5), 1), "an accepted append answers true");
    assert_eq!(held(&log), vec![1]);
    assert_eq!(log.len(), 1);
    assert_eq!(
        log.take_pending().len(),
        1,
        "and the appended item is pending, to become a transition event at Tx close"
    );
}

#[test]
fn sequence_emit_refuses_an_input_at_or_before_the_hydration_edge() {
    let mut log = log4();
    log.read(m(10), vec![(m(9), 1)], true);

    assert!(
        !log.emit(m(10), 2),
        "an input stamped EXACTLY AT folded_at is one the read already saw"
    );
    assert!(
        !log.emit(m(9), 3),
        "and so is one stamped before it"
    );
    assert_eq!(
        held(&log),
        vec![1],
        "neither appended anything: this is the guard that stops a cell \
         fabricating entries out of events that were in flight when the read ran"
    );
    assert!(
        log.take_pending().is_empty(),
        "and nothing was published either"
    );
}

#[test]
fn sequence_emit_accepts_an_input_strictly_after_the_edge() {
    let mut log = log4();
    log.read(m(10), vec![(m(9), 1)], true);

    assert!(
        log.emit(m(11), 2),
        "OPPOSING ARM: strictly after the edge is live, so an implementation that \
         refuses everything fails"
    );
    assert_eq!(held(&log), vec![1, 2]);
    assert_eq!(log.take_pending().len(), 1);
}

#[test]
fn sequence_append_is_the_documented_way_past_the_stamp_rule() {
    let mut refused = log4();
    refused.read(m(10), vec![(m(9), 1)], true);
    let mut appended = log4();
    appended.read(m(10), vec![(m(9), 1)], true);

    assert!(!refused.emit(m(10), 2), "emit refuses it …");
    assert!(
        appended.append(m(10), 2),
        "… and append takes it, which is the whole difference between the two"
    );
    assert_ne!(
        held(&refused),
        held(&appended),
        "SENTINEL: the two cells must actually diverge, or this test is comparing \
         one behaviour with itself"
    );
    assert_eq!(held(&appended), vec![1, 2]);
}

#[test]
fn sequence_evicts_the_oldest_at_capacity_plus_one() {
    let mut log = log4();
    for n in 1..=4 {
        log.emit(m(n), u64::from(n));
    }
    assert_eq!(
        held(&log),
        vec![1, 2, 3, 4],
        "POSITIVE ARM: at exactly capacity nothing is evicted — an eviction test \
         at N = capacity would pass against a cell that evicts nothing"
    );
    assert_eq!(log.state().folded_at(), Some(m(4)));

    log.emit(m(5), 5);
    assert_eq!(
        held(&log),
        vec![2, 3, 4, 5],
        "the capacity + 1-th item evicts the oldest, off the FRONT"
    );
    assert_eq!(log.len(), 4, "and the length does not grow past capacity");
}

#[test]
fn sequence_evicts_by_window_and_covers_since_follows_it() {
    // A one-hour window. Items at 09:01 and 09:02, then one an hour and a half
    // later: the first two are outside the window the third defines.
    let mut log = log4();
    log.emit(m(1), 1);
    log.emit(m(2), 2);
    assert_eq!(
        log.state(),
        &CellState::Known {
            complete: true,
            folded_at: m(2),
            since_init: true,
            covers_since: Some(m(1)),
        },
        "POSITIVE ARM: covers_since is the oldest item held, before any sweep"
    );

    log.emit(at("2026-10-12T10:31:00Z"), 3);
    assert_eq!(
        held(&log),
        vec![3],
        "the clock sweep took both items that fell out of the one-hour window"
    );
    assert_eq!(
        log.state(),
        &CellState::Known {
            complete: true,
            folded_at: at("2026-10-12T10:31:00Z"),
            since_init: true,
            covers_since: Some(at("2026-10-12T10:31:00Z")),
        },
        "and covers_since MOVED, which is what qualifies any count a consumer \
         derives from this cell"
    );
}

/// **A COUNT-ONLY LOG OUTLASTS EVERY WINDOW, AND THE PERIOD IS NAMED: 24 HOURS.**
///
/// The two cells are fed the same two inputs 24 hours apart. The dual-bounded
/// one empties itself, which is the correct answer to *"the failovers of the
/// last hour"*; the count-only one still holds the last thing that was said,
/// which is the correct answer to *"the transcript I have open"*. Asserting both
/// arms is the point — a test of the count-only cell alone would pass against a
/// cell that never evicts anything at all.
#[test]
fn a_count_only_sequence_outlasts_a_24h_quiet_period_and_a_dual_bounded_one_does_not() {
    let yesterday = at("2026-10-11T09:00:00Z");
    let today = at("2026-10-12T09:00:00Z");

    let mut dual = Sequence::new(4, mins(60));
    dual.emit(yesterday, 1);
    dual.emit(today, 2);
    assert_eq!(
        held(&dual),
        vec![2],
        "24 h is well past the one-hour window, so yesterday's item is gone"
    );

    let mut count_only: Sequence<u64> = Sequence::new_capacity_only(4);
    count_only.emit(yesterday, 1);
    count_only.emit(today, 2);
    assert_eq!(
        held(&count_only),
        vec![1, 2],
        "a cell with NO clock keeps it — and `None` is the absence stated once, \
         rather than an enormous Duration whose `now - window` is not a \
         representable instant"
    );
}

#[test]
fn sequence_read_keeps_the_newest_capacity_and_says_the_window_is_short() {
    let mut log = log4();
    log.read(
        m(20),
        vec![(m(1), 1), (m(2), 2), (m(3), 3), (m(4), 4), (m(5), 5), (m(6), 6)],
        true,
    );

    assert_eq!(
        held(&log),
        vec![3, 4, 5, 6],
        "six items into a four-item cell keeps the RECENT end and drops the front"
    );
    assert_eq!(
        log.state(),
        &CellState::Known {
            complete: false,
            folded_at: m(20),
            since_init: false,
            covers_since: Some(m(3)),
        },
        "and the drop is stated twice, structurally: complete goes false WHATEVER \
         the producer claimed, and covers_since moves to what is actually held"
    );
}

#[test]
fn sequence_read_that_fits_carries_the_producers_own_complete_claim() {
    let mut whole = log4();
    whole.read(m(20), vec![(m(1), 1), (m(2), 2)], true);
    let mut short = log4();
    short.read(m(20), vec![(m(1), 1), (m(2), 2)], false);

    assert_eq!(
        whole.state(),
        &CellState::Known {
            complete: true,
            folded_at: m(20),
            since_init: false,
            covers_since: Some(m(1)),
        },
        "OPPOSING ARM: a read that fits is complete if the producer said so — an \
         implementation that always answers `complete: false` fails here"
    );
    assert_ne!(
        whole.state(),
        short.state(),
        "and a producer that admits a short read is believed, so the two differ"
    );
}

#[test]
fn sequence_read_appends_nothing_to_pending() {
    let mut log = log4();
    log.read(m(20), vec![(m(1), 1), (m(2), 2), (m(3), 3)], true);
    assert!(
        log.take_pending().is_empty(),
        "A HYDRATE PUBLISHES NOTHING. A read that filled `pending` would hand \
         every already-subscribed consumer N transition frames for items it \
         already holds — the phantom transition at the scale of a whole window"
    );

    log.emit(m(21), 4);
    assert_eq!(
        log.take_pending().len(),
        1,
        "OPPOSING ARM: a LIVE append does publish, so a cell that never pends fails"
    );
}

#[test]
fn sequence_read_does_not_evict_by_time_even_on_a_dual_bounded_cell() {
    let mut log = log4();
    log.read(
        at("2026-10-12T09:00:00Z"),
        vec![
            (at("2020-01-01T00:00:00Z"), 1),
            (at("2021-01-01T00:00:00Z"), 2),
        ],
        true,
    );
    assert_eq!(
        held(&log),
        vec![1, 2],
        "these came out of a durable log, not off the wire: a clock sweep here \
         would silently delete the answer to 'what was the last thing said' for \
         every subject quiet longer than the window"
    );
}

#[test]
fn sequence_a_live_append_carries_both_flags_rather_than_recomputing_them() {
    // The case that hard-coding `since_init: true` would have erased: a cell
    // that WAS hydrated, short, and then has one live append.
    let mut log = log4();
    log.read(m(10), vec![(m(1), 1), (m(2), 2)], false);
    log.emit(m(11), 3);

    assert_eq!(
        log.state(),
        &CellState::Known {
            complete: false,
            folded_at: m(11),
            since_init: false,
            covers_since: Some(m(1)),
        },
        "`complete: false` is CARRIED — an append tells you nothing about whether \
         the read behind it was whole — and `since_init` stays false, because a \
         cell that was hydrated does not stop being hydrated when something \
         happens in it"
    );
}

#[test]
fn sequence_an_append_to_a_never_hydrated_cell_is_since_init() {
    let mut log = log4();
    log.emit(m(5), 1);
    assert_eq!(
        log.state(),
        &CellState::Known {
            complete: true,
            folded_at: m(5),
            since_init: true,
            covers_since: Some(m(5)),
        },
        "OPPOSING ARM for the carry rule: with no read behind it the cell covers \
         only what it witnessed, and the export says so"
    );
}

#[test]
fn sequence_mark_since_init_covers_from_the_instant_it_is_given() {
    let mut log = log4();
    log.mark_since_init(m(7));
    assert_eq!(
        log.state(),
        &CellState::Known {
            complete: true,
            folded_at: m(7),
            since_init: true,
            covers_since: Some(m(7)),
        }
    );
}

#[test]
fn sequence_take_pending_reports_once_and_then_clears() {
    let mut log = log4();
    log.emit(m(1), 1);
    log.emit(m(2), 2);
    assert_eq!(log.take_pending().len(), 2);
    assert!(
        log.take_pending().is_empty(),
        "pending is TAKEN: a second drain inside one Tx must not re-publish the \
         same two transitions"
    );
}

#[test]
fn sequence_newest_is_the_head_a_monotone_key_fold_compares_against() {
    let mut log = log4();
    assert_eq!(log.newest(), None, "POSITIVE ARM: nothing to compare against yet");
    log.append(m(1), 10);
    log.append(m(2), 20);
    assert_eq!(
        log.newest(),
        Some(&20),
        "the head, which is what the caller of append owes its own guard against"
    );
}

#[test]
fn sequence_to_json_retained_takes_the_newest_n_in_oldest_first_order() {
    let mut log = log4();
    for n in 1..=4 {
        log.append(m(n), u64::from(n));
    }

    assert_eq!(
        log.to_json(),
        json!([1, 2, 3, 4]),
        "Retain::Max is everything retained, oldest first"
    );
    assert_eq!(
        log.to_json_retained(crate::wire::Retain::Count(2)),
        json!([3, 4]),
        "the newest two, still oldest-first — shorter at the FRONT, so a consumer \
         that asked for fewer items reads the same array shape"
    );
    assert_eq!(
        log.to_json_retained(crate::wire::Retain::Count(0)),
        json!([]),
        "zero is a real ask: the row, without its log"
    );
    assert_eq!(
        log.to_json_retained(crate::wire::Retain::Count(99)),
        json!([1, 2, 3, 4]),
        "and an ask larger than the window is the world being small, not a refusal"
    );
}

#[test]
fn sequence_to_json_retained_evicts_nothing() {
    let mut log = log4();
    for n in 1..=4 {
        log.append(m(n), u64::from(n));
    }
    let _ = log.to_json_retained(crate::wire::Retain::Count(1));
    assert_eq!(
        held(&log),
        vec![1, 2, 3, 4],
        "what ONE subscriber asked for must not change what the cell holds — \
         every other subscriber's frame is built from the same untouched items"
    );
    assert_eq!(
        log.to_json_retained(crate::wire::Retain::Max),
        json!([1, 2, 3, 4]),
        "and the next frame proves it"
    );
}

/// **FROZEN BYTES: the graft happens on an OBJECT and must not happen on a
/// SCALAR.** Both arms, because a renderer that grafts unconditionally would
/// have to invent a wrapper for the scalar case, and that would change every
/// byte every existing scalar window puts on the wire.
#[test]
fn sequence_json_grafts_at_onto_an_object_and_leaves_a_scalar_alone() {
    #[derive(Clone, Serialize)]
    struct Said {
        who: &'static str,
    }

    let mut objects: Sequence<Said> = Sequence::new_capacity_only(2);
    objects.append(at("2026-10-12T09:05:00Z"), Said { who: "sam" });
    assert_eq!(
        serde_json::to_string(&objects.to_json()).unwrap(),
        r#"[{"at":"2026-10-12T09:05:00+00:00","who":"sam"}]"#,
        "an object gets `at` grafted on"
    );

    let mut scalars: Sequence<u64> = Sequence::new_capacity_only(2);
    scalars.append(at("2026-10-12T09:05:00Z"), 7);
    assert_eq!(
        serde_json::to_string(&scalars.to_json()).unwrap(),
        "[7]",
        "a scalar has nowhere to put it and is left exactly as it serialised"
    );
}

/// **THE EXPORT ITEM AND THE TRANSITION ITEM ARE ONE ITEM, SO THEY ARE ONE
/// SPELLING.** This is the assertion the defect of 2026-10-06 would have
/// failed: a consumer assembling a window from an export PLUS the transitions
/// after it must be able to append one list to the other.
#[test]
fn the_pending_item_and_the_retained_item_are_rendered_identically() {
    #[derive(Clone, Serialize)]
    struct Said {
        who: &'static str,
    }

    let mut log: Sequence<Said> = Sequence::new_capacity_only(4);
    log.append(at("2026-10-12T09:05:00Z"), Said { who: "sam" });

    let pending = log.take_pending();
    let retained = log.to_json();

    assert_eq!(
        Value::Array(pending),
        retained,
        "one function and not two expressions — the asymmetry this replaced was a \
         real defect found by threads/log's re-subscribe proof, where the two \
         mirrors differed by exactly this key"
    );
}

#[test]
fn sequence_mark_dead_replaces_a_known_state() {
    let mut log = log4();
    log.emit(m(1), 1);
    assert!(log.state().is_known(), "POSITIVE ARM");
    log.mark_dead("the fold is gone");
    assert_eq!(
        log.state(),
        &CellState::Dead {
            reason: "the fold is gone".to_string()
        }
    );
    assert_eq!(held(&log), vec![1], "the window is last-known, not erased");
}

// ─────────────────────────────────────────────────────────────────────────────
// Y. `Recency` — an ORDER, which is not a log and not a lattice
// ─────────────────────────────────────────────────────────────────────────────

fn order3() -> Recency<u64> {
    Recency::new(3)
}

fn ids(cell: &Recency<u64>) -> Vec<(String, u64)> {
    cell.now().map(|(id, v)| (id.to_string(), *v)).collect()
}

#[test]
fn recency_touch_moves_a_held_member_without_growing_the_cell() {
    let mut index = order3();
    index.touch(m(1), k("a"), 1);
    index.touch(m(2), k("b"), 2);
    assert_eq!(
        ids(&index),
        vec![(k("b"), 2), (k("a"), 1)],
        "POSITIVE ARM: front is newest"
    );

    assert!(index.touch(m(3), k("a"), 9), "a touch answers whether it moved");
    assert_eq!(
        ids(&index),
        vec![(k("a"), 9), (k("b"), 2)],
        "A TOUCH MOVES, IT DOES NOT APPEND: `a` is lifted to the front with its \
         new value, and the length does not grow"
    );
    assert_eq!(
        index.len(),
        2,
        "two distinct members touched three times is an order of two — which is \
         the whole difference from a Sequence, where it would be three items"
    );
}

#[test]
fn recency_evicts_the_oldest_at_capacity_plus_one_distinct_member() {
    let mut index = order3();
    for (n, id) in [(1u32, "a"), (2, "b"), (3, "c")] {
        index.touch(m(n), k(id), u64::from(n));
    }
    assert_eq!(index.len(), 3, "POSITIVE ARM: at capacity, nothing is evicted");

    index.touch(m(4), k("d"), 4);
    assert_eq!(
        ids(&index),
        vec![(k("d"), 4), (k("c"), 3), (k("b"), 2)],
        "the capacity + 1-th DISTINCT member evicts the oldest from the back"
    );
}

#[test]
fn recency_touch_refuses_an_input_at_or_before_the_edge_and_takes_one_after_it() {
    let mut index = order3();
    index.read(m(10), vec![(k("a"), m(9), 1)], true);

    assert!(
        !index.touch(m(10), k("b"), 2),
        "an input stamped at the edge was already in flight when the read ran"
    );
    assert!(!index.touch(m(9), k("b"), 2), "and so was an earlier one");
    assert_eq!(ids(&index), vec![(k("a"), 1)], "neither moved anything");

    assert!(
        index.touch(m(11), k("b"), 2),
        "OPPOSING ARM: strictly after the edge is live"
    );
    assert_eq!(ids(&index), vec![(k("b"), 2), (k("a"), 1)]);
}

/// **`set` CHANGES THE VALUE AND LEAVES THE ORDER ALONE, AND THE TWO STAMPS COME
/// APART HERE.** The member's own `at` is its POSITION and must not move; the
/// cell's `folded_at` is when it last folded an input and must.
#[test]
fn recency_set_changes_a_value_without_moving_the_member_or_its_stamp() {
    #[derive(Clone, Serialize)]
    struct Roll {
        unread: u64,
    }

    // An OBJECT-valued order, because the claim is about the exported `id`/`at`
    // pair and a scalar order carries neither — see
    // `recency_over_a_scalar_value_exports_an_order_with_no_ids_at_all`.
    let mut index: Recency<Roll> = Recency::new(3);
    index.touch(m(1), k("a"), Roll { unread: 1 });
    index.touch(m(2), k("b"), Roll { unread: 2 });
    let before = index.to_json();

    assert!(
        index.set(m(3), "a", Roll { unread: 99 }),
        "the member is held, so the set lands"
    );
    assert_eq!(
        index.get("a").map(|r| r.unread),
        Some(99),
        "the value changed"
    );
    assert_eq!(
        index.now().map(|(id, _)| id).collect::<Vec<_>>(),
        vec!["b", "a"],
        "and the ORDER did not: a thread being marked seen is a consumer-side \
         fact, and a producer that let it reorder the index would be recording one"
    );
    assert_eq!(
        index.state().folded_at(),
        Some(m(3)),
        "the CELL's folded_at advances, because it just folded an input"
    );

    let after = index.to_json();
    assert_ne!(before, after, "SENTINEL: the export did change");
    let stamp_of = |v: &Value, id: &str| -> String {
        v.as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == json!(id))
            .expect("the entry must be in the export")["at"]
            .as_str()
            .expect("every entry carries its position stamp")
            .to_string()
    };
    assert_eq!(
        stamp_of(&before, "a"),
        stamp_of(&after, "a"),
        "and the MEMBER's own at did NOT move — advancing it would claim a reorder \
         that did not happen, and the export's id/at pairs would stop agreeing \
         with the order they are printed in"
    );
}

/// **A FINDING, ASSERTED RATHER THAN DESCRIBED: an order over a SCALAR exports
/// no ids.**
///
/// The graft is conditional on the value serialising to an object, which is
/// right for [`Sequence`] — a scalar log item has nowhere to put a stamp, and
/// inventing a wrapper would move every byte of every existing scalar window.
/// For [`Recency`] the same conditional has a sharper consequence, because the
/// `id` is not decoration here: it is which member the entry is ABOUT. A
/// `Recency<u64>` therefore exports a bare array of values, in order, with
/// nothing saying whose they are.
///
/// Carried deliberately — it is substrate's behaviour and the wire is frozen —
/// and asserted here so that it is a known property rather than a surprise. No
/// shape in either tree instantiates a scalar `Recency`; the one `Recency` in
/// substrate's threads holds an object.
#[test]
fn recency_over_a_scalar_value_exports_an_order_with_no_ids_at_all() {
    let mut index = order3();
    index.touch(m(1), k("a"), 1);
    index.touch(m(2), k("b"), 2);

    assert_eq!(
        index.to_json(),
        json!([2, 1]),
        "the order is right and the ids are gone"
    );
    assert_eq!(
        ids(&index),
        vec![(k("b"), 2), (k("a"), 1)],
        "SENTINEL: the cell itself DOES hold the ids — the loss is in the \
         rendering, not in the cell"
    );
}

#[test]
fn recency_set_refuses_a_member_the_window_does_not_hold() {
    let mut index = order3();
    index.touch(m(1), k("a"), 1);
    assert!(
        !index.set(m(2), "absent", 9),
        "so a fold can decide THROUGH this call and dirty nothing when there was \
         nothing to change"
    );
    assert_eq!(
        index.state().folded_at(),
        Some(m(1)),
        "and a refused set folds nothing: the stamp does not advance either"
    );
    assert!(
        index.set(m(2), "a", 9),
        "OPPOSING ARM: a member it does hold is accepted"
    );
}

#[test]
fn recency_set_refuses_an_input_at_or_before_the_edge() {
    let mut index = order3();
    index.read(m(10), vec![(k("a"), m(9), 1)], true);
    assert!(!index.set(m(10), "a", 9), "the stamp rule binds `set` too");
    assert_eq!(index.get("a"), Some(&1));
    assert!(index.set(m(11), "a", 9), "OPPOSING ARM");
    assert_eq!(index.get("a"), Some(&9));
}

#[test]
fn recency_get_reads_a_member_without_moving_it() {
    let mut index = order3();
    index.touch(m(1), k("a"), 1);
    index.touch(m(2), k("b"), 2);
    assert_eq!(index.get("a"), Some(&1));
    assert_eq!(
        ids(&index),
        vec![(k("b"), 2), (k("a"), 1)],
        "reading is not touching"
    );
    assert_eq!(index.get("absent"), None);
}

#[test]
fn recency_read_takes_the_first_capacity_newest_first() {
    let mut index = order3();
    index.read(
        m(20),
        vec![
            (k("a"), m(5), 1),
            (k("b"), m(4), 2),
            (k("c"), m(3), 3),
            (k("d"), m(2), 4),
        ],
        true,
    );
    assert_eq!(
        ids(&index),
        vec![(k("a"), 1), (k("b"), 2), (k("c"), 3)],
        "the producer's order is NEWEST FIRST and anything past capacity is \
         dropped here, so the cell cannot be handed more than it declares"
    );
    assert_eq!(
        index.state(),
        &CellState::Known {
            complete: true,
            folded_at: m(20),
            since_init: false,
            covers_since: Some(m(3)),
        },
        "covers_since is the BACK of this cell — the oldest member still in the order"
    );
}

/// **THE TWO `read`s DISAGREE ABOUT `complete`, DELIBERATELY, AND THIS NAMES
/// BOTH SIDES.** `Sequence::read` downgrades a `complete: true` claim when it
/// had to drop items; `Recency::read` does not. Handed over-long input and the
/// same producer claim, the two cells must end in different states — asserted
/// rather than described, because the difference is a ruling and a future edit
/// that "tidied" one into the other would be silent.
#[test]
fn recency_read_keeps_the_producers_complete_claim_where_sequence_read_downgrades_it() {
    let mut log = Sequence::new(2, mins(60));
    log.read(m(20), vec![(m(1), 1), (m(2), 2), (m(3), 3)], true);

    let mut index: Recency<u64> = Recency::new(2);
    index.read(
        m(20),
        vec![(k("a"), m(3), 3), (k("b"), m(2), 2), (k("c"), m(1), 1)],
        true,
    );

    assert_eq!(
        log.state(),
        &CellState::Known {
            complete: false,
            folded_at: m(20),
            since_init: false,
            covers_since: Some(m(2)),
        },
        "a log that dropped the front of its window IS demonstrably short"
    );
    assert_eq!(
        index.state(),
        &CellState::Known {
            complete: true,
            folded_at: m(20),
            since_init: false,
            covers_since: Some(m(2)),
        },
        "an order asked for the newest N and kept the newest N: an over-long \
         answer is the producer being generous, not the cell being short"
    );
    assert_ne!(
        log.state(),
        index.state(),
        "SENTINEL: the two cells, same shaped input, same producer claim, and the \
         states must genuinely differ"
    );
}

#[test]
fn recency_touch_carries_both_flags_rather_than_recomputing_them() {
    let mut index = order3();
    index.read(m(10), vec![(k("a"), m(9), 1)], false);
    index.touch(m(11), k("b"), 2);
    assert_eq!(
        index.state(),
        &CellState::Known {
            complete: false,
            folded_at: m(11),
            since_init: false,
            covers_since: Some(m(9)),
        },
        "complete: false is carried, and a hydrated cell does not become \
         since_init because something happened in it"
    );

    let mut never_read = order3();
    never_read.touch(m(1), k("a"), 1);
    assert_eq!(
        never_read.state(),
        &CellState::Known {
            complete: true,
            folded_at: m(1),
            since_init: true,
            covers_since: Some(m(1)),
        },
        "OPPOSING ARM: with no read behind it, since_init stays true and the \
         export keeps saying so"
    );
}

/// **AN ORDER IS NOT COMMUTATIVE, AND THAT IS THE CONTRACT.** Asserted as a
/// positive claim so that nobody later "fixes" this cell into a lattice: its
/// meaning IS the arrival order, which is exactly what `Semilattice` exists to
/// be independent of.
#[test]
fn recency_is_an_order_and_therefore_does_not_commute_at_n2() {
    let mut forwards = order3();
    forwards.touch(m(1), k("a"), 1);
    forwards.touch(m(2), k("b"), 2);

    let mut backwards = order3();
    backwards.touch(m(1), k("b"), 2);
    backwards.touch(m(2), k("a"), 1);

    assert_ne!(
        ids(&forwards),
        ids(&backwards),
        "the order the two members were touched in IS the value of this cell"
    );
}

/// **FROZEN BYTES: `id` and `at` ride on every entry**, so the order a consumer
/// is shown is checkable rather than merely asserted.
#[test]
fn recency_json_grafts_id_and_at_onto_each_entry() {
    #[derive(Clone, Serialize)]
    struct Roll {
        unread: u64,
    }

    let mut index: Recency<Roll> = Recency::new(3);
    index.touch(
        at("2026-10-12T09:01:00Z"),
        k("t-1"),
        Roll { unread: 2 },
    );
    index.touch(
        at("2026-10-12T09:02:00Z"),
        k("t-2"),
        Roll { unread: 0 },
    );

    assert_eq!(
        serde_json::to_string(&index.to_json()).unwrap(),
        r#"[{"at":"2026-10-12T09:02:00+00:00","id":"t-2","unread":0},{"at":"2026-10-12T09:01:00+00:00","id":"t-1","unread":2}]"#,
        "newest first, each entry carrying its own id and position stamp"
    );
    assert_eq!(
        serde_json::to_string(&index.to_json_retained(crate::wire::Retain::Count(1))).unwrap(),
        r#"[{"at":"2026-10-12T09:02:00+00:00","id":"t-2","unread":0}]"#,
        "and the newest n, which for this cell is simply the first n"
    );
    assert_eq!(
        index.len(),
        2,
        "the projection evicted nothing"
    );
}

#[test]
fn recency_mark_not_hydrated_and_mark_dead_are_different_claims() {
    let mut unreachable = order3();
    unreachable.mark_not_hydrated("the index read failed");
    let mut gone = order3();
    gone.mark_dead("the fold is gone");

    assert_eq!(
        unreachable.state(),
        &CellState::NotHydrated {
            reason: Some("the index read failed".to_string())
        }
    );
    assert_eq!(
        gone.state(),
        &CellState::Dead {
            reason: "the fold is gone".to_string()
        }
    );
    assert_ne!(
        unreachable.state(),
        gone.state(),
        "a mirror has to be able to render two different question marks"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// E. `Edge` — a level with a log of its changes
// ─────────────────────────────────────────────────────────────────────────────

fn status() -> Edge<&'static str> {
    Edge::new(4, mins(60))
}

fn moves(cell: &Edge<&'static str>) -> Vec<(Option<&'static str>, &'static str)> {
    cell.transitions()
        .now()
        .map(|t| (t.from, t.to))
        .collect()
}

#[test]
fn edge_the_first_level_a_shape_learns_is_not_a_transition() {
    let mut cell = status();
    cell.emit(m(1), "idle");
    assert_eq!(cell.now(), Some(&"idle"));
    assert!(
        moves(&cell).is_empty(),
        "the first level is not a change FROM anything, and a `from: null` \
         transition would be one every cold subscriber had to learn to ignore"
    );
    assert!(
        cell.transitions_mut().take_pending().is_empty(),
        "and nothing was published"
    );
}

#[test]
fn edge_a_change_of_level_records_exactly_one_transition() {
    let mut cell = status();
    cell.emit(m(1), "idle");
    cell.emit(m(2), "busy");

    assert_eq!(cell.now(), Some(&"busy"));
    assert_eq!(
        moves(&cell),
        vec![(Some("idle"), "busy")],
        "OPPOSING ARM for the two refusals below: a real change DOES record, so a \
         cell that never records anything fails here"
    );
    assert_eq!(cell.transitions_mut().take_pending().len(), 1);
}

#[test]
fn edge_a_reassertion_of_the_same_level_records_nothing() {
    let mut cell = status();
    cell.emit(m(1), "idle");
    cell.emit(m(2), "idle");
    cell.emit(m(3), "idle");

    assert!(
        moves(&cell).is_empty(),
        "IDEMPOTENCE of a level: three statements of the same level are one level"
    );
    assert_eq!(
        cell.state().folded_at(),
        Some(m(3)),
        "the level is still SET each time — a re-assertion is refused as evidence \
         of change, never as evidence of the value"
    );
}

#[test]
fn edge_past_the_hydration_edge_it_sets_the_level_and_records_nothing() {
    let mut cell = status();
    cell.read(m(10), "idle");

    cell.emit(m(10), "busy");
    assert_eq!(
        cell.now(),
        Some(&"busy"),
        "THE LEVEL IS STILL SET: the input is refused as evidence of a CHANGE, \
         not as evidence of the value"
    );
    assert!(
        moves(&cell).is_empty(),
        "and no transition is fabricated — an event already in flight when the \
         read ran cannot have changed a level the read already observed"
    );

    cell.emit(m(11), "idle");
    assert_eq!(
        moves(&cell),
        vec![(Some("busy"), "idle")],
        "OPPOSING ARM: strictly after the edge, a change records normally"
    );
}

#[test]
fn edge_read_marks_the_transition_log_since_init_rather_than_filling_it() {
    let mut cell = status();
    cell.read(m(10), "idle");

    assert_eq!(cell.now(), Some(&"idle"), "the LEVEL hydrates …");
    assert!(moves(&cell).is_empty(), "… and the log does not");
    assert_eq!(
        cell.transitions().state(),
        &CellState::Known {
            complete: true,
            folded_at: m(10),
            since_init: true,
            covers_since: Some(m(10)),
        },
        "the log says since_init, so nobody counts its contents as history"
    );
    assert_ne!(
        cell.state(),
        cell.transitions().state(),
        "SENTINEL: the level and its log carry DIFFERENT provenance — one state \
         word could not say both"
    );
}

#[test]
fn edge_not_hydrated_takes_the_transition_log_with_it() {
    let mut cell = status();
    cell.read(m(10), "idle");
    assert!(cell.state().is_known(), "POSITIVE ARM: Known before the failure");

    cell.not_hydrated("the status derivation hit a store error");

    let unreached = CellState::NotHydrated {
        reason: Some("the status derivation hit a store error".to_string()),
    };
    assert_eq!(
        cell.state(),
        &unreached,
        "a level nobody could read is NotHydrated — not Known with no value, \
         which is indistinguishable from a real level, and not Dead, which \
         claims the whole task died"
    );
    assert_eq!(
        cell.transitions().state(),
        &unreached,
        "AND THE LOG GOES WITH IT: a level nobody could read has no transitions \
         anybody should trust"
    );
}

#[test]
fn edge_mark_dead_takes_the_transition_log_with_it() {
    let mut cell = status();
    cell.emit(m(1), "idle");
    cell.emit(m(2), "busy");
    assert!(
        cell.state().is_known() && cell.transitions().state().is_known(),
        "POSITIVE ARM: both are Known before the kill, so a constant Dead fails"
    );

    cell.mark_dead("the shape task panicked");

    let gone = CellState::Dead {
        reason: "the shape task panicked".to_string(),
    };
    assert_eq!(cell.state(), &gone, "the level is dead");
    assert_eq!(
        cell.transitions().state(),
        &gone,
        "AND SO IS THE LOG — it is the level's own history and cannot be current \
         when the level is not"
    );
    assert_eq!(
        moves(&cell),
        vec![(Some("idle"), "busy")],
        "last-known, not erased"
    );
}

#[test]
fn edge_take_dirty_reports_once_and_then_clears_for_every_entry() {
    for (label, which) in [("emit", 0u8), ("read", 1), ("not_hydrated", 2)] {
        let mut cell = status();
        match which {
            0 => cell.emit(m(1), "idle"),
            1 => cell.read(m(1), "idle"),
            _ => cell.not_hydrated("why"),
        }
        assert!(cell.take_dirty(), "{label} must dirty the cell");
        assert!(!cell.take_dirty(), "{label} must then be clear");
    }
}

#[test]
fn edge_transitions_mut_is_how_a_drain_reaches_the_pending_items() {
    let mut cell = status();
    cell.emit(m(1), "idle");
    cell.emit(m(2), "busy");
    let pending = cell.transitions_mut().take_pending();
    assert_eq!(pending.len(), 1);
    assert!(
        cell.transitions_mut().take_pending().is_empty(),
        "and taking them clears them"
    );
}

/// **FROZEN BYTES for a transition item**, including the `from: null` of the
/// shape that could not happen through `emit` but can be constructed by hand,
/// and the `at` graft every windowed item carries.
#[test]
fn a_transition_item_serialises_from_to_and_its_stamp() {
    let mut log: Sequence<Transition<&str>> = Sequence::new_capacity_only(4);
    log.append(
        at("2026-10-12T09:05:00Z"),
        Transition {
            from: Some("idle"),
            to: "busy",
        },
    );
    assert_eq!(
        serde_json::to_string(&log.to_json()).unwrap(),
        r#"[{"at":"2026-10-12T09:05:00+00:00","from":"idle","to":"busy"}]"#
    );

    let mut first: Sequence<Transition<&str>> = Sequence::new_capacity_only(4);
    first.append(
        at("2026-10-12T09:05:00Z"),
        Transition {
            from: None,
            to: "busy",
        },
    );
    assert_eq!(
        serde_json::to_string(&first.to_json()).unwrap(),
        r#"[{"at":"2026-10-12T09:05:00+00:00","from":null,"to":"busy"}]"#,
        "`from` is not skipped when absent: a consumer parsing a transition reads \
         the same two keys every time"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// O. `Outbox` — what one fold produced
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn outbox_counts_every_one_of_its_five_arms() {
    assert_eq!(Outbox::default().len(), 0, "POSITIVE ARM: nothing is nothing");

    let rows = Outbox {
        rows: vec![k("a")],
        ..Outbox::default()
    };
    assert_eq!(rows.len(), 1, "rows are counted");

    let gone = Outbox {
        gone: vec![k("a")],
        ..Outbox::default()
    };
    assert_eq!(gone.len(), 1, "gone is counted");

    let added = Outbox {
        added: vec![k("a")],
        ..Outbox::default()
    };
    assert_eq!(added.len(), 1, "added is counted");

    let transitions = Outbox {
        transitions: vec![(k("a"), "moves", json!({"to": "busy"}))],
        ..Outbox::default()
    };
    assert_eq!(transitions.len(), 1, "transitions are counted");

    let shared = Outbox {
        shared: true,
        ..Outbox::default()
    };
    assert_eq!(
        shared.len(),
        1,
        "AND THE SHARED BIT IS COUNTED: a drain that produced only a shape-level \
         change still produced something, and an outbox calling itself empty \
         would have it dropped"
    );

    let everything = Outbox {
        rows: vec![k("a"), k("b")],
        gone: vec![k("c")],
        added: vec![k("d")],
        transitions: vec![(k("a"), "moves", json!(1)), (k("b"), "moves", json!(2))],
        shared: true,
    };
    assert_eq!(
        everything.len(),
        7,
        "2 rows + 1 gone + 1 added + 2 transitions + 1 shared — an implementation \
         that forgot any single arm cannot reach 7"
    );
}

#[test]
fn outbox_is_empty_only_when_every_arm_is() {
    assert!(Outbox::default().is_empty());
    for populated in [
        Outbox {
            rows: vec![k("a")],
            ..Outbox::default()
        },
        Outbox {
            gone: vec![k("a")],
            ..Outbox::default()
        },
        Outbox {
            added: vec![k("a")],
            ..Outbox::default()
        },
        Outbox {
            transitions: vec![(k("a"), "moves", json!(1))],
            ..Outbox::default()
        },
        Outbox {
            shared: true,
            ..Outbox::default()
        },
    ] {
        assert!(
            !populated.is_empty(),
            "any single arm makes the outbox non-empty: {populated:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Z. `Membership` and `Root` — the composite, through a shape
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn a_complete_answer_of_zero_keys_is_not_a_refusal_to_enumerate() {
    let empty_pool = Membership::Known {
        keys: vec![],
        complete: true,
    };
    let cannot_say = Membership::NotHydrated {
        reason: "the ledger cannot enumerate its namespace".to_string(),
    };
    assert_ne!(
        empty_pool, cannot_say,
        "two values and not three: `Empty` is deliberately absent, because an \
         empty pool is a COMPLETE answer of zero and the genuinely different \
         statement is 'I cannot enumerate'"
    );
    assert_ne!(
        empty_pool,
        Membership::Known {
            keys: vec![],
            complete: false,
        },
        "and a short enumeration of zero is a third thing again"
    );
}

/// One row of the test shape: one of every cell type that can live under a key.
#[derive(Default)]
struct Row {
    label: Register<&'static str>,
    hits: Counter,
    state: Option<Edge<&'static str>>,
    log: Option<Sequence<u64>>,
}

impl Row {
    fn edge(&mut self) -> &mut Edge<&'static str> {
        self.state.get_or_insert_with(|| Edge::new(4, mins(60)))
    }

    fn log(&mut self) -> &mut Sequence<u64> {
        self.log.get_or_insert_with(|| Sequence::new(4, mins(60)))
    }
}

/// **A WHOLE ROOT**: it holds every key it knows about, so it does not override
/// `members` and the default answer is the right one.
#[derive(Default)]
struct Pool {
    rows: Map<String, Row>,
    since: Semilattice<Min<Timestamp>>,
    note: Register<&'static str>,
}

impl Root for Pool {
    fn keys(&self) -> Vec<String> {
        self.rows.keys().cloned().collect()
    }

    fn row(&self, key: &str) -> Option<Value> {
        self.row_at(key, crate::wire::Retain::Max)
    }

    fn row_at(&self, key: &str, retain: crate::wire::Retain) -> Option<Value> {
        let row = self.rows.get(&key.to_string())?;
        Some(json!({
            "label": row.label.now(),
            "hits": row.hits.now(),
            "state": row.state.as_ref().and_then(Edge::now),
            "log": row.log.as_ref().map(|l| l.to_json_retained(retain)),
        }))
    }

    fn cells(&self, key: &str) -> Option<Value> {
        let row = self.rows.get(&key.to_string())?;
        Some(json!({
            "label": row.label.state(),
            "hits": row.hits.state(),
        }))
    }

    fn shared(&self) -> Value {
        json!({ "note": self.note.now() })
    }

    fn shared_cells(&self) -> Value {
        json!({
            "note": self.note.state(),
            "since": self.since.state(),
        })
    }

    fn drain(&mut self) -> Outbox {
        let mut out = Outbox::default();
        for key in self.rows.take_dirty_keys() {
            // Pull each dirty row's transitions out BEFORE deciding on the row
            // frame, so a row that only moved its log still publishes both.
            if let Some(row) = self.rows.get_mut(&key) {
                let pending = row
                    .state
                    .as_mut()
                    .map(|e| e.transitions_mut().take_pending())
                    .unwrap_or_default();
                for item in pending {
                    out.transitions.push((key.clone(), "state", item));
                }
                for item in row.log.as_mut().map(Sequence::take_pending).unwrap_or_default() {
                    out.transitions.push((key.clone(), "log", item));
                }
                row.label.take_dirty();
                row.hits.take_dirty();
            }
            out.rows.push(key);
        }
        out.added = self.rows.take_added();
        out.gone = self.rows.take_removed();
        out.shared = self.note.take_dirty() | self.since.take_dirty();
        out
    }

    fn mark_dead(&mut self, reason: &str) {
        self.rows.mark_dead(reason);
        for (_, row) in self.rows.entries_mut() {
            row.label.mark_dead(reason);
            if let Some(edge) = row.state.as_mut() {
                edge.mark_dead(reason);
            }
            if let Some(log) = row.log.as_mut() {
                log.mark_dead(reason);
            }
        }
        self.note.mark_dead(reason);
        self.since.mark_dead(reason);
    }
}

fn loaded_pool() -> Pool {
    let mut pool = Pool::default();
    {
        let row = pool.rows.key(k("a"));
        row.label.emit(m(1), "first");
        row.hits.add(m(1), 1);
        row.edge().emit(m(1), "idle");
        row.edge().emit(m(2), "busy");
        row.log().emit(m(2), 7);
    }
    pool.note.emit(m(1), "a note");
    pool.since.emit(m(1), Min(m(1)));
    pool
}

#[test]
fn a_drain_reports_what_the_fold_did() {
    let mut pool = loaded_pool();
    let out = pool.drain();

    assert_eq!(out.rows, vec![k("a")], "one dirty key, one row frame");
    assert_eq!(out.added, vec![k("a")], "the key joined the pool");
    assert!(out.gone.is_empty());
    assert!(out.shared, "the shape-level block moved");
    assert_eq!(
        out.transitions.len(),
        2,
        "one level change and one log append, each its own occurrence"
    );
    assert_eq!(out.len(), 5, "and the outbox counts all of it");
}

/// **THE DRAIN CLEARS, AND A SECOND DRAIN IS EMPTY.**
///
/// The clearing is half of `drain`'s contract: a drain that collected without
/// clearing would re-publish the same row on every input for the life of the
/// shape. Every arm is asserted, not just the row list — and the primitives
/// underneath (`take_dirty_keys`, `take_added`, `take_removed`, `take_dirty`,
/// `take_pending`) each have their own clearing test above, so this one is not
/// the only thing standing between a leak and a green suite.
#[test]
fn a_second_drain_is_empty() {
    let mut pool = loaded_pool();
    let first = pool.drain();
    assert!(!first.is_empty(), "SENTINEL: there was something to drain");

    let second = pool.drain();
    assert!(
        second.rows.is_empty(),
        "no row re-published: {:?}",
        second.rows
    );
    assert!(second.added.is_empty(), "no added re-published");
    assert!(second.gone.is_empty(), "no gone re-published");
    assert!(
        second.transitions.is_empty(),
        "no transition re-published: {:?}",
        second.transitions
    );
    assert!(!second.shared, "and the shared bit is down again");
    assert!(
        second.is_empty(),
        "nothing happened between the two drains, so the second produced nothing"
    );
}

#[test]
fn a_drain_coalesces_many_cell_writes_under_one_key_into_one_row() {
    let mut pool = Pool::default();
    for n in 1..=5u32 {
        let row = pool.rows.key(k("a"));
        row.label.emit(m(n), "moved again");
        row.hits.add(m(n), 1);
    }
    let out = pool.drain();
    assert_eq!(
        out.rows,
        vec![k("a")],
        "five writes under one key coalesce into ONE row frame, which loses \
         nothing because a row is an idempotent upsert"
    );
}

#[test]
fn a_drain_reports_a_removed_key_once_and_stops_dirtying_it() {
    let mut pool = loaded_pool();
    pool.drain();
    pool.rows.remove(&k("a"));

    let out = pool.drain();
    assert_eq!(out.gone, vec![k("a")]);
    assert!(
        out.rows.is_empty(),
        "a key that left the pool must not also publish a row frame"
    );
    assert!(pool.drain().is_empty(), "and it leaves once");
}

/// **`mark_dead` REACHES EVERY CELL THE ROOT OWNS**, asserted cell by cell, with
/// the positive arm first so that a root whose cells were born `Dead` could not
/// pass.
#[test]
fn mark_dead_reaches_every_cell_the_root_owns() {
    let mut pool = loaded_pool();

    // POSITIVE ARM, cell by cell: nothing is Dead yet.
    let alive = |pool: &Pool| -> Vec<(&'static str, bool)> {
        let row = pool.rows.get(&k("a")).expect("the row is held");
        vec![
            ("rows", matches!(pool.rows.state(), CellState::Dead { .. })),
            (
                "members",
                matches!(pool.rows.members_state(), CellState::Dead { .. }),
            ),
            ("label", matches!(row.label.state(), CellState::Dead { .. })),
            (
                "state",
                matches!(
                    row.state.as_ref().map(Edge::state),
                    Some(CellState::Dead { .. })
                ),
            ),
            (
                "state.transitions",
                matches!(
                    row.state.as_ref().map(|e| e.transitions().state()),
                    Some(CellState::Dead { .. })
                ),
            ),
            (
                "log",
                matches!(
                    row.log.as_ref().map(Sequence::state),
                    Some(CellState::Dead { .. })
                ),
            ),
            ("note", matches!(pool.note.state(), CellState::Dead { .. })),
            ("since", matches!(pool.since.state(), CellState::Dead { .. })),
        ]
    };

    let before = alive(&pool);
    assert!(
        before.iter().all(|(_, dead)| !dead),
        "POSITIVE ARM: every cell must be ALIVE first, or a constant Dead passes \
         this test: {before:?}"
    );

    pool.mark_dead("the shape task panicked");

    let after = alive(&pool);
    let survivors: Vec<&str> = after
        .iter()
        .filter(|(_, dead)| !dead)
        .map(|(name, _)| *name)
        .collect();
    assert!(
        survivors.is_empty(),
        "EVERY cell must be Dead after mark_dead; these were left alive: {survivors:?}"
    );

    // And the reason is carried, so an implementation that writes a blank Dead
    // fails too.
    assert_eq!(
        pool.rows.state(),
        &CellState::Dead {
            reason: "the shape task panicked".to_string()
        }
    );
    assert_eq!(
        pool.note.state(),
        &CellState::Dead {
            reason: "the shape task panicked".to_string()
        }
    );
}

#[test]
fn the_default_members_is_the_held_keys_complete() {
    let mut pool = Pool::default();
    pool.rows.key(k("b"));
    pool.rows.key(k("a"));

    assert_eq!(
        pool.members(),
        Membership::Known {
            keys: vec![k("a"), k("b")],
            complete: true,
        },
        "exactly right for a Whole root, which holds everything it knows about — \
         and in KEY ORDER, which is what added/gone move over"
    );
}

/// **A `PerKey` ROOT OVERRIDES `members`, AND THE TWO ANSWERS GENUINELY
/// DIFFER.** Without the override the default would answer the held keys, which
/// on this root is a strict subset of what exists — the sentinel asserts the
/// gap, so the test cannot pass by the override being a no-op.
#[test]
fn a_per_key_root_overrides_members_and_the_default_would_have_been_wrong() {
    struct Ledger {
        rows: Map<String, Row>,
    }

    impl Root for Ledger {
        fn keys(&self) -> Vec<String> {
            self.rows.keys().cloned().collect()
        }
        fn members(&self) -> Membership {
            Membership::Known {
                keys: self.rows.members().cloned().collect(),
                complete: true,
            }
        }
        fn row(&self, _key: &str) -> Option<Value> {
            None
        }
        fn cells(&self, _key: &str) -> Option<Value> {
            None
        }
        fn shared(&self) -> Value {
            json!({})
        }
        fn shared_cells(&self) -> Value {
            json!({})
        }
        fn drain(&mut self) -> Outbox {
            Outbox::default()
        }
        fn mark_dead(&mut self, reason: &str) {
            self.rows.mark_dead(reason);
        }
    }

    let mut ledger = Ledger { rows: Map::new() };
    ledger.rows.key(k("held"));
    ledger.rows.member(k("exists-only"));

    let held = ledger.keys();
    let exists = match ledger.members() {
        Membership::Known { keys, .. } => keys,
        Membership::NotHydrated { reason } => panic!("expected an enumeration: {reason}"),
    };

    assert_ne!(
        held, exists,
        "SENTINEL: on a PerKey root the two answers MUST come apart, or this test \
         cannot tell an override from the default"
    );
    assert_eq!(held, vec![k("held")]);
    assert_eq!(exists, vec![k("exists-only"), k("held")]);
}

#[test]
fn a_root_that_cannot_enumerate_says_so_with_a_reason() {
    struct Opaque;

    impl Root for Opaque {
        fn keys(&self) -> Vec<String> {
            vec![]
        }
        fn members(&self) -> Membership {
            Membership::NotHydrated {
                reason: "this producer cannot enumerate its namespace".to_string(),
            }
        }
        fn row(&self, _key: &str) -> Option<Value> {
            None
        }
        fn cells(&self, _key: &str) -> Option<Value> {
            None
        }
        fn shared(&self) -> Value {
            json!({})
        }
        fn shared_cells(&self) -> Value {
            json!({})
        }
        fn drain(&mut self) -> Outbox {
            Outbox::default()
        }
        fn mark_dead(&mut self, _reason: &str) {}
    }

    assert_ne!(
        Opaque.members(),
        Membership::Known {
            keys: vec![],
            complete: true,
        },
        "'I cannot enumerate' must not collapse into 'the pool is empty' — there \
         is no MEMBERS flag because the three-valued state already says it"
    );
}

/// **THE DEFAULT `row_at` IGNORES `retain`, AND AN OVERRIDE HONOURS IT.** Both
/// arms, because the default is only defensible while the shape's own ceiling
/// refuses every `Retain::Count` at subscribe time — and the direction that
/// bites is a shape declaring the ceiling without overriding this, where the ask
/// would be accepted and silently ignored.
#[test]
fn the_default_row_at_answers_row_and_an_override_honours_retain() {
    struct Plain {
        rows: Map<String, Row>,
    }

    impl Root for Plain {
        fn keys(&self) -> Vec<String> {
            self.rows.keys().cloned().collect()
        }
        fn row(&self, key: &str) -> Option<Value> {
            let row = self.rows.get(&key.to_string())?;
            Some(json!({ "log": row.log.as_ref().map(Sequence::to_json) }))
        }
        fn cells(&self, _key: &str) -> Option<Value> {
            None
        }
        fn shared(&self) -> Value {
            json!({})
        }
        fn shared_cells(&self) -> Value {
            json!({})
        }
        fn drain(&mut self) -> Outbox {
            Outbox::default()
        }
        fn mark_dead(&mut self, reason: &str) {
            self.rows.mark_dead(reason);
        }
    }

    let mut plain = Plain { rows: Map::new() };
    {
        let row = plain.rows.key(k("a"));
        for n in 1..=3u32 {
            row.log().emit(m(n), u64::from(n));
        }
    }

    assert_eq!(
        plain.row_at("a", crate::wire::Retain::Count(1)),
        plain.row("a"),
        "the DEFAULT row_at ignores the ask and answers the whole row — correct \
         while the only reachable ask is Retain::Max"
    );

    // The same rows, through a root that DOES override it.
    let mut honours = Pool::default();
    {
        let row = honours.rows.key(k("a"));
        for n in 1..=3u32 {
            row.log().emit(m(n), u64::from(n));
        }
    }
    let whole = honours.row_at("a", crate::wire::Retain::Max).expect("a row");
    let trimmed = honours
        .row_at("a", crate::wire::Retain::Count(1))
        .expect("a row");
    assert_eq!(whole["log"], json!([1, 2, 3]));
    assert_eq!(
        trimmed["log"],
        json!([3]),
        "OPPOSING ARM: an overriding root applies the ask to every windowed cell"
    );
    assert_ne!(
        whole, trimmed,
        "SENTINEL: the override must actually change the answer, or it is the \
         default wearing a hat"
    );
}

#[test]
fn a_roots_export_reports_per_cell_provenance_beside_the_values() {
    let pool = loaded_pool();
    let cells = pool.cells("a").expect("the row is held");
    assert_eq!(
        cells["hits"]["since_init"],
        json!(true),
        "the counter's own state rides on the export, and it says since_init \
         because a Counter has no hydrate entry at all"
    );
    assert_eq!(cells["label"]["state"], json!("known"));
    assert_eq!(
        pool.cells("absent"),
        None,
        "and a key the root does not hold has no cell block"
    );
    assert_eq!(
        pool.shared_cells()["since"]["state"],
        json!("known"),
        "the shape-level cells have their own block"
    );
}
