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

/// A sandbox profile that denies every read under `dirs` (and allows the
/// rest), for `sandbox-exec -p`.
#[cfg(target_os = "macos")]
fn deny_reads(dirs: &[&Path]) -> String {
    let mut p = String::from("(version 1)(allow default)");
    for d in dirs {
        let d = d.canonicalize().unwrap_or_else(|e| panic!("{}: {e}", d.display()));
        p.push_str(&format!("(deny file-read* (subpath \"{}\"))", d.display()));
    }
    p
}

/// V1.4.S2b — **an INSTALLED `cauldron` seeds the demo.** The built binary
/// is COPIED to `<HOME>/.cauldron/bin/cauldron` (the install layout) and run
/// from there, cwd the scratch HOME, under `sandbox-exec` with every read of
/// this source tree and of the build slot DENIED: `env create
/// cauldron-build` must still write the soup crate's dataset, byte for byte,
/// so nothing it installs can come from a path resolved against
/// `CARGO_MANIFEST_DIR`, an include path or the build dir at run time.
/// Red: the seeding hook skipped (`envs.rs`), or the dataset read at run
/// time from the source tree. Sentinels: the profile really denies the tree
/// (`cat` of this crate's Cargo.toml fails under it) and really allows the
/// scratch. Opposed by `env_create_of_any_other_name_starts_empty`.
#[cfg(target_os = "macos")]
#[test]
fn an_installed_copy_seeds_cauldron_build_with_the_source_tree_unreadable() {
    let h = Home::new("v14s2b-installed");
    let bin_dir = h.0.join(".cauldron").join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let copy = bin_dir.join("cauldron");
    std::fs::copy(BIN, &copy).unwrap();

    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tree = crate_dir.join("..");
    // The test binary runs from the build slot: `<slot>/<profile>/deps/it-…`.
    let slot = std::env::current_exe().unwrap().ancestors().nth(3).unwrap().to_path_buf();
    let profile = deny_reads(&[&tree, &slot]);
    let sb = || {
        let mut c = Command::new("/usr/bin/sandbox-exec");
        c.arg("-p").arg(&profile);
        c
    };
    // Sentinels: the profile denies the tree and allows the scratch.
    assert!(!sb().arg("/bin/cat").arg(crate_dir.join("Cargo.toml")).output().unwrap().status.success(), "the sandbox let the binary read the source tree");
    let probe = h.0.join("probe.txt");
    std::fs::write(&probe, "x").unwrap();
    assert!(sb().arg("/bin/cat").arg(&probe).output().unwrap().status.success(), "the sandbox denies the scratch too");

    let out = sb()
        .arg(&copy)
        .args(["env", "create", soup::demo::ENV])
        .current_dir(&h.0)
        .env("HOME", &h.0)
        .env_remove("CAULDRON_HOME")
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .output()
        .expect("spawn the installed cauldron");
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{said}");
    let seed = h.seed(soup::demo::ENV);
    assert_eq!(std::fs::read_to_string(seed.join("ledger.json")).ok().as_deref(), Some(soup::demo::LEDGER.text), "the installed ledger is not the soup crate's: {said}");
    let installed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(seed.join("clusters.json")).unwrap()).unwrap();
    let shipped: serde_json::Value = serde_json::from_str(soup::demo::CLUSTERS.text).unwrap();
    assert_eq!(installed, shipped, "the installed clusters are not the soup crate's");
    for f in soup::demo::FILES {
        let name = f.name.strip_prefix("files/").unwrap();
        assert_eq!(std::fs::read_to_string(seed.join("files").join(name)).ok().as_deref(), Some(f.text), "files/{name}");
    }
}
