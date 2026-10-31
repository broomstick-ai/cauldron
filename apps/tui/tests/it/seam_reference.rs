//! # **THIS FACE'S SEAM IS THE REFERENCE, OUTSIDE ITS FENCES.**
//!
//! `docs/consumer-seam.md` §7.1, and `vendor-chassis.sh` prints it on every
//! vendor bump: *"each face's `src/seam/` is a COPY of `libs/shapewire/seam/*.rs`,
//! checked by `tests/it/seam_reference.rs`, which `include!`s
//! `seam/tools/seamdiff-engine.rs`."* **Seven files now, not six:**
//! `inventory.rs` joined them in this vendor bump.
//!
//! **Why the engine is `include!`d rather than linked.** The reference is DATA
//! FOR A TEST, never a `use` — that is the rule that keeps "copied, not shared"
//! honest, and it applies to the guard as much as to the six files it guards. A
//! `libs/shapewire/seam` crate this test depended on would be the shared backend
//! the whole design refused; a file compiled into this test is a copy that
//! `VENDORED`'s digest covers.
//!
//! ## What goes red here, and what it means
//!
//! | finding | what somebody did |
//! |---|---|
//! | the text outside the fences differs | a line of the seam was edited HERE instead of in the reference. The fix is upstream, then a vendor bump; `docs/consumer-seam.md` §7.1 calls that the correct direction of pressure |
//! | the fence ids differ | a fence the reference declares and this copy dropped — this face took the reference's answer where it was supposed to give its own — or a fence this copy invented, which is a divergence nobody ruled on |
//! | an id not in `D1…D6` | a divergence outside §5's table |
//! | the budget | more than 60 CODE lines inside fences across the whole copy — 59 is the last green. Comments are free, deliberately: a divergence has to be allowed to explain itself |
//! | `inventory.rs` and `inventory.json` disagree | the GENERATED half and the committed half are not the same surface. `check_generated` is the half of the inventory check a byte-diff cannot do: a face that vendored a stale PAIR diffs clean against its own json and is still behind the producer |
//! | a stray fence | a `SEAM-DIVERGE` marker in a seventh file, where nothing diffs, so nothing checks it |
//!
//! ## And the inventory, which is the half about the PRODUCER
//!
//! `commands::methods::TEMPLATES`, the `held.rs` shape consts, the declared
//! contract version and `SHAPE_VERSION` are routed against `inventory.json` —
//! the producer's own surface, a committed artifact of the chassis repo taken at
//! the same commit the crate was vendored from. This is what caught
//! `thread.replace` and `thread.unpend` in the AUI: both were in that face, the
//! daemon answers `unknown method` to both, and nothing was checking. Both are
//! deleted from all three seam copies now, on the operator's ruling
//! (*"kill it"*), along with the `pending` status word their two Options rows
//! were gated on — a word no producer on this wire has ever sent. **This face
//! never had either one**, and `src/sends.rs` is where that stays true.
//!
//! What this test caught HERE is the thing it exists for: the six READS this
//! face used to issue — `threads/list`, `threads/status`,
//! `threads/{id}/get`, `/history`, `/queue`, `/queue_failed` — cannot be
//! smuggled back in, because `commands::methods::TEMPLATES` is the whole of
//! what the seam can say and `src/methods.rs`, the file that used to name
//! them, no longer exists.

use std::path::{Path, PathBuf};

// CAULDRON (R7.S1b): here `libs/shapewire` is the SOURCE, not a vendored copy,
// so the engine is read where the source keeps it — the `seamdiff` crate — and
// not at the flattened `seam/tools/seamdiff-engine.rs` a vendor bump writes.
include!("../../../../libs/shapewire/seam/tools/seamdiff/src/engine.rs");

fn repo() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `apps/tui`; the repo root is two up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("apps/tui is two levels under the repo root")
        .to_path_buf()
}

fn reference_dir() -> PathBuf {
    repo().join("libs/shapewire/seam")
}

fn copy_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/seam")
}

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file))
        .unwrap_or_else(|e| panic!("{}: {e}", dir.join(file).display()))
}

