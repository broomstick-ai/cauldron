//! **EVERY SERVICE IS REGISTERED, AND THE RESTART BACKOFF IS THE SCHEDULE IT
//! SAYS** (R5.S1).
//!
//! 1. The backoff schedule: immediate first relaunch, doubling, capped, and
//!    off at `backoff_base_secs = 0`. Paired forcings: it is neither constant
//!    nor uncapped.
//! 2. The policy keys default when absent and a base above the ceiling is
//!    refused, beside a legal one that is accepted.
//! 3. Every shipped declaration loads, the shipped set is EXACTLY the one
//!    named, and it splits EXACTLY into the services an outside launcher still
//!    owns (a `[migration]` naming that launcher) and the hosted ones (none).
//!    Equated, not enumerated: R5.S1b, on the R5.S1 inspection's F-3.
//! 4. `[migration]` and `kind` refuse what they must: a `job` with no outside
//!    launcher, an unknown kind, a migration with no steps — each beside the
//!    nearest legal declaration, which loads.

use env::decl::{self, Declaration, Manifest, Supervisor};

fn sup(toml_body: &str) -> Supervisor {
    let m: Manifest = toml::from_str(&format!("[supervisor]\n{toml_body}\n")).expect("parses");
    m.supervisor
}

#[test]
fn backoff_is_immediate_then_doubling_then_capped() {
    let s = sup("backoff_base_secs = 2\nbackoff_max_secs = 300");
    let got: Vec<u64> = (0..=10).map(|n| s.backoff_secs(n)).collect();
    assert_eq!(got, vec![0, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
    // A huge count does not overflow into a tiny wait.
    assert_eq!(s.backoff_secs(u32::MAX), 300);
}

#[test]
fn backoff_base_zero_disables_the_wait() {
    let s = sup("backoff_base_secs = 0");
    assert!((0..20).all(|n| s.backoff_secs(n) == 0));
}

#[test]
fn backoff_keys_default_when_absent() {
    let s = sup("auto_restart = true");
    assert_eq!((s.backoff_base_secs, s.backoff_max_secs, s.stable_after_secs), (2, 300, 60));
    assert_eq!(s.backoff_secs(1), 2);
}

#[test]
fn a_base_above_the_ceiling_is_refused_and_an_equal_one_is_not() {
    let bad = sup("backoff_base_secs = 10\nbackoff_max_secs = 5");
    let e = bad.validate("t").expect_err("base > max must be refused");
    let msg = format!("{e:#}");
    println!("{msg}");
    assert!(msg.contains("backoff_base_secs = 10"), "{msg}");
    sup("backoff_base_secs = 5\nbackoff_max_secs = 5").validate("t").expect("base == max is legal");
}

/// The shipped `services/` directory, through the real loader: a root with no
/// overrides, so `search_paths` falls through to the shipped files.
fn shipped(name: &str) -> anyhow::Result<Declaration> {
    let empty = std::env::temp_dir().join(format!("r5s1-no-overrides-{}", std::process::id()));
    decl::load(&empty, name)
}

/// The shipped registry, through the real scan: every declaration, broken or
/// not, keyed by name.
fn shipped_scan() -> std::collections::BTreeMap<String, Declaration> {
    let empty = std::env::temp_dir().join(format!("r5s1-scan-{}", std::process::id()));
    let scan = decl::all(&empty);
    for (n, e) in &scan.broken {
        eprintln!("BROKEN {n}: {e:#}");
    }
    assert!(scan.broken.is_empty(), "{} broken shipped declaration(s)", scan.broken.len());
    scan.ok.into_iter().map(|d| (d.name.clone(), d)).collect()
}

/// EXACT, not "contains" (R5.S1 inspection F-3, doctrine 10): a declaration
/// added to `services/` or dropped from it must change this list in the same
/// commit, so nobody registers a service — or loses one — without a test
/// naming it.
const SHIPPED: [&str; 7] = ["claude-account-expiry-alarm", "echo", "soup-backup", "tally", "threads", "tts", "ui-server"];

#[test]
fn every_shipped_declaration_loads() {
    let names: Vec<String> = shipped_scan().into_keys().collect();
    assert_eq!(names, SHIPPED, "the shipped service set changed; update SHIPPED and the [migration] split below");
}

#[test]
fn outside_launched_services_name_their_launcher_and_hosted_ones_do_not() {
    // The WHOLE partition of the registry, each half equated rather than
    // sampled: a new `[migration]` on `echo` or `tally`, or one dropped from
    // `ui-server`, changes a set below and goes red.
    let all = shipped_scan();
    let outside: Vec<(&str, &str)> = all
        .values()
        .filter_map(|d| d.migration.as_ref().map(|mg| (d.name.as_str(), mg.launched_by.as_str())))
        .collect();
    // R5.S4b: Deploy 4's three cut-over commits (04fbe314, 902c10d4,
    // 9c12b15d) deleted every remaining `[migration]`; this list still named
    // all three and was red on `main` from 9c12b15d. Nothing is launched from
    // outside now, so the per-`[migration]` loop below checks nothing until a
    // service is declared outside-launched again (left in place for that).
    let none: [(&str, &str); 0] = [];
    assert_eq!(outside, none, "the set of services an outside launcher still owns changed");
    let hosted: Vec<&str> = all.values().filter(|d| d.migration.is_none()).map(|d| d.name.as_str()).collect();
    assert_eq!(hosted, SHIPPED, "the set of services cauldron hosts changed");
    for (n, _) in &outside {
        let mg = all[*n].migration.as_ref().unwrap();
        assert!(!mg.steps.is_empty(), "{n}: a [migration] with no steps");
        assert!(mg.refusal(n).starts_with("REFUSED:"));
    }
    // And through the single-service loader too, which is what `pin` reads.
    for n in SHIPPED {
        let d = shipped(n).unwrap_or_else(|e| panic!("{n}: {e:#}"));
        assert!(d.migration.is_none(), "{n} is hosted by cauldron and must not carry [migration]");
    }
}

/// Write `body` as a root override and load it through the real loader, so
/// `validate` runs exactly as it does for `cauldron status`.
///
/// One root per CALL: two tests here load `j` in parallel threads, and a
/// `<pid>-<nanos>` name alone collides on macOS's microsecond clock (the same
/// race R5.S1c proved in `cli/tests/broken_declaration.rs`). The sequence
/// number makes it unique; the exclusive create makes a collision loud.
fn load_override(name: &str, body: &str) -> anyhow::Result<Declaration> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "r5s1-decl-{name}-{}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&root).unwrap_or_else(|e| panic!("override root {} already taken: {e}", root.display()));
    std::fs::write(root.join(format!("{name}.toml")), body).unwrap();
    let r = decl::load(&root, name);
    let _ = std::fs::remove_dir_all(&root);
    r
}

const JOB: &str = "name = \"j\"\nkind = \"job\"\nregisters = false\nprobe = \"none\"\n\
                   [build]\nartifact = \"/bin/true\"\nbin = \"true\"\n";
const MIG: &str = "[migration]\nlaunched_by = \"launchd:x\"\nsteps = [\"do it\"]\n";

#[test]
fn a_job_needs_an_outside_launcher() {
    let e = load_override("j", JOB).expect_err("a job with no [migration] must be refused");
    let msg = format!("{e:#}");
    println!("{msg}");
    assert!(msg.contains("kind = \"job\"") && msg.contains("[migration]"), "{msg}");
    let ok = load_override("j", &format!("{JOB}{MIG}")).expect("a job WITH [migration] loads");
    assert_eq!(ok.kind, "job");
}

#[test]
fn an_unknown_kind_is_refused_and_daemon_is_not() {
    let base = "name = \"k\"\nregisters = false\nprobe = \"connect\"\n[launch]\naddr = \"127.0.0.1:1\"\n\
                [build]\nartifact = \"/bin/true\"\nbin = \"true\"\n";
    let e = load_override("k", &format!("kind = \"cron\"\n{base}")).expect_err("unknown kind");
    let msg = format!("{e:#}");
    println!("{msg}");
    assert!(msg.contains("kind = \"cron\""), "{msg}");
    load_override("k", &format!("kind = \"daemon\"\n{base}")).expect("daemon loads");
}

#[test]
fn a_migration_with_no_steps_is_refused() {
    let e = load_override("j", &format!("{JOB}[migration]\nlaunched_by = \"launchd:x\"\n"))
        .expect_err("no steps");
    let msg = format!("{e:#}");
    println!("{msg}");
    assert!(msg.contains("no `launched_by` or no"), "{msg}");
}
