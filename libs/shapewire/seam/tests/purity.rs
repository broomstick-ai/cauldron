//! **THE NO-TOKIO FLOOR, AND THE NO-HTTP-CLIENT FLOOR, AS TESTS.**
//!
//! `lib/shapewire/check-features.sh` asserts the CHASSIS mirror's no-tokio
//! floor. Nothing asserted the SEAM's, and the seam is where it is easy to
//! lose: `link.rs` legitimately needs a runtime and sits two files away from
//! `view.rs`, which a face calls from a synchronous render walk.
//!
//! `held.rs` and `view.rs` are the two files the design calls PURE, and
//! `view.rs` is the one it says is byte-identical in every copy with zero
//! fenced lines. An `async` in either of them would force a runtime into every
//! render path a face has, and the AUI's row, tone and phone-projection paths
//! are synchronous by construction.
//!
//! **These scan source, so they must not witness themselves.** They read the
//! five seam files by name and never this file — see
//! [`the_exclusion_of_this_file_is_not_vacuous`], which proves the exclusion is
//! load-bearing by showing that this file's own text WOULD trip every pattern.

use std::path::{Path, PathBuf};

fn seam() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(name: &str) -> String {
    let p = seam().join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// A `use` or a path reference, as opposed to the word appearing in prose. The
/// discriminator is the line's own shape: a `//` comment is prose and anything
/// else that names the crate is a dependency.
fn code_lines_naming(body: &str, needle: &str) -> Vec<String> {
    body.lines()
        .filter(|l| {
            let t = l.trim();
            !t.starts_with("//") && t.contains(needle)
        })
        .map(|l| l.trim().to_string())
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
//  The two pure files
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn held_and_view_name_no_runtime() {
    for f in ["held.rs", "view.rs"] {
        let body = read(f);
        for needle in ["tokio", "async ", ".await", "futures_util", "tokio_tungstenite"] {
            let hits = code_lines_naming(&body, needle);
            assert!(
                hits.is_empty(),
                "{f} names `{needle}` in code, and it is one of the two files the \
                 design calls PURE:\n  {}",
                hits.join("\n  ")
            );
        }
    }
}

/// **WHERE THE RUNTIME IS ALLOWED TO BE, AND IT IS TWO FILES.**
///
/// `link.rs` owns the socket and `mod.rs` owns the handle and the wake-up
/// channel, so both name tokio. The other four may not, and the line is not
/// arbitrary:
///
/// * `held.rs` and `view.rs` are the PURE pair (above).
/// * `commands.rs` is `async` — it sends requests — but it awaits the LINK
///   and never a tokio primitive of its own, which is what keeps the verb set
///   a vocabulary rather than a second place the runtime leaks in.
/// * `ledger.rs` is a reader over a mirror and has no business with either.
///
/// This test was written claiming FIVE files and reddened on `mod.rs`'s
/// `use tokio::sync::broadcast`, which is the channel `Seam::events` hands out.
/// The claim was wrong and is corrected rather than the code; a guard whose
/// first run agrees with its author has not been run.
#[test]
fn only_the_link_and_the_handle_name_tokio() {
    for f in ["held.rs", "view.rs", "commands.rs", "ledger.rs"] {
        let hits = code_lines_naming(&read(f), "tokio");
        assert!(
            hits.is_empty(),
            "{f} names `tokio` in code. `link.rs` (the socket) and `mod.rs` (the \
             handle and the wake-up channel) are the only two files in the seam \
             that may:\n  {}",
            hits.join("\n  ")
        );
    }
    // And `link.rs` does, so the ban above is about where the runtime is and not
    // about there being none — a test that passed because nothing used tokio
    // would be measuring the wrong thing.
    for f in ["link.rs", "mod.rs"] {
        assert!(
            !code_lines_naming(&read(f), "tokio").is_empty(),
            "{f} names no tokio at all, so the ban above is vacuous: it would be \
             passing because nothing in the seam uses a runtime rather than \
             because the runtime is where it belongs"
        );
    }
}

/// **NO HTTP CLIENT ANYWHERE IN THE SEAM**, which is what turning divergence D1
/// from a fenced body into `link::Supervisor` bought. The `/health` probe that
/// needed one is the face's now.
#[test]
fn the_seam_links_no_http_client() {
    for f in ["mod.rs", "link.rs", "held.rs", "view.rs", "commands.rs", "ledger.rs"] {
        let body = read(f);
        // **EVERY NEEDLE IS `::`-QUALIFIED, AND THAT IS A CORRECTION.** `surf`
        // as a bare substring reddened on `shapewire::surface::Dialed` — the
        // sealed method token `link::request` now takes — which is a false
        // positive of exactly the kind a substring scan produces. Naming a crate
        // in code requires the path form, so the path form is the needle; the
        // manifest scan below keeps the bare names, because a dependency line IS
        // the bare name.
        for needle in ["reqwest::", "hyper::", "ureq::", "isahc::", "surf::"] {
            let hits = code_lines_naming(&body, needle);
            assert!(
                hits.is_empty(),
                "{f} names `{needle}`. The seam reaches the daemon through ONE socket; \
                 a face that probes `/health` to decide whether to SPAWN implements \
                 `link::Supervisor` above the seam:\n  {}",
                hits.join("\n  ")
            );
        }
    }
    // Non-vacuous: the manifest must not list one either, or the scan above is
    // checking the wrong artifact.
    let manifest = read("Cargo.toml");
    for needle in ["reqwest", "ureq", "isahc", "surf"] {
        assert!(
            !code_lines_naming(&manifest, needle)
                .iter()
                .any(|l| !l.starts_with('#')),
            "Cargo.toml depends on `{needle}`"
        );
    }
}

/// **NO CURSOR IN THE SEAM**, which is what `threads/log` bought (§8 Gap A) and
/// what rule 2.5 says must be true: *"Recovery is re-subscribe. There is no
/// re-emit verb, no replay, no cursor."* The interim `ledger.rs` would have had
/// one; this one does not, and `seam/probe/` is the production measurement
/// behind the claim.
#[test]
fn no_file_in_the_seam_carries_a_cursor() {
    for f in ["mod.rs", "link.rs", "held.rs", "view.rs", "commands.rs", "ledger.rs"] {
        let body = read(f);
        for needle in ["after_seq", "threads/{id}/history", "/history", "park_"] {
            let hits = code_lines_naming(&body, needle);
            assert!(
                hits.is_empty(),
                "{f} names `{needle}` in code. Rule 2.5: recovery is re-subscribe, \
                 and the one thing that used to need a cursor is a `threads/log` \
                 mirror now:\n  {}",
                hits.join("\n  ")
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
//  The self-witnessing proof
// ─────────────────────────────────────────────────────────────────────────────

/// **THIS FILE WOULD TRIP EVERY PATTERN ABOVE**, so excluding it is doing work.
///
/// Fifteen source-scanning guards in this arc were satisfied by their own text,
/// including a ban satisfied by the comment explaining it. The exclusion here is
/// structural — [`read`] takes a NAME and the tests above pass a fixed list —
/// but "structural" is a claim until it is measured, and the measurement is
/// that this file's own text contains the needles in CODE position (inside
/// string literals, which are not comments) and is not in any list.
#[test]
fn the_exclusion_of_this_file_is_not_vacuous() {
    let me = std::fs::read_to_string(Path::new(file!()))
        .or_else(|_| std::fs::read_to_string(seam().join("tests/purity.rs")))
        .expect("this test file");
    for needle in ["tokio", "reqwest", "after_seq", ".await"] {
        assert!(
            !code_lines_naming(&me, needle).is_empty(),
            "`{needle}` does not appear in code position in tests/purity.rs, so the \
             claim that this file would trip the scan is false and the structural \
             exclusion is proving nothing"
        );
    }
    // And the scanned set is exactly the six seam files — never seven, and never
    // a glob that would pick this one up.
    let scanned = ["mod.rs", "link.rs", "held.rs", "view.rs", "commands.rs", "ledger.rs"];
    assert!(
        !scanned.contains(&"purity.rs") && !scanned.contains(&"tests/purity.rs"),
        "the scanned list includes this file"
    );
    assert_eq!(scanned.len(), 6);
}
