//! **R3.S2.10 — `cauldron permissions` reads a CARGO-BUILT service's binary.**
//!
//! Ledger line 188 (R3.S2.8a identity audit): a declaration with
//! `[build] package = …` and no `artifact` — threads' shape — used to print
//! "not built here, so nothing to read", although cauldron builds it, so a
//! durable signature landing on that build would go unseen. These tests pin
//! the fix: the verdict is read off the PINNED version's binary in the build
//! store, the same file a launch execs.
//!
//! Opposed pairs, so no constant implementation passes:
//! * an ad-hoc (cargo linker-signed) binary reads as `gap`, and a durably
//!   signed one (a copy of `/usr/bin/true`, `identifier "com.apple.true" and
//!   anchor apple`) reads as `unclaimed` — two different verdicts from one
//!   declaration, decided by the BYTES at the pinned path;
//! * the pin, not the newest build, is what is read: two builds, pinned to the
//!   OLDER one, and the older one's verdict is printed;
//! * a cargo-form service with no build at all still says it cannot be read,
//!   rather than inventing a verdict.
//!
//! Throwaway `HOME` per test; `CAULDRON_HOME`/`CAULDRON_ENV`/`ENV` removed
//! from the child. Nothing is started; the verb only reads.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_cauldron");
const OLD: &str = "0123456789ab-release";
const NEW: &str = "fedcba987654-release";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

