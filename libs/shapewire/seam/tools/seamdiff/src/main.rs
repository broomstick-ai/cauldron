//! `seamdiff` — **IS THIS COPY OF THE SEAM THE REFERENCE, OR HAS IT DRIFTED?**
//!
//! ```text
//! seamdiff --reference <dir> --copy <dir> [--inventory <inventory.json>] [--strays]
//! ```
//!
//! Exit 0 when the copy is the reference outside its fences, every fence id is
//! sanctioned, the fence sequence is the reference's and the budget holds.
//! Non-zero and a named finding per line otherwise.
//!
//! `--reference` defaults to the directory this binary's source lives beside,
//! so running it from the chassis repo with only a `--copy` is the common case.

use std::path::{Path, PathBuf};

use seamdiff::*;

fn main() {
    let mut reference = PathBuf::from(
        std::env::var("SEAM_REFERENCE").unwrap_or_else(|_| {
            // `tools/seamdiff` → the seam directory two levels up.
            concat!(env!("CARGO_MANIFEST_DIR"), "/../..").to_string()
        }),
    );
    let mut copy: Option<PathBuf> = None;
    let mut inventory: Option<PathBuf> = None;
    let mut strays = false;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--reference" => {
                reference = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--copy" => {
                copy = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--inventory" => {
                inventory = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--strays" => {
                strays = true;
                i += 1;
            }
            other => {
                eprintln!("seamdiff: unknown argument `{other}`");
                std::process::exit(2);
            }
        }
    }

    let mut findings: Vec<Finding> = Vec::new();

    if strays {
        findings.extend(stray_fences(&reference));
    }

    if let Some(copy) = &copy {
        let mut parsed: Vec<(String, Parsed)> = Vec::new();
        for file in FILES {
            let r = read(&reference, file, "reference", &mut findings);
            let c = read(copy, file, "copy", &mut findings);
            let (Some(r), Some(c)) = (r, c) else { continue };
            let (p, f) = compare_file(file, &r, &c);
            findings.extend(f);
            parsed.push((file.to_string(), p));
        }
        if let Some(b) = budget(&parsed) {
            findings.push(b);
        }

        if let Some(inv_path) = &inventory {
            match std::fs::read_to_string(inv_path).map_err(|e| e.to_string()).and_then(|t| parse_inventory(&t)) {
                Err(e) => {
                    eprintln!("seamdiff: {}: {e}", inv_path.display());
                    std::process::exit(2);
                }
                Ok(inv) => {
                    let commands = read_or_empty(copy, "commands.rs");
                    let held = read_or_empty(copy, "held.rs");
                    let mod_rs = read_or_empty(copy, "mod.rs");
                    let templates = scan_templates(&commands, "TEMPLATES");
                    // A `held.rs` const whose value the producer does not serve
                    // as a shape is a KEY (`fleet`, `link`), not a shape. Filter
                    // against the producer's own list rather than a hardcoded
                    // one, so a face that adds a shape IS checked on it and a
                    // face that adds a key is not falsely accused.
                    let served: Vec<String> =
                        inv.shapes.iter().map(|s| s.name.clone()).collect();
                    let shapes: Vec<String> = scan_shapes(&held)
                        .into_iter()
                        .filter(|s| s.contains('/') || served.contains(s))
                        .collect();
                    // Per shape (rule 2.6 is per shape). An unreadable table is
                    // version 0 everywhere, which no producer serves: RED, not
                    // a vacuous green.
                    let contract = contracts_of(&mod_rs);
                    let stamp = scan_const(&mod_rs, "SHAPE_VERSION").unwrap_or_default();
                    findings.extend(check_inventory(&inv, &templates, &shapes, &contract, &stamp));
                    // THE CELL GUARD: every cell `view.rs` reads, every word it
                    // compares against and every field it gets, routed against
                    // the producer's manifest — `shapewire::cell`.
                    //
                    // **IT IS NO LONGER THE MECHANISM, AND IT NO LONGER FINDS
                    // ANYTHING IN THE REFERENCE.** Those three scanners look for
                    // LITERALS, and the reference's reads are now sealed tokens
                    // out of `inventory.rs` — so the compiler refuses an
                    // undeclared cell, word or field before this tool is run.
                    // The guard is kept and still wired because it still reddens
                    // on any copy that writes a read the old way, and because a
                    // checker that is correct costs nothing to keep. It is a
                    // SECOND WITNESS now; `check_generated` below is the one
                    // doing work the compiler cannot.
                    let view = read_or_empty(copy, "view.rs");
                    findings.extend(check_cells(
                        &inv,
                        "view.rs",
                        &scan_cell_reads(&view),
                        &scan_shape_consts(&held),
                        &scan_words(&view),
                        &scan_field_reads(&view),
                    ));
                    // THE GENERATED INVENTORY: is the Rust the consumer COMPILES
                    // against the same surface as the JSON a reviewer read? One
                    // run of the generator writes both, so agreement is the
                    // default and disagreement is a hand-edit. This is the half
                    // no compile-time check can reach, because the JSON is not a
                    // build input of anything.
                    let generated = read_or_empty(copy, "inventory.rs");
                    match parse_generated(&generated) {
                        Err(why) => findings.push(Finding::GeneratedMissing { why }),
                        Ok(g) => findings.extend(check_generated(&inv, &g)),
                    }
                }
            }
        }
    } else if !strays {
        eprintln!("seamdiff: nothing asked for; pass --copy <dir> and/or --strays");
        std::process::exit(2);
    }

    if findings.is_empty() {
        println!("seamdiff: ok");
        if copy.is_some() {
            println!("  the copy is the reference outside its fences");
        }
        if strays {
            println!(
                "  no SEAM-DIVERGE marker outside the seven seam files \
                 ({} path(s) excluded as self-witnessing)",
                SELF_EXCLUDED.len()
            );
        }
        return;
    }

    eprintln!("seamdiff: {} finding(s)\n", findings.len());
    for f in &findings {
        eprintln!("  {f}\n");
    }
    std::process::exit(1);
}

fn read(
    dir: &Path,
    file: &str,
    side: &str,
    findings: &mut Vec<Finding>,
) -> Option<String> {
    match std::fs::read_to_string(dir.join(file)) {
        Ok(t) => Some(t),
        Err(_) => {
            findings.push(Finding::FileMissing {
                file: file.to_string(),
                side: side.to_string(),
            });
            None
        }
    }
}

fn read_or_empty(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap_or_default()
}
