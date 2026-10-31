//! # The operation payload dtypes — the census that keeps the set closed
//!
//! Operator: *"Payload shapes (dtypes etc) are sufficient from the tech
//! standpoint, and maybe we do an MCP carve-out in the future with a verbs
//! library, which maps onto those dtypes."*
//!
//! **The rule these tests enforce is the one that keeps a closed set honest:**
//! *if a dtype has no instance, do not add it.* A set derived from a document
//! rather than from operations grows a member per plausible future and then
//! describes nothing, which is `docs/shape-design-guide.md` rule 10's stub
//! wearing a taxonomy. `every_dtype_has_a_declared_instance` is that rule as a
//! failing test — it is the reason `blob` is not in the set, and it is what would
//! fail on the day somebody adds a twelfth member with nothing behind it.
//!
//! Well-formedness is checked at COMPILE time by `declare_operation!` and is not
//! re-tested here. What is tested here is what a `const` cannot see: that the set
//! is grounded in instances, and that the declarations name paths this producer
//! actually serves.
//!
//! **Every test here was forced red by the mutation named in its own docstring,
//! and the red was checked for `error[E` before it was believed.**

use std::collections::BTreeSet;

use shapewire::cell::Cells;
use shapewire::hub::Shape;
use shapewire::operation::{DType, Presence, FAMILIES};
use threads::operations::DECLARED;
use threads::server::WS_METHODS;
use threads::shape::Input;

/// Every dtype family used anywhere in a declared payload OR a declared cell,
/// including inside a `list` or a `map`, because a `list<cursor>` is an instance
/// of `cursor` as much as of `list`.
///
/// **Cells count since 2026-10-08.** The set describes both directions now —
/// what an operation takes and what a shape carries (`shapewire::cell`) — and
/// the twelfth member, `map`, has its only instances on the output side
/// (`accounts.threads`, `accounts.gauge`, `threads.app`). A census that read
/// only payloads would call it an orphan; one that reads both is the honest
/// grounding rule.
fn families_in_use() -> BTreeSet<&'static str> {
    fn walk(d: &DType, out: &mut BTreeSet<&'static str>) {
        out.insert(d.family());
        if let DType::List(inner) | DType::Map(inner) = d {
            walk(inner, out);
        }
    }
    let mut out = BTreeSet::new();
    for op in DECLARED {
        for p in op.takes {
            walk(&p.dtype, &mut out);
        }
    }
    for manifest in every_manifest() {
        for c in manifest.cells() {
            walk(&c.dtype, &mut out);
            for f in c.fields {
                walk(&f.dtype, &mut out);
            }
        }
    }
    out
}

/// Every cell manifest this producer declares, per-key and shape-level.
fn every_manifest() -> Vec<Cells> {
    use threads::shapes::{
        accounts::Accounts, heartbeat::Heartbeat, log::Log, narration::Narration,
        recent::Recent, summary::Summary, threads::Threads,
    };
    vec![
        <Accounts as Shape<Input>>::CELLS,
        <Accounts as Shape<Input>>::SHARED,
        <Threads as Shape<Input>>::CELLS,
        <Threads as Shape<Input>>::SHARED,
        <Summary as Shape<Input>>::CELLS,
        <Summary as Shape<Input>>::SHARED,
        <Log as Shape<Input>>::CELLS,
        <Log as Shape<Input>>::SHARED,
        <Recent as Shape<Input>>::CELLS,
        <Recent as Shape<Input>>::SHARED,
        <Narration as Shape<Input>>::CELLS,
        <Narration as Shape<Input>>::SHARED,
        <Heartbeat as Shape<Input>>::CELLS,
        <Heartbeat as Shape<Input>>::SHARED,
    ]
}

/// **EVERY MEMBER OF THE CLOSED SET HAS AT LEAST ONE DECLARED INSTANCE.**
///
/// The derivation rule, mechanised. A dtype is in the set because an operation
/// this producer serves needs it, and the moment that stops being true this test
/// says which member has become a claim about the future.
///
/// **It is also why `blob` is absent.** There is no bytes, base64 or file-body
/// parameter in the fifty method paths, and `threads` implements neither
/// `Resources` nor `Uploads` — so adding `blob` would put a member here that this
/// test would immediately fail on.
///
/// **THE MUTATION.** In `shapewire/src/operation.rs`, add `"blob"` to
/// `FAMILIES` — the candidate set's seventh member, restored without an instance.
///
/// Forced red:
/// ```text
/// every dtype in the closed set has at least one declared instance — a member
/// with none is a claim about the future, not a description: ["blob"]
/// ```
#[test]
fn every_dtype_has_a_declared_instance() {
    let used = families_in_use();
    let orphans: Vec<&str> = FAMILIES
        .iter()
        .copied()
        .filter(|f| !used.contains(f))
        .collect();
    assert_eq!(
        orphans,
        Vec::<&str>::new(),
        "every dtype in the closed set has at least one declared instance — a \
         member with none is a claim about the future, not a description: \
         {orphans:?}"
    );

    // …and the other direction, which is what makes the set CLOSED rather than
    // merely non-empty: nothing in use is missing from the census list, so a
    // twelfth member cannot arrive without being added to `FAMILIES` where it is
    // visible.
    let unlisted: Vec<&str> = used
        .iter()
        .copied()
        .filter(|f| !FAMILIES.contains(f))
        .collect();
    assert_eq!(
        unlisted,
        Vec::<&str>::new(),
        "a dtype in use and not in the census is a set that is closed only by \
         accident: {unlisted:?}"
    );
}

