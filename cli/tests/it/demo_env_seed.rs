//! REL-3.S1: `cauldron env create cauldron-build` seeds the curated dataset
//! into the environment's own seed (the face's read path), and every other
//! name is created EMPTY.
//!
//! Nothing here touches `~/.cauldron` or a live process: `HOME` is a fresh temp
//! dir, `CAULDRON_HOME`/`CAULDRON_ENV`/`ENV` are removed from the child's
//! environment, and `env create` starts nothing.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_cauldron");

struct Home(PathBuf);

impl Home {
    fn new(tag: &str) -> Home {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let p = std::env::temp_dir().join(format!("cauldron-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Home(p)
    }
    fn seed(&self, env: &str) -> PathBuf {
        self.0.join(".cauldron").join("data").join(env).join("tui").join("soup").join("seed")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn create(home: &Path, name: &str) -> (i32, String) {
    let out = Command::new(BIN)
        .env("HOME", home)
        .env_remove("CAULDRON_HOME")
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .args(["env", "create", name])
        .output()
        .expect("spawn cauldron");
    (out.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn env_create_cauldron_build_seeds_the_curated_dataset() {
    let h = Home::new("rel3-seed");
    let (rc, out) = create(&h.0, "cauldron-build");
    assert_eq!(rc, 0, "{out}");
    let seed = h.seed("cauldron-build");
    for f in ["clusters.json", "artifacts.json", "ledger.json"] {
        assert!(seed.join(f).is_file(), "{f} missing after create: {out}");
    }
    let installed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(seed.join("clusters.json")).unwrap()).unwrap();
    let shipped: serde_json::Value = serde_json::from_str(soup::demo::CLUSTERS.text).unwrap();
    assert_eq!(installed, shipped, "the installed clusters are not the soup crate's");
    assert!(out.contains("seeded `cauldron-build`"), "{out}");
}

/// Opposes the test above: a fresh environment named anything else starts
/// empty — no seed directory, no face data at all.
#[test]
fn env_create_of_any_other_name_starts_empty() {
    let h = Home::new("rel3-empty");
    let (rc, out) = create(&h.0, "my-first");
    assert_eq!(rc, 0, "{out}");
    assert!(!h.seed("my-first").exists(), "a fresh environment was seeded: {out}");
    assert!(!h.0.join(".cauldron/data/my-first/tui").exists(), "a fresh environment has face data: {out}");
    assert!(!out.contains("seeded"), "{out}");
}
