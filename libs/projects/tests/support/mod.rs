//! Shared scaffolding for the registry suites.
//!
//! **EVERY TEST IN THIS CRATE RUNS AGAINST A SCRATCH ROOT.** Nothing here ever
//! names `$HOME`. The one suite that reads his real `~/minds`
//! (`derivation_matches_tui.rs`) is `#[ignore]`d, is read-only, and asserts its
//! own read-onlyness. See the crate docs, "THE ROOT IS A PARAMETER".

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// A scratch `$HOME` with `minds/` and `.cauldron/` absent until asked for.
pub fn scratch_home() -> TempDir {
    tempfile::Builder::new()
        .prefix("cauldron-home-")
        .tempdir()
        .expect("scratch home")
}

/// Build a `~/minds` full of symlinks, exactly the shape his real one has:
/// `minds/<name> -> <home>/<target>`, with the target directory created.
pub fn make_minds(home: &Path, links: &[(&str, &str)]) {
    let minds = home.join("minds");
    fs::create_dir_all(&minds).expect("minds");
    for (name, target) in links {
        let target_abs = home.join(target);
        fs::create_dir_all(&target_abs).expect("target");
        std::os::unix::fs::symlink(&target_abs, minds.join(name)).expect("symlink");
    }
}

/// A symlink into nothing — the dangling entry today's `discover_roots` cannot
/// see, because `metadata()` follows and fails.
pub fn make_dangling(home: &Path, name: &str) {
    let minds = home.join("minds");
    fs::create_dir_all(&minds).expect("minds");
    std::os::unix::fs::symlink(home.join("gone-forever"), minds.join(name)).expect("symlink");
}

/// The registry's bytes, for a byte-identity assertion.
pub fn registry_bytes(home: &Path) -> Vec<u8> {
    fs::read(projects::registry_path(home)).expect("registry should be readable")
}

/// Every path under `dir` with its length — the witness for "this call wrote
/// nothing anywhere", which an absence alone could never be.
pub fn tree(dir: &Path) -> BTreeMap<PathBuf, u64> {
    let mut out = BTreeMap::new();
    walk(dir, &mut out);
    out
}

fn walk(dir: &Path, out: &mut BTreeMap<PathBuf, u64>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let p = ent.path();
        let Ok(md) = fs::symlink_metadata(&p) else {
            continue;
        };
        out.insert(p.clone(), md.len());
        if md.is_dir() {
            walk(&p, out);
        }
    }
}

/// Names of the `projects.json.tmp-*` strays in `~/.cauldron`.
pub fn strays(home: &Path) -> Vec<String> {
    let dir = projects::cauldron_dir(home);
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut v: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.starts_with("projects.json.tmp-"))
        .collect();
    v.sort();
    v
}
