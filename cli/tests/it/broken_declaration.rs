//! **ONE BROKEN DECLARATION DOES NOT TAKE A BARE VERB DOWN.** The real binary,
//! under a sandbox `CAULDRON_HOME`, with one unparsable root declaration beside
//! one that loads: `cauldron status` names the broken one on stderr, lists the
//! good one, and exits 0. Naming the broken one explicitly still fails loudly.
//!
//! This is the test the inspector found missing: a `report_broken` that prints
//! nothing passed every unit test, because none of them ran a verb.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// **ONE ROOT PER CALL, BY CONSTRUCTION.** The root used to be named
/// `<pid>-<SystemTime nanos>`, and on macOS that clock moves in whole
/// MICROSECONDS (every value ends `000`), so this file's two tests — run in
/// parallel threads of one process — could take the same root, and the first
/// to finish `remove_dir_all`ed the other's declarations mid-run (R5.S1b
/// inspection F-B; forced in the R5.S1c report). The sequence number is what
/// makes a root unique within the process; pid and time only keep it apart
/// from other processes and earlier runs.
static SEQ: AtomicU64 = AtomicU64::new(0);

fn unique_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "cauldron-broken-decl-{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ))
}

fn sandbox() -> PathBuf {
    let root = unique_root();
    // EXCLUSIVE create: a root that already exists is a collision, and it
    // fails here, loudly, instead of two tests silently sharing fixtures.
    std::fs::create_dir(&root).unwrap_or_else(|e| panic!("sandbox root {} already taken: {e}", root.display()));
    let services = root.join("services");
    std::fs::create_dir_all(&services).unwrap();
    let shipped = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("services").join("tally.toml");
    let tally = std::fs::read_to_string(&shipped).unwrap();
    assert!(tally.contains("name        = \"tally\""), "fixture premise");
    std::fs::write(
        services.join("zz-good.toml"),
        tally.replacen("name        = \"tally\"", "name = \"zz-good\"", 1),
    )
    .unwrap();
    std::fs::write(services.join("zz-broken.toml"), "name = \"zz-broken\"\nthis is not toml ==\n").unwrap();
    root
}

fn cauldron(root: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_cauldron"))
        .args(args)
        .env("CAULDRON_HOME", root)
        .env("CAULDRON_ENV", "sandbox")
        .output()
        .expect("spawn cauldron");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn bare_status_reports_the_broken_one_and_answers_for_the_rest() {
    let root = sandbox();
    let (rc, out, err) = cauldron(&root, &["status"]);
    assert_eq!(rc, 0, "a bare verb survives one broken declaration; stderr: {err}");
    assert!(
        err.contains("skipping `zz-broken`") && err.contains("parsing the service declaration"),
        "the broken one is NAMED with its reason on stderr: {err}"
    );
    // POSITIVE SENTINEL over the same predicate: the good root declaration
    // and a shipped one are both answered for, so an empty table could not
    // pass as "survived".
    assert!(out.contains("zz-good"), "the loadable root declaration is listed: {out}");
    assert!(out.contains("tally"), "sentinel: a shipped declaration is listed: {out}");
    assert!(!out.contains("zz-broken"), "the broken one is not listed as a service: {out}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn naming_the_broken_one_explicitly_still_fails_loudly() {
    let root = sandbox();
    let (rc, _out, err) = cauldron(&root, &["status", "zz-broken"]);
    assert_ne!(rc, 0, "an explicitly named broken declaration is a failure, not a skip");
    assert!(err.contains("parsing the service declaration"), "{err}");
    assert!(!err.contains("skipping"), "named = refused, never skipped: {err}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The guard on [`unique_root`]: 16 threads released by one barrier, the
/// way the test harness starts this file's tests, never share a root. With
/// the old `<pid>-<nanos>` name they do (forced in the R5.S1c report).
#[test]
fn sandbox_roots_taken_at_the_same_instant_are_distinct() {
    const N: usize = 16;
    let gate = std::sync::Arc::new(std::sync::Barrier::new(N));
    let roots: Vec<PathBuf> = (0..N)
        .map(|_| {
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                unique_root()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    let distinct: std::collections::BTreeSet<_> = roots.iter().collect();
    assert_eq!(distinct.len(), N, "sandbox roots collided: {roots:#?}");
}