/// **THE COPY IS THE REFERENCE OUTSIDE ITS FENCES**, file by file, so a failure
/// names the file and the first line that differs rather than "something moved".
#[test]
fn the_seam_is_the_reference_outside_its_fences() {
    let (rd, cd) = (reference_dir(), copy_dir());
    let mut findings = Vec::new();
    let mut parsed = Vec::new();
    for file in FILES {
        let (p, f) = compare_file(file, &read(&rd, file), &read(&cd, file));
        parsed.push((file.to_string(), p));
        findings.extend(f);
    }
    if let Some(over) = budget(&parsed) {
        findings.push(over);
    }
    assert!(
        findings.is_empty(),
        "{} finding(s) against the reference at {}:\n\n{}",
        findings.len(),
        rd.display(),
        findings
            .iter()
            .map(|f| format!("{f:?}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    // The spend, printed rather than asserted at a number: the budget is the
    // ceiling and §5's table is the argument, so a number here would be a second
    // place to update when the table changes.
    let spend: usize = parsed
        .iter()
        .flat_map(|(_, p)| p.fences.iter())
        .map(|f| f.code_lines())
        .sum();
    println!("this face spends {spend} of {BUDGET} code lines inside fences");
    for (file, p) in &parsed {
        for f in &p.fences {
            println!("  {file:14} {}  {} code line(s)", f.id, f.code_lines());
        }
    }
}

/// **NO `SEAM-DIVERGE` MARKER OUTSIDE THE SEVEN.** A fence in an eighth file is
/// a divergence nothing compares, because the comparison is per-file against the
/// reference and there is no eighth file to compare it to.
///
/// It was SIX until this vendor bump. `inventory.rs` is the seventh and it is
/// GENERATED, so it has no fence and never will — which is exactly why it is in
/// `FILES`: a face that hand-edited it would be compiling against its own opinion
/// of what the producer serves, and the byte-diff is the only thing that says so.
#[test]
fn no_fence_lives_outside_the_seam() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    // The SEVEN are compared against the reference file by file; two of them are
    // legitimately fenced. A stray is a marker anywhere else — an eighth file
    // in `src/seam/`, or a fence somebody wrote in `src/mirror.rs` thinking
    // it would be honoured — where nothing compares it to anything.
    let six: Vec<String> = FILES.iter().map(|f| format!("seam/{f}")).collect();
    let strays: Vec<Finding> = stray_fences(&root)
        .into_iter()
        .filter(|f| match f {
            Finding::StrayFence { path, .. } => !six.contains(path),
            _ => true,
        })
        .collect();
    // And the exclusion is proved NON-VACUOUS rather than asserted: an unfiltered
    // walk must find only files in the seven, or this test is protecting a set
    // that does not exist. It does NOT require all seven to carry one —
    // `inventory.rs` is generated and never will.
    let all: Vec<String> = stray_fences(&root)
        .into_iter()
        .filter_map(|f| match f {
            Finding::StrayFence { path, .. } => Some(path),
            _ => None,
        })
        .collect();
    assert!(
        !all.is_empty(),
        "an unfiltered walk of {} found no fence at all, so the filter below \
         excludes nothing and this test can never fail",
        root.display()
    );
    for p in &all {
        assert!(
            six.contains(p),
            "`{p}` carries a fence and is not one of the seven — see the filter above"
        );
    }
    assert!(
        strays.is_empty(),
        "{} stray fence(s) under {}:\n{}",
        strays.len(),
        root.display(),
        strays
            .iter()
            .map(|f| format!("{f:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// **EVERY METHOD AND EVERY SHAPE THE SEAM NAMES IS ONE THE PRODUCER SERVES.**
///
/// The half that is about the daemon rather than about the other copy, and the
/// one that caught the two dot-form names nothing answers.
#[test]
fn the_seams_surface_routes_against_the_producers_own_inventory() {
    let inv_text = std::fs::read_to_string(repo().join("libs/shapewire/seam/inventory.json"))
        .expect("inventory.json is vendored beside the crate");
    let inv = parse_inventory(&inv_text).expect("inventory.json parses");

    let cd = copy_dir();
    let commands = read(&cd, "commands.rs");
    let held = read(&cd, "held.rs");
    let mod_rs = read(&cd, "mod.rs");

    let templates = scan_templates(&commands, "TEMPLATES");
    assert!(
        !templates.is_empty(),
        "the seam declares no method templates, so this check would pass on an \
         empty list — which is the shape of a guard that guards nothing"
    );
    // **A KEY IS NOT A SHAPE.** `held.rs` declares `FLEET` and `LINK` as
    // `pub const … : &str`, and `scan_shapes` cannot tell them from a shape name
    // by shape alone — so filter against the producer's OWN list rather than a
    // hardcoded one, which is what `seamdiff`'s CLI does and why: a face that
    // adds a shape IS checked on it, and a face that adds a key is not falsely
    // accused.
    let served: Vec<String> = inv.shapes.iter().map(|s| s.name.clone()).collect();
    let shapes: Vec<String> = scan_shapes(&held)
        .into_iter()
        .filter(|s| s.contains('/') || served.contains(s))
        .collect();
    assert!(
        shapes.len() >= 5,
        "only {} shape(s) survived the key filter; the book names seven of the \
         producer's eight and a filter that dropped them would make the check \
         below vacuous: {shapes:?}",
        shapes.len()
    );
    // **PER SHAPE since R2.S8** (`SHAPE_CONTRACT_VERSIONS` + `SHAPE_CONTRACT_DEFAULT`):
    // `accounts` is at 2 and the rest at 1, so one integer can no longer be right.
    // `scan_contracts` and not `contracts_of`: an unreadable table is a panic HERE,
    // naming itself, rather than version 0 for every shape further down.
    let contracts: Contracts =
        scan_contracts(&mod_rs).expect("the seam declares a per-shape contract table");
    assert!(
        !contracts.per_shape.is_empty(),
        "the contract table has no rows, so every shape is checked at the default \
         and a per-shape mismatch could not be seen"
    );
    let stamp = scan_const(&mod_rs, "SHAPE_VERSION").expect("the seam declares a vendor stamp");

    let findings = check_inventory(&inv, &templates, &shapes, &contracts, &stamp);
    assert!(
        findings.is_empty(),
        "{} finding(s) against the producer's surface ({} methods, {} shapes, taken \
         at {}):\n\n{}",
        findings.len(),
        inv.methods.len(),
        inv.shapes.len(),
        inv.source_commit,
        findings
            .iter()
            .map(|f| format!("{f:?}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    println!(
        "{} template(s) and {} shape(s) route against the producer's {} methods",
        templates.len(),
        shapes.len(),
        inv.methods.len()
    );
}

/// **THE GENERATED HALF AND THE COMMITTED HALF ARE ONE SURFACE**, and this is the
/// check a byte-diff against the reference cannot do.
///
/// `inventory.rs` is the half the COMPILER reads and `inventory.json` is the half
/// the guard reads. `the_seam_is_the_reference_outside_its_fences` proves this
/// copy's `inventory.rs` is the reference's; this proves it is the same surface as
/// the `inventory.json` sitting beside it. A face that vendored a STALE PAIR —
/// both files consistent, both behind the producer — satisfies the byte-diff
/// perfectly and fails here only if the pair itself disagrees, so the two halves
/// are complementary and neither subsumes the other. `vendor-chassis.sh` is the
/// third: it refuses a bump whose producer surface has moved.
///
/// **Only a re-vendored tree can run this at all.** `check_generated`,
/// `parse_generated` and `Generated` arrived with the seventh file; before it
/// there was no Rust surface to check.
#[test]
fn the_generated_surface_and_the_committed_inventory_are_the_same_surface() {
    let inv_text = std::fs::read_to_string(repo().join("libs/shapewire/seam/inventory.json"))
        .expect("inventory.json is vendored beside the crate");
    let inv = parse_inventory(&inv_text).expect("inventory.json parses");
    let gen = parse_generated(&read(&copy_dir(), "inventory.rs"))
        .expect("this face's generated inventory parses");

    // **NON-VACUOUS FIRST.** A `parse_generated` that silently matched nothing
    // would make every finding below impossible, which is the shape of a probe
    // that cannot fail. Both sides must carry a surface before the comparison
    // between them means anything.
    assert!(
        !gen.methods.is_empty() && !gen.shapes.is_empty(),
        "the generated inventory parsed to {} method(s) and {} shape(s) — a scan \
         that found nothing cannot disagree with anything",
        gen.methods.len(),
        gen.shapes.len()
    );
    assert!(
        !inv.methods.is_empty() && !inv.shapes.is_empty(),
        "inventory.json parsed to nothing"
    );

    let findings = check_generated(&inv, &gen);
    assert!(
        findings.is_empty(),
        "{} finding(s) between this face's `inventory.rs` and the `inventory.json` \
         beside it:\n\n{}",
        findings.len(),
        findings
            .iter()
            .map(|f| format!("{f:?}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    println!(
        "the generated surface at {} carries {} method token(s) over {} shape(s)",
        gen.source_commit,
        gen.method_tokens.len(),
        gen.shapes.len()
    );
}

/// **THE INVENTORY NAMES ITS PRODUCER'S REPO AND COMMIT, AND THE SEAM DECLARES
/// THAT COMMIT.**
///
/// CAULDRON (R7.S1b): in harness this test also read `libs/shapewire/VENDORED`,
/// the stamp a vendor bump writes, to prove the reference was not a stale copy.
/// In this repo `libs/shapewire` is the chassis's own source — there is no
/// vendor act and no stamp. The half that still means something is the pair
/// that goes on the wire, below.
///
/// CAULDRON (R7.S1e): **the commit is a SUBSTRATE commit, not a cauldron one,
/// and the inventory says so.** `inventory.json` was taken while threads still
/// lived in `substrate-interface-layer`, and its `source_repo` field records
/// that: `a968e07` there is "threads: a thread's message history is carried as
/// resource pages … (R2.S3d C)". It is NOT reachable from any cauldron branch.
/// (The object happens to sit unreachable in cauldron's object store, so a
/// `git cat-file` here would pass and prove nothing.) This test does not reach
/// into the substrate checkout to resolve it — a test that reads a sibling
/// checkout is the outside-the-tree dependency `scripts/check-tree-paths`
/// guards against. What it DOES check: the inventory names a known producer
/// repo, and when that repo is THIS one (once the inventory is re-taken against
/// `services/threads`, which now lives here), the commit must be an ancestor of
/// HEAD — checked with git, so a stray unreachable object cannot satisfy it.
#[test]
fn the_inventory_names_its_producer_repo_and_commit_and_the_seam_declares_it() {
    // **THE INVENTORY HAS ITS OWN COMMIT, AND THAT IS DELIBERATE, NOT DRIFT.**
    // `inventory.json` is a COMMITTED ARTIFACT of the chassis repo, written by
    // `make-inventory.py` against a running daemon, and the vendor script COPIES
    // it rather than regenerating it — so a reviewer saw it land in a diff. It
    // therefore names the commit it was TAKEN at, which is at or behind the
    // commit the crate was vendored from. What must agree is the seam's declared
    // `SHAPE_VERSION` and the inventory, because that is the pair a consumer puts
    // on the wire: `check_inventory` asserts it above, and this states the
    // relationship so the asymmetry is not read as a bug next time.
    let inv_text = std::fs::read_to_string(repo().join("libs/shapewire/seam/inventory.json"))
        .expect("inventory.json is vendored");
    let inv = parse_inventory(&inv_text).expect("inventory.json parses");
    assert!(
        inv.source_commit.len() == 40 && inv.source_commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "inventory.json's source_commit `{}` is not a full commit id",
        inv.source_commit
    );
    let raw: serde_json::Value = serde_json::from_str(&inv_text).expect("inventory.json is JSON");
    let source_repo = raw["source_repo"].as_str().unwrap_or("");
    match source_repo {
        // Taken while threads lived in substrate. Not resolvable from this tree
        // without reading a sibling checkout; see the doc comment.
        "substrate-interface-layer" => {}
        // Re-taken here: the commit must be in THIS repo's history.
        "cauldron" => {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(repo())
                .args(["merge-base", "--is-ancestor", &inv.source_commit, "HEAD"])
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "inventory.json says it was taken from cauldron at `{}`, but that commit \
                 is not an ancestor of HEAD here (git merge-base rc={:?}: {})",
                inv.source_commit,
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        other => panic!(
            "inventory.json names producer repo `{other}`; expected \
             `substrate-interface-layer` (where it was taken) or `cauldron` (where \
             threads lives now)"
        ),
    }
    let declared = scan_const(&read(&copy_dir(), "mod.rs"), "SHAPE_VERSION")
        .expect("the seam declares a vendor stamp");
    assert!(
        inv.source_commit
            .starts_with(declared.trim_start_matches("shapewire@")),
        "the seam says it was built against `{declared}` and the inventory was \
         taken at `{}` — the pair that goes on the wire as `consumer.shape_version` \
         must name a commit somebody can check out",
        inv.source_commit
    );
    println!(
        "inventory @ {source_repo} {} ({} methods, {} shapes); seam declares {declared}",
        inv.source_commit,
        inv.methods.len(),
        inv.shapes.len()
    );
}