/// **EVERY DECLARED OPERATION IS A PATH THIS PRODUCER ACTUALLY SERVES.**
///
/// A declaration that named a path the router does not know would be a contract
/// for an operation that does not exist — the taxonomy drifting off the surface
/// it is supposed to describe. `WS_METHODS` is the closed inventory (its own test
/// pins it against the dispatch arms), so this ties one to the other.
///
/// **THE MUTATION.** In `operations.rs`, change `Send`'s path to
/// `"threads/{thread_id}/sends"`.
///
/// Forced red:
/// ```text
/// every declared operation names a path the router serves:
/// ["threads/{thread_id}/sends"]
/// ```
#[test]
fn every_declared_operation_is_a_real_method_path() {
    let strays: Vec<&str> = DECLARED
        .iter()
        .map(|d| d.path)
        .filter(|p| !WS_METHODS.contains(p))
        .collect();
    assert_eq!(
        strays,
        Vec::<&str>::new(),
        "every declared operation names a path the router serves: {strays:?}"
    );

    // And the gap is stated rather than left to be discovered: this is a SLICE
    // of the surface, not a description of it.
    assert!(
        DECLARED.len() < WS_METHODS.len(),
        "if this ever becomes equality the module doc's `twelve of fifty` is \
         stale and should be deleted"
    );
}

/// **THE PATH SUBJECT IS DECLARED AS A PARAMETER, AND IT IS A `ref`.**
///
/// `shapewire::server::bind_subject` writes a path segment back into `params`
/// before a handler ever sees it, so `thread_id` on `threads/{thread_id}/send` IS
/// a payload parameter — it simply arrives through the path. A declaration that
/// left subjects out would describe every keyed operation as taking one fewer
/// value than it takes.
///
/// And it is a `ref` rather than a `scalar`: it names something in this
/// producer's address space. `reconcile_transcript` is the operation where the
/// two sit side by side and the difference is visible — `thread_id` is a `ref`
/// this producer resolves, `path` is a `Path` it cannot.
///
/// **THE MUTATION.** In `operations.rs`, declare `ReconcileTranscript`'s `path`
/// as `Ref("transcript")` — the collapse the split exists to prevent.
///
/// Forced red:
/// ```text
/// a filesystem path is not a ref: nothing in this producer resolves it, and a
/// consumer told `ref` would construct one out of an id
/// ```
#[test]
fn a_path_subject_is_a_declared_ref_and_a_filesystem_path_is_not() {
    for op in DECLARED {
        // Every `{segment}` in the path must appear as a declared parameter.
        for seg in op.path.split('/') {
            let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
                continue;
            };
            let p = op
                .takes
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{}: the subject `{name}` is undeclared", op.path));
            assert!(
                matches!(p.dtype, DType::Ref(_)),
                "{}: a path subject addresses something in this producer, so it \
                 is a `ref` and not a `{}`",
                op.path,
                p.dtype
            );
            assert_eq!(
                p.presence,
                Presence::Required,
                "{}: a subject is part of the address, so it cannot be optional",
                op.path
            );
        }
    }

    let recon = DECLARED
        .iter()
        .find(|d| d.path == "threads/{thread_id}/reconcile_transcript")
        .expect("the operation that split `ref` from `path`");
    let path_param = recon.takes.iter().find(|p| p.name == "path").unwrap();
    assert!(
        matches!(path_param.dtype, DType::Path),
        "a filesystem path is not a ref: nothing in this producer resolves it, \
         and a consumer told `ref` would construct one out of an id"
    );
}

/// **`Ceiling` IS ITS OWN PRESENCE, AND `shapes/{shape}/subscribe` IS WHY.**
///
/// Rule 2.8's second clause — *"asking nothing means the ceiling"* — is not the
/// same statement as *"this parameter is optional"*. A consumer reading
/// `Optional` learns it may leave a field out; a consumer reading `Ceiling` learns
/// that leaving it out is an ASK, for the most the producer serves. Collapsing
/// them would be the two-facts-one-spelling class, and this operation carries
/// three of them at once — `detail`, `retain` and the `beat` this dispatch added.
///
/// **THE MUTATION.** In `operations.rs`, declare `Subscribe`'s three knobs
/// `Optional`.
///
/// Forced red:
/// ```text
/// `detail`, `retain` and `beat` are asks with a ceiling, not optional fields —
/// absent means the MOST the producer serves: 0
/// ```
#[test]
fn asking_nothing_means_the_ceiling_is_its_own_declared_presence() {
    let sub = DECLARED
        .iter()
        .find(|d| d.path == "shapes/{shape}/subscribe")
        .expect("the subscribe declaration");
    let ceilings: Vec<&str> = sub
        .takes
        .iter()
        .filter(|p| p.presence == Presence::Ceiling)
        .map(|p| p.name)
        .collect();
    assert_eq!(
        ceilings.len(),
        3,
        "`detail`, `retain` and `beat` are asks with a ceiling, not optional \
         fields — absent means the MOST the producer serves: {}",
        ceilings.len()
    );
    assert!(
        ceilings.contains(&"beat"),
        "the rate knob is one of them, and it is the one where the default is \
         EXPENSIVE: {ceilings:?}"
    );
}