struct Home(PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fresh(tag: &str) -> Home {
    let h = std::env::temp_dir().join(format!("cauldron-perm-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&h);
    std::fs::create_dir_all(&h).unwrap();
    Home(h)
}

fn cauldron(home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN)
        .env("HOME", home)
        .env_remove("CAULDRON_HOME")
        .env_remove("CAULDRON_ENV")
        .env_remove("ENV")
        .args(args)
        .output()
        .expect("spawn cauldron");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A cargo-form declaration, threads' shape: `package` + `bin`, no artifact,
/// `identity = ""`.
fn declare(home: &Path) {
    let svc = home.join(".cauldron/services");
    std::fs::create_dir_all(&svc).unwrap();
    std::fs::write(
        svc.join("cargosvc.toml"),
        r#"name        = "cargosvc"
description = "test fixture: a service cauldron compiles itself"
kind        = "daemon"

[build]
package = "cargosvc"
bin     = "cargosvc"
profile = "release"

[permissions]
responsibility = "inherit"
identity = ""
"#,
    )
    .unwrap();
}

/// Install `image` as version `label` of `cargosvc`, recorded as built at
/// `built_at` (so `latest` is decidable).
fn install(home: &Path, label: &str, image: &Path, built_at: u64) {
    let dir = home.join(".cauldron/data/shared/builds/cargosvc").join(label);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(image, dir.join("cargosvc")).unwrap();
    let rec = serde_json::json!({
        "service": "cargosvc", "label": label, "bin": "cargosvc", "package": "cargosvc",
        "profile": "release", "workspace": "/nowhere", "built_at": built_at,
        "commit": COMMIT, "dirty": false, "content": "0123456789abcdef", "source_commit": null
    });
    std::fs::write(dir.join("build.json"), serde_json::to_string_pretty(&rec).unwrap()).unwrap();
}

fn pin(home: &Path, label: &str) {
    let dir = home.join(".cauldron/data/prod");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("env.toml"), format!("[services.cargosvc]\nversion = \"{label}\"\n")).unwrap();
}

/// The `cargosvc` row of the IDENTITY table.
fn row(out: &str) -> String {
    out.lines()
        .find(|l| l.trim_start().starts_with("cargosvc "))
        .unwrap_or_else(|| panic!("no cargosvc row in:\n{out}"))
        .to_string()
}

/// An ad-hoc image: this test binary's sibling, cargo's linker-signed output.
fn adhoc() -> PathBuf {
    PathBuf::from(BIN)
}

/// A durably signed image.
fn durable() -> PathBuf {
    PathBuf::from("/usr/bin/true")
}

/// Sentinel (doctrine 9): the two images really are different subjects, or
/// the opposed pair below compares a thing with itself.
fn assert_images_differ() {
    let req = |p: &Path| {
        let o = Command::new("/usr/bin/codesign").args(["-d", "-r-"]).arg(p).output().unwrap();
        format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
    };
    let (a, d) = (req(&adhoc()), req(&durable()));
    assert!(a.contains("cdhash H\""), "the ad-hoc image is not ad-hoc: {a}");
    assert!(d.contains("identifier \"com.apple.true\""), "the durable image is not durable: {d}");
}

#[test]
fn a_pinned_cargo_build_with_an_adhoc_binary_reads_as_a_gap_not_not_built_here() {
    assert_images_differ();
    let home = fresh("adhoc");
    let h = &home.0;
    declare(h);
    install(h, OLD, &adhoc(), 1);
    pin(h, OLD);
    let (_rc, out, err) = cauldron(h, &["permissions", "--hours", "1"]);
    let r = row(&out);
    assert!(!r.contains("not built here"), "a cargo-built service must be read: {r}\n{err}");
    assert!(r.contains("gap") && r.contains("ad-hoc cdhash"), "expected a gap verdict: {r}");
    assert!(r.contains(OLD), "the row must say which version was read: {r}");
}

#[test]
fn a_planted_durably_signed_cargo_build_reads_as_unclaimed() {
    assert_images_differ();
    let home = fresh("durable");
    let h = &home.0;
    declare(h);
    install(h, OLD, &durable(), 1);
    pin(h, OLD);
    let (rc, out, err) = cauldron(h, &["permissions", "--hours", "1"]);
    let r = row(&out);
    assert!(
        r.contains("unclaimed") && r.contains("com.apple.true"),
        "a durable signature on the pinned build must surface: {r}\n{err}"
    );
    // Unclaimed is not a contradicted claim: exit 4 is reserved for that. A
    // nonzero code from the tccd half (log show) is not this test's subject.
    assert_ne!(rc, 4, "unclaimed is not a defect: {out}");
}

/// The PIN decides, not recency: the newer build is durable, the pinned older
/// one is ad-hoc, and the ad-hoc verdict is what prints. Flipping the pin
/// flips the verdict.
#[test]
fn the_pinned_version_is_read_not_the_newest() {
    let home = fresh("pin");
    let h = &home.0;
    declare(h);
    install(h, OLD, &adhoc(), 1);
    install(h, NEW, &durable(), 2);
    pin(h, OLD);
    let r = row(&cauldron(h, &["permissions", "--hours", "1"]).1);
    assert!(r.contains("gap") && r.contains(OLD), "pinned OLD must be read: {r}");
    pin(h, NEW);
    let r = row(&cauldron(h, &["permissions", "--hours", "1"]).1);
    assert!(r.contains("unclaimed") && r.contains(NEW), "pinned NEW must be read: {r}");
}

#[test]
fn a_cargo_form_service_with_no_build_says_it_cannot_be_read() {
    let home = fresh("none");
    let h = &home.0;
    declare(h);
    pin(h, OLD);
    let r = row(&cauldron(h, &["permissions", "--hours", "1"]).1);
    assert!(!r.contains("gap") && !r.contains("unclaimed"), "no build, no verdict: {r}");
    assert!(r.contains("cargo-built") && r.contains(OLD), "say why it cannot be read: {r}");
}

/// DEPLOY-8 (R3.S2.10 inspect note): a malformed `env.toml` used to abort the
/// whole audit before any row printed. Now it is one DEFECT row, the cargo row
/// says why it cannot be read, and an artifact row that never needed the pins
/// still reads its artifact.
#[test]
fn a_malformed_env_toml_is_one_defect_not_the_end_of_the_audit() {
    let home = fresh("badenv");
    let h = &home.0;
    declare(h);
    std::fs::write(
        h.join(".cauldron/services/artsvc.toml"),
        r#"name        = "artsvc"
description = "test fixture: an artifact service, needs no pins"
kind        = "daemon"

[build]
artifact = "/usr/bin/true"
bin      = "true"
profile  = "release"

[permissions]
responsibility = "inherit"
identity = ""
"#,
    )
    .unwrap();
    let dir = h.join(".cauldron/data/prod");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("env.toml"), "[services.cargosvc\nversion = \"x\"\n").unwrap();
    // rc is not asserted: the tccd half (`log show`) decides it too.
    let (_rc, out, err) = cauldron(h, &["permissions", "--hours", "1"]);
    assert!(out.contains("DEFECT: env.toml does not load"), "the bad env.toml must be named: {out}\n{err}");
    let r = row(&out);
    assert!(r.contains("env.toml does not load"), "the cargo row must say why: {r}");
    let a = out.lines().find(|l| l.trim_start().starts_with("artsvc ")).unwrap_or_else(|| panic!("no artsvc row: {out}\n{err}"));
    assert!(a.contains("com.apple.true"), "the artifact row must still be read: {a}");
}
