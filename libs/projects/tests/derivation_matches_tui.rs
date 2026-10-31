//! **THE DERIVATION MUST REPRODUCE TODAY'S LIST EXACTLY.**
//!
//! A derivation that differs by one name is a silent project loss: the day the
//! registry lands, a project he has would stop existing in both faces while its
//! directory sat in `~/minds`. So this suite runs BOTH implementations over the
//! SAME real input and compares.
//!
//! The oracle below is a VERBATIM COPY of `apps/tui/src/index.rs:175-198`
//! (`discover_roots`), pasted here and not imported: `apps/tui` is a binary
//! crate with no library target, and this crate must not touch it. It is
//! copied so that the comparison is against the code that ships today rather
//! than against my own restatement of what it does. If `index.rs` changes, this
//! copy must be re-synced — that is the cost of the copy and it is noted where
//! anyone diffing the two will see it.
//!
//! **READ-ONLY, AND IT ASSERTS ITS OWN READ-ONLYNESS.** It runs against his
//! real `$HOME`, so it walks `~/.cauldron` before and after and fails if one
//! byte moved. `derive_from_minds` writes nothing by construction; this is the
//! witness, not the argument.
//!
//! `#[ignore]`d: it is a statement about ONE machine's `~/minds` and would be
//! meaningless anywhere else. Run it with
//! `cargo test -p cauldron --test derivation_matches_tui -- --ignored --nocapture`.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use projects::*;

// ──────────────────────────────────────────────────────────────────────────
//  THE ORACLE — apps/tui/src/index.rs:56, 57, 175-198, copied verbatim
// ──────────────────────────────────────────────────────────────────────────

const MINDS: &str = "minds";
const MISCELLANEOUS: &str = ".aui/miscellaneous";

fn discover_roots(home: &str) -> Vec<(String, PathBuf)> {
    let home = Path::new(home);
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(home.join(MINDS)) {
        for ent in rd.flatten() {
            let Some(name) = ent.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            match std::fs::metadata(ent.path()) {
                Ok(md) if md.is_dir() => out.push((name, ent.path())),
                _ => continue,
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.push(("miscellaneous".to_string(), home.join(MISCELLANEOUS)));
    out
}

// ──────────────────────────────────────────────────────────────────────────

#[test]
#[ignore = "reads his real ~/minds; meaningless on another machine"]
fn the_derivation_yields_exactly_the_list_the_tui_draws_today() {
    let home = home_dir().expect("a home directory");
    let cauldron_before = support::tree(&cauldron_dir(&home));
    let minds_before = dir_stamp(&minds_root(&home));

    // TODAY: the shipped face's own function, over the real directory.
    let today = discover_roots(home.to_str().unwrap());

    // TOMORROW: this crate's derivation, plus the synthesized bucket.
    let derived = Registry::derive_from_minds(&home, "conformance");
    let rows = derived.view(&home);

    // ── 1. THE NAMES AND THEIR ORDER, INCLUDING THE BUCKET LAST ──────────
    let today_names: Vec<&str> = today.iter().map(|(n, _)| n.as_str()).collect();
    let rows_names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        rows_names, today_names,
        "the registry's list differs from the list he sees today"
    );

    // A POSITIVE WITNESS, not just equality: both lists are non-trivial. Two
    // empty lists compare equal and would score a triumphant pass.
    assert!(
        today_names.len() >= 10,
        "the oracle found almost nothing ({}) — it did not run against a real ~/minds",
        today_names.len()
    );

    // ── 2. THE PROJECT COUNT IS THE LIST MINUS THE BUCKET ────────────────
    assert_eq!(
        derived.names().count(),
        today_names.len() - 1,
        "the bucket must not be counted among the projects"
    );

    // ── 3. EVERY ROW RESOLVES TO THE SAME DIRECTORY ──────────────────────
    // The paths are deliberately DIFFERENT strings — today's is the symlink
    // under the relic, the registry's is the canonical target — so the
    // assertion is that they are the same directory, canonicalized.
    let mut changed = 0usize;
    for ((name, old_path), row) in today.iter().zip(rows.iter()) {
        if row.bucket {
            assert_eq!(&row.path, old_path, "the bucket's root moved");
            continue;
        }
        let old_real = std::fs::canonicalize(old_path)
            .unwrap_or_else(|e| panic!("{name}: today's path does not resolve: {e}"));
        assert_eq!(
            row.path, old_real,
            "{name}: the registry resolves somewhere else"
        );
        assert!(
            row.path.is_absolute(),
            "{name}: a registry path must be absolute"
        );
        assert!(!row.missing, "{name}: drawn missing on day one");
        if &row.path != old_path {
            changed += 1;
        }
    }
    assert!(
        changed > 0,
        "no path was rewritten from the relic to its target — the symlinks were not followed"
    );

    // ── 4. NAMES ARE VERBATIM ────────────────────────────────────────────
    for r in rows.iter().filter(|r| !r.bucket) {
        assert!(
            !r.name.is_empty() && name_objection(&r.name).is_none(),
            "{}: unusable name",
            r.name
        );
    }

    eprintln!(
        "DERIVATION CONFORMANCE: {} projects + the bucket = {} rows; \
         {changed} paths rewritten from ~/minds to their targets",
        derived.names().count(),
        rows.len()
    );
    eprintln!("names: {}", today_names.join(" "));

    // ── 5. READ-ONLY, WITNESSED ──────────────────────────────────────────
    assert_eq!(
        support::tree(&cauldron_dir(&home)),
        cauldron_before,
        "this test wrote into ~/.cauldron"
    );
    assert_eq!(
        dir_stamp(&minds_root(&home)),
        minds_before,
        "this test wrote into ~/minds"
    );
    assert!(
        !registry_path(&home).exists(),
        "a real ~/.cauldron/projects.json exists — nothing is supposed to mint it yet"
    );
}

/// Every entry of a directory with its link target, without following it.
fn dir_stamp(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        let target = std::fs::read_link(ent.path())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<not a link>".to_string());
        out.insert(name, target);
    }
    out
}
