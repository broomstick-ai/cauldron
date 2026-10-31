//! **A `version_refused` SUBSCRIBE PUTS THE MIRROR IN ITS TERMINAL STATE (R2.S8).**
//!
//! Rule 2.6's third state: the producer will not serve the contract this seam
//! states for a shape. The chassis door answers that subscribe with the code
//! `version_refused` (`lib/threads/tests/chassis_api.rs`
//! `a_version_refusal_carries_version_refused_and_an_unknown_method_its_own`),
//! and `link.rs` hands every subscribe refusal to `Held::refuse`. This file
//! proves what `Held::refuse` does with it: the mirror goes TERMINAL through
//! `Mirror::refused_version`, carrying the version this seam stated
//! (`mod.rs`'s per-shape table), and the entry records the code.
//!
//! **Opposed, three ways.** The same sentence under the untyped code `error` —
//! what a door that drops the code would send — must NOT go terminal, so a
//! `refuse` that went terminal on EVERY code cannot pass; a refusal of `accounts`
//! must leave `threads`, in the same book, live and still folding frames, so a
//! `refuse` that took EVERY MIRROR terminal cannot pass; and the refused shape
//! itself must go terminal, so one that never did cannot pass either.

use seam_reference::held::{EntryState, Held};
use shapewire::envelope::{codes, Refusal};

const SENTENCE: &str = "shape accounts: built against v2; this producer serves [3]. The link is \
    REFUSED and nothing was queued — it is not served on a nearer version, because the \
    versions are a set and not a range";

#[test]
fn a_version_refused_subscribe_goes_terminal_through_refused_version() {
    let mut h = Held::new();
    h.refuse("accounts", None, &Refusal::new(codes::VERSION_REFUSED, SENTENCE));
    let m = h.mirror("accounts").expect("accounts is in the book");
    let vm = m
        .version_mismatch()
        .expect("a version_refused subscribe must go terminal THROUGH refused_version");
    assert_eq!(vm.shape, "accounts");
    assert_eq!(vm.wanted, 2, "the version this seam states for accounts: {vm:?}");
    assert!(m.refusal().is_some(), "and the mirror is refused, i.e. terminal");
    match &h.book.get("accounts", None).expect("the standing entry").state {
        EntryState::Refused { code, .. } => assert_eq!(code, codes::VERSION_REFUSED),
        other => panic!("the entry must record the refusal: {other:?}"),
    }
}

#[test]
fn the_same_sentence_under_a_plain_error_code_is_not_terminal() {
    let mut h = Held::new();
    h.refuse("accounts", None, &Refusal::new(codes::ERROR, SENTENCE));
    let m = h.mirror("accounts").expect("accounts is in the book");
    assert!(m.version_mismatch().is_none(), "an untyped refusal is not rule 2.6's");
    assert!(m.refusal().is_none(), "and it does not take the whole shape down");
}

/// **A REFUSAL OF ONE SHAPE LEAVES THE OTHERS LIVE.** `version_refused` is about
/// ONE shape's contract; `threads` in the same book is served on its own version
/// and must stay live and keep folding. The frame is the producer's own, out of
/// `conformance/one_thread.jsonl`.
///
/// **THE MUTATION.** `Held::refuse` taking every mirror terminal
/// (`for m in self.mirrors.values_mut() { m.refused_version(..) }`): `threads`
/// reads refused and the first assertion fires.
#[test]
fn a_version_refusal_of_accounts_leaves_threads_live_and_folding() {
    let mut h = Held::new();
    h.refuse("accounts", None, &Refusal::new(codes::VERSION_REFUSED, SENTENCE));
    assert!(
        h.mirror("accounts").and_then(|m| m.version_mismatch()).is_some(),
        "POSITIVE SENTINEL: the refused shape did go terminal"
    );
    let t = h.mirror("threads").expect("threads is in the book");
    assert!(t.refusal().is_none(), "threads was not refused: {:?}", t.refusal());
    assert!(!t.dead(), "and is not terminal");
    let before = t.exports();

    let frame = include_str!("../conformance/one_thread.jsonl")
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v.get("frame").cloned())
        .find(|f| f["shape"] == "threads" && f["op"] == "read")
        .expect("one_thread.jsonl carries a threads export");
    h.fold(&frame);
    let t = h.mirror("threads").expect("threads is in the book");
    assert_eq!(t.exports(), before + 1, "threads still accepts a frame after accounts was refused");
    assert!(t.refusal().is_none() && !t.dead(), "and is still live");
}

/// **`contract_version` IS THE TABLE.** `seamdiff --inventory` reads
/// `SHAPE_CONTRACT_VERSIONS` as TEXT (so it can run without compiling a
/// consumer), which means a `contract_version` that ignored the table — the
/// function `link.rs` actually sends — would be invisible to it. This is the
/// compiled half: every row answers its own version, and a name with no row
/// answers `SHAPE_CONTRACT_DEFAULT`.
///
/// **Sentinel:** at least two rows and at least two DISTINCT versions, so a
/// function returning any one constant cannot satisfy every row.
///
/// **THE MUTATION.** `contract_version` reduced to `SHAPE_CONTRACT_DEFAULT`:
/// the `accounts` row (2) fires.
#[test]
fn contract_version_answers_every_row_of_its_table() {
    use seam_reference::{contract_version, SHAPE_CONTRACT_DEFAULT, SHAPE_CONTRACT_VERSIONS};
    let distinct: std::collections::BTreeSet<u32> =
        SHAPE_CONTRACT_VERSIONS.iter().map(|(_, v)| *v).collect();
    assert!(SHAPE_CONTRACT_VERSIONS.len() >= 2, "{SHAPE_CONTRACT_VERSIONS:?}");
    assert!(distinct.len() >= 2, "two distinct versions, or a constant passes: {distinct:?}");
    for (name, v) in SHAPE_CONTRACT_VERSIONS {
        assert_eq!(contract_version(name), *v, "the row for `{name}`");
    }
    assert_eq!(contract_version("no-such-shape"), SHAPE_CONTRACT_DEFAULT);
}
