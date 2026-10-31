//! `daemon.image` — the running-image report.
//!
//! **A green you have not forced to fail is not evidence.** Every assertion
//! here is paired with the construction of the state it claims to detect:
//! the divergence tests build a divergence, the signing tests build an ad-hoc
//! image AND a properly signed one, and the undeterminable tests remove the
//! thing being asked about.
//!
//! The report always describes THE CURRENT PROCESS, so under `cargo test` the
//! "running image" is the test harness. That is exactly what makes these tests
//! honest: the harness is an ad-hoc-signed binary that is not the daemon and
//! has no boot identity, so it exercises the undeterminable paths for real
//! rather than through a mock.

use serde_json::Value;
use std::path::{Path, PathBuf};
use threads::image;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("threads-image-test-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn report_with(launch_path: Option<&Path>, data_dir: &Path) -> Value {
    image::report(&image::Context {
        launch_path,
        data_dir,
        boot_identity: None,
        boot_id: None,
        image_booted_at: None,
    })
}

fn fact<'a>(v: &'a Value, path: &[&str]) -> &'a Value {
    let mut cur = v;
    for k in path {
        cur = cur.get(k).unwrap_or_else(|| panic!("missing key {k} in {path:?}"));
    }
    cur
}

fn known_bool(v: &Value, path: &[&str]) -> Option<bool> {
    let f = fact(v, path);
    if f["known"] == Value::Bool(true) {
        f["value"].as_bool()
    } else {
        None
    }
}

/// Is a signing identity available to sign a fixture with? Signing is a
/// keychain operation and cannot be faked, so the properly-signed half of the
/// contrast is asserted only where it can actually be produced.
fn can_sign() -> bool {
    std::process::Command::new("/usr/bin/security")
        .args(["find-identity", "-v", "-p", "codesigning"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("harness-codesign"))
        .unwrap_or(false)
}

// ──────────────────────────────────────────────────────────────────────────
//  DIVERGENCE — the condition that confused three investigations
// ──────────────────────────────────────────────────────────────────────────

/// **THE ONE THAT MATTERS.** Build the divergent state on a copy and prove the
/// report says so.
///
/// The running image is this test process. We point `launch_path` at a
/// DIFFERENT file — which is precisely what happens on the real machine when a
/// `cargo build` lands a new binary on top of the path a daemon was launched
/// from. The inode at the path is then not the inode being executed, and that
/// is the divergence.
#[test]
fn a_replaced_launch_path_is_reported_as_a_divergence() {
    let dir = tmp("diverged");
    // A real, distinct file standing in for "somebody rebuilt over the path".
    let replaced = dir.join("threads");
    std::fs::copy(std::env::current_exe().unwrap(), &replaced).unwrap();
    // Sanity: the fixture really is a different inode from the running image's.
    // If this ever stopped being true the test would be asserting nothing.
    {
        use std::os::unix::fs::MetadataExt;
        let a = std::fs::metadata(&replaced).unwrap().ino();
        let b = std::fs::metadata(std::env::current_exe().unwrap()).unwrap().ino();
        assert_ne!(a, b, "the fixture must be a genuinely different file");
    }

    let v = report_with(Some(&replaced), &dir);

    assert_eq!(
        known_bool(&v, &["divergence", "diverged"]),
        Some(true),
        "a replaced launch path MUST report a divergence, got: {}",
        fact(&v, &["divergence", "diverged"])
    );
    assert_eq!(
        known_bool(&v, &["divergence", "by_inode", "same"]),
        Some(false),
        "the inodes must be reported as different"
    );
    let meaning = fact(&v, &["divergence", "meaning"]).as_str().unwrap();
    assert!(
        meaning.contains("NOT THE RUNNING IMAGE"),
        "the divergence must be stated in words, not left to be inferred: {meaning}"
    );
    // And the running image's own inode must still be reported — that is the
    // number a person needs to act on.
    assert!(
        fact(&v, &["running", "text_inode"])["known"] == Value::Bool(true),
        "the executing text inode must be known even when the path has diverged"
    );
}

/// The paired green: the SAME path is not a divergence. Without this, the test
/// above could pass by always reporting divergence.
#[test]
fn an_unreplaced_launch_path_is_not_a_divergence() {
    let dir = tmp("same");
    let me = std::env::current_exe().unwrap();
    let v = report_with(Some(&me), &dir);
    assert_eq!(
        known_bool(&v, &["divergence", "diverged"]),
        Some(false),
        "running from the path we are executing is NOT a divergence, got: {}",
        fact(&v, &["divergence", "diverged"])
    );
    assert_eq!(known_bool(&v, &["divergence", "by_inode", "same"]), Some(true));
}

/// The cheap check `daemon.status` uses must agree with the full report, or the
/// two fields answer the same question differently — which is the thing this
/// whole change exists to stop.
#[test]
fn the_cheap_status_check_agrees_with_the_full_report() {
    let dir = tmp("cheap");
    let me = std::env::current_exe().unwrap();
    let replaced = dir.join("threads");
    std::fs::copy(&me, &replaced).unwrap();

    assert_eq!(image::launch_path_holds_running_image(Some(&me)), Some(true));
    assert_eq!(image::launch_path_holds_running_image(Some(&replaced)), Some(false));
    // No path at all is UNDETERMINABLE, not `true`.
    assert_eq!(image::launch_path_holds_running_image(None), None);

    for (p, expect_diverged) in [(&me, false), (&replaced, true)] {
        let v = report_with(Some(p), &dir);
        assert_eq!(
            known_bool(&v, &["divergence", "diverged"]),
            Some(expect_diverged),
            "the full report and the cheap check must never disagree for {}",
            p.display()
        );
        assert_eq!(
            image::launch_path_holds_running_image(Some(p)),
            Some(!expect_diverged)
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  SIGNED vs AD-HOC
// ──────────────────────────────────────────────────────────────────────────

/// An ad-hoc image reads as ad-hoc, and a properly signed one reads as signed.
/// Both halves are constructed here; neither is assumed.
#[test]
fn an_adhoc_image_reads_adhoc_and_a_signed_one_reads_signed() {
    let dir = tmp("signing");

    // ── ad-hoc: what `cargo build` leaves ────────────────────────────────
    let adhoc = dir.join("adhoc-bin");
    std::fs::copy(std::env::current_exe().unwrap(), &adhoc).unwrap();
    let ok = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(&adhoc)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(ok, "could not produce an ad-hoc fixture");

    let d = image::on_disk(&adhoc);
    assert_eq!(d.ad_hoc, Some(true), "an ad-hoc image must read as ad-hoc");
    assert_eq!(
        d.signed_for_deploy(),
        Some(false),
        "an ad-hoc image is NOT signed for deploy — that is the whole absorb gate"
    );
    assert!(
        d.authority.is_none(),
        "an ad-hoc image has no signing authority; it must not report one"
    );
    let dr = d.designated_requirement.unwrap_or_default();
    assert!(
        !dr.contains("com.harness.threads"),
        "an ad-hoc designated requirement must NOT name the deploy identifier: {dr}"
    );

    // ── properly signed ──────────────────────────────────────────────────
    if !can_sign() {
        eprintln!(
            "NOTE: `harness-codesign` is not in the keychain, so the properly-signed half of \
             this contrast was not asserted. It is proven whenever the identity is present."
        );
        return;
    }
    let signed = dir.join("signed-bin");
    std::fs::copy(std::env::current_exe().unwrap(), &signed).unwrap();
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "harness-codesign", "--identifier", "com.harness.threads"])
        .arg(&signed)
        .output()
        .unwrap();
    assert!(out.status.success(), "signing the fixture failed: {out:?}");

    let d = image::on_disk(&signed);
    assert_eq!(d.ad_hoc, Some(false), "a properly signed image must not read as ad-hoc");
    assert_eq!(
        d.signed_for_deploy(),
        Some(true),
        "a properly signed image with the deploy identifier IS signed for deploy"
    );
    assert!(d.authority.is_some(), "a properly signed image must report its authority");
    assert!(
        d.designated_requirement.unwrap_or_default().contains("com.harness.threads"),
        "the designated requirement must NAME the deploy identifier"
    );
}

/// The running image's ad-hoc-ness comes from the KERNEL, not from a path, so
/// it survives the path being replaced. Under `cargo test` the harness is
/// itself an ad-hoc build, which is what makes this assertable.
#[test]
fn the_running_images_signature_is_read_from_the_kernel_not_the_path() {
    let dir = tmp("kernel");
    // Point the launch path at something that is NOT the running image.
    let decoy = dir.join("decoy");
    std::fs::copy(std::env::current_exe().unwrap(), &decoy).unwrap();
    let _ = std::process::Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-"])
        .arg(&decoy)
        .output();

    let v = report_with(Some(&decoy), &dir);
    // The kernel answered these regardless of the decoy at the launch path.
    assert!(
        fact(&v, &["running", "code_signature", "cdhash"])["known"] == Value::Bool(true),
        "the running cdhash must be readable from the kernel"
    );
    assert!(
        fact(&v, &["running", "code_signature", "ad_hoc"])["known"] == Value::Bool(true),
        "whether the RUNNING image is ad-hoc must be readable from the kernel"
    );
    // And it must not have been taken from the decoy: the two cdhashes differ.
    let running = fact(&v, &["running", "code_signature", "cdhash"])["value"].as_str().unwrap();
    let on_path = fact(&v, &["launch_path", "code_signature", "cdhash"])["value"]
        .as_str()
        .unwrap_or("");
    assert_ne!(
        running, on_path,
        "the report must not have read the running image's identity off the launch path"
    );
}

// ──────────────────────────────────────────────────────────────────────────
//  UNDETERMINABLE — the rule the whole endpoint turns on
// ──────────────────────────────────────────────────────────────────────────

/// A question with no answer must report that it has no answer, with a reason.
/// It must NOT default, and in particular a missing file must not read as
/// "not signed" — those are different facts and only one is a deploy problem.
#[test]
fn an_undeterminable_field_says_so_rather_than_defaulting() {
    let dir = tmp("undeterminable");
    let missing = dir.join("not-here");
    assert!(!missing.exists());

    // The struct level.
    let d = image::on_disk(&missing);
    assert!(!d.exists);
    assert_eq!(
        d.signed_for_deploy(),
        None,
        "a file that does not exist must not report `false` for signed-for-deploy — that is a \
         confident answer to an unanswerable question"
    );

    // The payload level.
    let v = report_with(Some(&missing), &dir);
    for path in [
        vec!["launch_path", "inode"],
        vec!["launch_path", "code_signature", "signed_for_deploy"],
        vec!["divergence", "diverged"],
        vec!["divergence", "by_inode", "same"],
        vec!["divergence", "by_code_identity", "same"],
        vec!["absorbable", "would_absorb"],
    ] {
        let f = fact(&v, &path);
        assert_eq!(
            f["known"],
            Value::Bool(false),
            "{path:?} must be undeterminable when there is no file at the launch path, got {f}"
        );
        assert_eq!(f["value"], Value::Null, "{path:?} must carry no value");
        let why = f["why"].as_str().unwrap_or("");
        assert!(
            why.len() > 20,
            "{path:?} must explain WHY it is undeterminable, got {why:?}"
        );
    }
}

/// No `known: false` fact anywhere in the payload may omit its reason. Walks
/// the whole tree, so a field added later cannot quietly break the rule.
#[test]
fn every_undeterminable_fact_in_the_payload_carries_a_reason() {
    let dir = tmp("walk");
    let missing = dir.join("nope");
    for v in [
        report_with(Some(&missing), &dir),
        report_with(None, &dir),
        report_with(Some(&std::env::current_exe().unwrap()), &dir),
        image::unreachable_report(
            &image::Context {
                launch_path: None,
                data_dir: &dir,
                boot_identity: None,
                boot_id: None,
                image_booted_at: None,
            },
            "the connection was refused",
        ),
    ] {
        let mut checked = 0usize;
        walk(&v, &mut |node| {
            if node.get("known") == Some(&Value::Bool(false)) {
                checked += 1;
                assert_eq!(node["value"], Value::Null, "an unknown fact must be null: {node:?}");
                let why = node.get("why").and_then(Value::as_str).unwrap_or("");
                assert!(why.len() > 15, "an unknown fact must say why: {node:?}");
            }
            if node.get("known") == Some(&Value::Bool(true)) {
                assert!(
                    node.get("value").is_some() && node["value"] != Value::Null,
                    "a KNOWN fact must carry a value: {node:?}"
                );
            }
        });
        assert!(checked > 0, "this fixture was supposed to produce unknown facts");
    }
}

fn walk(v: &Value, f: &mut impl FnMut(&serde_json::Map<String, Value>)) {
    match v {
        Value::Object(m) => {
            f(m);
            for (_, x) in m {
                walk(x, f);
            }
        }
        Value::Array(a) => {
            for x in a {
                walk(x, f);
            }
        }
        _ => {}
    }
}

/// Failing to READ a signature must never render as "there is no signature".
#[test]
fn an_unreadable_signature_is_not_reported_as_unsigned() {
    let err = image::parse_superblob(&[0xde, 0xad, 0xbe, 0xef, 0, 0, 0, 0, 0, 0, 0, 0])
        .expect_err("garbage must not parse");
    assert!(
        err.contains("not an embedded signature superblob"),
        "the reason must name what was actually wrong: {err}"
    );
}

// ──────────────────────────────────────────────────────────────────────────
//  BOOT — the field that would have prevented the second wrong investigation
// ──────────────────────────────────────────────────────────────────────────

/// The process's start time and the image's boot time are DIFFERENT fields and
/// both are reported, because reading one as the other is the documented error.
#[test]
fn the_process_start_and_the_image_boot_are_separate_fields() {
    let dir = tmp("boot");
    let v = image::report(&image::Context {
        launch_path: None,
        data_dir: &dir,
        boot_identity: None,
        boot_id: Some("test-boot-id"),
        image_booted_at: Some("2026-08-30T00:00:00+00:00"),
    });

    let proc_started = fact(&v, &["boot", "process_started_at"]);
    assert_eq!(
        proc_started["known"],
        Value::Bool(true),
        "the process start time must be readable from the kernel: {proc_started}"
    );
    assert_eq!(fact(&v, &["boot", "image_booted_at"])["value"], "2026-08-30T00:00:00+00:00");

    // Both must be explained, since the whole point is that they differ.
    for k in ["process_started_at_means", "image_booted_at_means"] {
        let s = fact(&v, &["boot", k]).as_str().unwrap();
        assert!(s.len() > 40, "{k} must explain itself: {s}");
    }
    assert!(
        fact(&v, &["boot", "process_started_at_means"])
            .as_str()
            .unwrap()
            .contains("SURVIVES EVERY RE-EXEC"),
        "the re-exec caveat is the entire reason this field exists"
    );

    // `boot_id` must state that it is memory-only. A prior report cited boot
    // identifier values and could not find them on disk; the payload has to
    // say which of the two is greppable and which is not.
    let where_ = fact(&v, &["boot", "boot_id", "recorded_where"]).as_str().unwrap();
    assert!(where_.contains("IN MEMORY ONLY"), "{where_}");
    assert!(where_.contains("NOT WRITTEN TO restarts.jsonl"), "{where_}");
    let where_id = fact(&v, &["boot", "boot_identity", "recorded_where"]).as_str().unwrap();
    assert!(where_id.contains("restarts.jsonl"), "{where_id}");
}

/// The build metadata is compiled in, so it describes the running image. It is
/// either present or explicitly undeterminable — never blank.
#[test]
fn build_metadata_describes_the_running_image_or_says_it_cannot() {
    let dir = tmp("build");
    let v = report_with(None, &dir);
    for k in ["built_at", "commit", "commit_dirty"] {
        let f = fact(&v, &["running", "build", k]);
        if f["known"] == Value::Bool(true) {
            assert!(f["value"] != Value::Null, "{k} claims known with no value");
        } else {
            let why = f["why"].as_str().unwrap_or("");
            assert!(why.len() > 20, "{k} must explain why it is unknown: {why:?}");
        }
    }
    // This crate has a build.rs, so a build compiled from this tree MUST carry
    // a timestamp. If this fails, build.rs stopped running.
    assert_eq!(
        fact(&v, &["running", "build", "built_at"])["known"],
        Value::Bool(true),
        "build.rs must stamp a build time into the image"
    );
}

// ──────────────────────────────────────────────────────────────────────────
//  The old, misleading field — RETIRED WITH ITS ARM
// ──────────────────────────────────────────────────────────────────────────
//
// `daemon_status_no_longer_reports_a_misleading_binary_field` scanned the
// `daemon/status` arm for the bare `binary` path field that misled three
// investigations. R2.S3c (R4.S9, HOLDS R9) retired `daemon/status` itself (no
// consumer sent it; the faces read `heartbeat` + `boot`), so the field has no
// arm to come back through. `daemon/image` — this file's subject — stays, and
// `ws_protocol.rs` pins that `daemon/status` answers `unknown_method`.
