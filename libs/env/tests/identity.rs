//! Permission identity: every refusal forced red against its real text, and a
//! positive sentinel beside each one so a test that could not fail is visible.
//!
//! The two designated-requirement strings used as fixtures here are not
//! invented. They were read off this machine's own artifacts on 2026-10-08 with
//! `codesign -d -r-`, one durable and one ad-hoc, and they are the two shapes
//! the parser has to tell apart.

use env::identity::{parse_requirement, process_name, Claim, Identity, Pin, Responsibility, Subject, Verdict};

/// `threads`, signed under the machine's stable identity.
const DURABLE: &str = r#"Executable=/Users/testuser/code/substrate-interface-layer/target/release/threads
designated => identifier "com.harness.threads" and certificate leaf = H"9a05af73b9eaac2f7fb5d71acee3e398bbd935a6"
"#;

/// `tts`, cargo's linker-signed output. Note the `# ` — codesign emits an
/// ad-hoc requirement as a COMMENT, and a parser that does not strip it reads
/// every cargo build as unsigned.
const AD_HOC: &str = r#"Executable=/Users/testuser/code/substrate-interface-layer/target/debug/tts
# designated => cdhash H"732b077bb33be6ae2b24c30a7b74d7870f2d7887"
"#;

const UNSIGNED: &str = "/tmp/probe: code object is not signed at all\n";

// ─── Identity::parse — four refusals, each with a sentinel ────────────────────

#[test]
fn an_empty_identifier_is_refused_because_a_prompt_has_nothing_to_show() {
    let err = Identity::parse("").expect_err("an empty identifier must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("an empty string is not a permission identity"),
        "the refusal must say what was wrong with it; got: {msg}"
    );
    assert!(
        msg.contains("no default"),
        "and must say there is no default to fall back to; got: {msg}"
    );

    // Positive sentinel: whitespace-only is the same refusal, and a real
    // identifier is not refused at all.
    assert!(Identity::parse("   ").is_err(), "whitespace-only is still empty");
    assert!(
        Identity::parse("ai.broomstick.cauldron").is_ok(),
        "a real reverse-DNS identifier must be accepted, or this test proves nothing"
    );
}

#[test]
fn cargos_own_ad_hoc_identifier_is_refused_by_name_as_the_tts_defect() {
    // The real string off the real binary.
    let err = Identity::parse("tts-adeff24ca3de5dea")
        .expect_err("a cargo content-hash identifier must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("cargo's own ad-hoc signing identifier"),
        "the refusal must name what the string actually is; got: {msg}"
    );
    assert!(
        msg.contains("CHANGES ON EVERY REBUILD"),
        "and must say why that disqualifies it; got: {msg}"
    );
    assert!(
        msg.contains("adeff24ca3de5dea"),
        "and must quote the hash it found; got: {msg}"
    );

    // Positive sentinel, and the one that matters most: an identifier that
    // merely ENDS in a hyphenated word must not be swept up by the hash rule.
    assert!(
        Identity::parse("ai.broomstick.cauldron-supervisor").is_ok(),
        "a hyphenated but non-hash suffix is a legitimate identifier"
    );
    // 16 chars but not hex, and 15 hex chars: both must pass.
    assert!(Identity::parse("ai.x.svc-zzzzzzzzzzzzzzzz").is_ok(), "not hex");
    assert!(Identity::parse("ai.x.svc-adeff24ca3de5de").is_ok(), "15 hex, not 16");
}

#[test]
fn a_bare_word_is_refused_because_it_reads_as_an_incidental_binary_name() {
    let err = Identity::parse("cauldron").expect_err("a dotless identifier must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("carries no `.`"),
        "the refusal must name the missing dot; got: {msg}"
    );
    assert!(
        msg.contains("indistinguishable from the incidental name"),
        "and must say what the operator would see; got: {msg}"
    );

    assert!(
        Identity::parse("ai.b.c").is_ok(),
        "one dot is enough, or the rule is stricter than it claims"
    );
}

#[test]
fn an_unquotable_character_is_refused_with_the_reason_it_cannot_appear() {
    let err = Identity::parse("ai.\"broomstick\".cauldron")
        .expect_err("an embedded quote must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("cannot appear in a permission identity"),
        "the refusal must say the character is not allowed; got: {msg}"
    );
    assert!(
        msg.contains("unparseable"),
        "and must say what it would actually break; got: {msg}"
    );

    // Each of the four barred classes, forced separately.
    for bad in ["ai.broomstick cauldron.x", "ai.b\\c.d", "ai.b\tc.d", "ai.b\nc.d"] {
        assert!(Identity::parse(bad).is_err(), "`{bad:?}` must be refused");
    }
    // Positive sentinel: the characters a real identifier does use.
    assert!(Identity::parse("ai.broomstick.cauldron-2").is_ok());
    // ...and leading/trailing whitespace is TRIMMED, not refused, because a
    // declaration file is written by hand.
    assert_eq!(
        Identity::parse("  ai.broomstick.cauldron  ").unwrap().identifier(),
        "ai.broomstick.cauldron",
        "surrounding whitespace is trimmed rather than refused"
    );
}

// ─── parse_requirement — all three subjects, from real strings ────────────────

#[test]
fn a_stable_signature_reads_as_a_durable_subject() {
    let s = parse_requirement(DURABLE);
    match &s {
        Subject::Durable { identity, pin } => {
            assert_eq!(identity.identifier(), "com.harness.threads");
            assert_eq!(
                pin,
                &Pin::LeafHash("9a05af73b9eaac2f7fb5d71acee3e398bbd935a6".to_string())
            );
        }
        other => panic!("the real threads requirement must read as durable, got {other:?}"),
    }
    assert!(s.is_durable());
    assert!(s.describe().contains("pinned to certificate"));
}

#[test]
fn cargos_linker_signature_reads_as_ad_hoc_not_as_unsigned() {
    // This is the assertion that would have failed before the `# ` prefix was
    // handled, and it is the whole reason the distinction is tested on real
    // output rather than on a string someone typed.
    let s = parse_requirement(AD_HOC);
    match &s {
        Subject::AdHoc { cdhash } => {
            assert_eq!(cdhash, "732b077bb33be6ae2b24c30a7b74d7870f2d7887")
        }
        other => panic!("the real tts requirement must read as ad-hoc, got {other:?}"),
    }
    assert!(!s.is_durable(), "an ad-hoc subject is never durable");
    assert!(s.identity().is_none(), "and it names no identity");
    assert!(
        s.describe().contains("NEW application on every rebuild"),
        "the description must say what it costs; got: {}",
        s.describe()
    );
}

#[test]
fn an_unsigned_artifact_reads_as_unsigned() {
    let s = parse_requirement(UNSIGNED);
    assert_eq!(s, Subject::Unsigned);
    assert!(!s.is_durable());
    // Positive sentinel: the parser is not simply returning Unsigned for
    // everything — the two fixtures above do not.
    assert!(parse_requirement(DURABLE).is_durable());
    assert_ne!(parse_requirement(AD_HOC), Subject::Unsigned);
}

#[test]
fn an_identifier_with_no_pinned_certificate_is_not_durable() {
    // A requirement naming an identifier but pinning no key: any key could mint
    // that name, so it is an identity nothing owns. It must not be read as
    // durable, and with no cdhash either there is nothing to fall back to.
    let req = "designated => identifier \"com.harness.threads\"\n";
    assert_eq!(
        parse_requirement(req),
        Subject::Unsigned,
        "an unpinned identifier is not a durable identity"
    );
    // Positive sentinel: adding the certificate pin flips it.
    assert!(parse_requirement(DURABLE).is_durable());
}

#[test]
fn reading_a_subject_off_a_file_that_does_not_exist_is_refused_not_guessed() {
    let err = Subject::read(std::path::Path::new("/nonexistent/cauldron-probe"))
        .expect_err("a missing artifact must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("no such file"),
        "the refusal must say the file is missing; got: {msg}"
    );
    assert!(
        msg.contains("a fact about bytes"),
        "and must say why there is no answer to give; got: {msg}"
    );

    // Positive sentinel: a file that DOES exist is read rather than refused.
    // /bin/sh is a platform binary and always present.
    let ok = Subject::read(std::path::Path::new("/bin/sh"));
    assert!(ok.is_ok(), "an existing artifact must be readable: {ok:?}");
}

// ─── Responsibility ──────────────────────────────────────────────────────────

#[test]
fn responsibility_defaults_to_inherit_because_that_is_the_whole_ask() {
    assert_eq!(
        Responsibility::default(),
        Responsibility::Inherit,
        "the default must be the disposition that makes a prompt say Cauldron"
    );
    assert_eq!(Responsibility::Inherit.as_str(), "inherit");
    assert_eq!(Responsibility::Own.as_str(), "own");
}

#[test]
fn an_unknown_responsibility_is_refused_by_name_rather_than_defaulted() {
    let err = Responsibility::parse("cauldron")
        .expect_err("an unknown responsibility must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains("is not a responsibility"),
        "the refusal must say so; got: {msg}"
    );
    assert!(
        msg.contains("`inherit`") && msg.contains("`own`"),
        "and must list both legal values; got: {msg}"
    );
    assert!(
        msg.contains("security dialog"),
        "and must say what a misread value costs; got: {msg}"
    );

    // Positive sentinels: both legal spellings round-trip, including with the
    // surrounding whitespace a hand-written declaration will have.
    assert_eq!(Responsibility::parse("inherit").unwrap(), Responsibility::Inherit);
    assert_eq!(Responsibility::parse("own").unwrap(), Responsibility::Own);
    assert_eq!(Responsibility::parse("  own\n").unwrap(), Responsibility::Own);
    // And the near-misses are refused, so the match is exact.
    for bad in ["Inherit", "OWN", "inherits", ""] {
        assert!(Responsibility::parse(bad).is_err(), "`{bad}` must be refused");
    }
}

#[test]
fn inherit_names_cauldron_even_when_the_service_has_no_identity_of_its_own() {
    // The load-bearing case: tts is ad-hoc and has no durable identity, and
    // inheriting is exactly what gives it one.
    let tts = parse_requirement(AD_HOC);
    let named = Responsibility::Inherit.subject_identity("prod", &tts);
    assert_eq!(
        named.as_ref().map(|i| i.identifier()),
        Some("ai.broomstick.cauldron"),
        "inheriting must name cauldron regardless of the service's own signature"
    );

    // And `own` on the same service names nothing, which is the honest answer:
    // there is no durable identity to be its own subject.
    assert_eq!(
        Responsibility::Own.subject_identity("prod", &tts),
        None,
        "an ad-hoc service that owns its subject has no durable name to show"
    );
    // Positive sentinel: `own` on a service that DOES have one names it.
    assert_eq!(
        Responsibility::Own
            .subject_identity("prod", &parse_requirement(DURABLE))
            .map(|i| i.identifier().to_string()),
        Some("com.harness.threads".to_string())
    );
}

// ─── Claim::check — all four verdicts, and only one is a defect ──────────────

fn claim(service: &str, claimed: Option<&str>) -> Claim {
    Claim {
        service: service.to_string(),
        responsibility: Responsibility::Inherit,
        claimed: claimed.map(|c| Identity::parse(c).unwrap()),
    }
}

#[test]
fn a_claim_the_artifact_bears_out_agrees() {
    let v = claim("threads", Some("com.harness.threads")).check(&parse_requirement(DURABLE));
    assert_eq!(
        v,
        Verdict::Agrees { identity: Identity::parse("com.harness.threads").unwrap() }
    );
    assert!(!v.is_defect(), "agreement is not a defect");
    assert!(v.describe().starts_with("ok"));
}

#[test]
fn a_claim_the_artifact_contradicts_is_the_one_verdict_that_is_a_defect() {
    let v = claim("tts", Some("com.harness.tts")).check(&parse_requirement(AD_HOC));
    assert!(v.is_defect(), "a contradicted claim must be a defect");
    let d = v.describe();
    assert!(d.starts_with("DEFECT"), "it must read as a defect; got: {d}");
    assert!(
        d.contains("com.harness.tts") && d.contains("every rebuild"),
        "and must name both what was claimed and what was found; got: {d}"
    );

    // Positive sentinel: the other three verdicts are NOT defects, so
    // `is_defect` is discriminating rather than always true.
    assert!(!claim("threads", Some("com.harness.threads"))
        .check(&parse_requirement(DURABLE))
        .is_defect());
    assert!(!claim("tts", None).check(&parse_requirement(AD_HOC)).is_defect());
    assert!(!claim("x", None).check(&parse_requirement(DURABLE)).is_defect());
}

#[test]
fn claiming_nothing_and_having_nothing_is_a_gap_not_a_violation() {
    // This is tts today, and the distinction is the same one `cauldron status`
    // already draws for an adopted seam: the service never signed the contract.
    let v = claim("tts", None).check(&parse_requirement(AD_HOC));
    assert!(matches!(v, Verdict::NoClaimNoIdentity { .. }));
    assert!(!v.is_defect(), "a gap must not be counted as a violation");
    assert!(v.describe().starts_with("gap"), "got: {}", v.describe());
}

#[test]
fn an_identity_on_the_artifact_that_the_declaration_does_not_name_is_reported() {
    let v = claim("threads", None).check(&parse_requirement(DURABLE));
    assert!(matches!(v, Verdict::Unclaimed { .. }));
    assert!(!v.is_defect(), "the artifact is the authority, so this is not a defect");
    assert!(
        v.describe().contains("which the declaration does not name"),
        "got: {}",
        v.describe()
    );
}

// ─── The constants are the identity, so they are pinned ──────────────────────

#[test]
fn every_environment_gets_its_own_identity_and_prod_is_the_unsuffixed_one() {
    // The ruling: "'cauldron' for prod or 'cauldron-<env>' for other envs...
    // That way each has its own signing identity and I can selectively approve
    // permissions per env."
    assert_eq!(process_name("prod"), "cauldron");
    assert_eq!(process_name("dev"), "cauldron-dev");
    assert_eq!(
        Identity::for_env("prod").unwrap().identifier(),
        "ai.broomstick.cauldron"
    );
    assert_eq!(
        Identity::for_env("dev").unwrap().identifier(),
        "ai.broomstick.cauldron-dev"
    );

    // THE LOAD-BEARING PROPERTY: two environments are two DIFFERENT identities,
    // because that is what lets him approve one and refuse the other. If these
    // were equal the whole mechanism would be a single machine-wide decision.
    assert_ne!(
        Identity::for_env("prod").unwrap(),
        Identity::for_env("test").unwrap(),
        "two environments must be two identities, or a grant is not divisible"
    );
    // And it must hold for every environment name the layout permits, not just
    // the two spelled above.
    let mut ids: Vec<String> = Vec::new();
    for e in ["prod", "dev", "test", "test-2", "a"] {
        ids.push(Identity::for_env(e).unwrap().identifier().to_string());
    }
    let mut uniq = ids.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), ids.len(), "every environment's identity is distinct: {ids:?}");

    // Each one must satisfy this module's OWN parser. An identity the parser
    // would refuse is the defect this catches.
    for e in ["prod", "dev", "test-2"] {
        let id = Identity::for_env(e).unwrap();
        Identity::parse(id.identifier())
            .unwrap_or_else(|err| panic!("the identity for `{e}` is not parseable: {err}"));
        assert!(
            !id.identifier().contains("threads") && !id.identifier().contains("tts"),
            "an identity must not carry a service's name: one name covers every service"
        );
    }
}

// ─── The declaration half: `[permissions]` in services/<name>.toml ───────────
//
// The point of these is that the knob now lives in cauldron's own declaration
// rather than in `~/.threads-supervisor/services/<name>.conf`, where `tts` is
// simply absent. Absence must mean the safe default, never an exemption.

/// A MINIMAL VALID DECLARATION, in the HOSTED shape.
///
/// This fixture used to carry `[adopt]`, because that was the only way to
/// declare a service cauldron did not build. Adoption is gone — cauldron hosts
/// what it declares — so the fixture is a `[build]` naming an image produced
/// elsewhere, which is the same situation stated in the shape that survived.
/// `artifact` is deliberately the field used, because it is the one
/// `service_binary` reads a signature off: the permission tests below should be
/// standing on the field that actually carries a path, not on whichever one
/// happens to make a declaration parse.
const HOSTED: &str = r#"
name = "svc"
probe = "connect"
[build]
bin = "svc"
artifact = "/tmp/svc/svc"
"#;

fn decl(extra: &str) -> env::decl::Declaration {
    toml::from_str(&format!("{HOSTED}{extra}")).expect("fixture must parse")
}

#[test]
fn a_declaration_with_no_permissions_section_still_inherits_cauldrons_identity() {
    let d = decl("");
    d.validate().expect("the bare fixture must be valid");
    assert_eq!(
        d.permissions.responsibility().unwrap(),
        Responsibility::Inherit,
        "absence must mean inherit, not exempt: a service cannot opt out by being forgotten"
    );
    assert_eq!(
        d.permissions.claimed_identity().unwrap(),
        None,
        "and claiming no identity is the honest default, as an empty commit is"
    );

    // Positive sentinel: an explicit section is read, so the default is not
    // simply overwriting whatever was written.
    let own = decl("\n[permissions]\nresponsibility = \"own\"\n");
    assert_eq!(own.permissions.responsibility().unwrap(), Responsibility::Own);
}

#[test]
fn an_unknown_responsibility_in_a_declaration_is_refused_at_load() {
    let d = decl("\n[permissions]\nresponsibility = \"cauldron\"\n");
    let err = d.validate().expect_err("an unknown responsibility must be refused");
    // anyhow chains the context in front of the cause; both halves matter.
    let chain = format!("{err:#}");
    assert!(
        chain.contains("`[permissions]` section of the declaration for `svc`"),
        "the refusal must say WHICH declaration and which section; got: {chain}"
    );
    assert!(
        chain.contains("is not a responsibility"),
        "and must carry the parser's own reason; got: {chain}"
    );

    // Positive sentinel: both legal values validate, so `validate` is not
    // simply always refusing a `[permissions]` section.
    decl("\n[permissions]\nresponsibility = \"inherit\"\n").validate().unwrap();
    decl("\n[permissions]\nresponsibility = \"own\"\n").validate().unwrap();
}

#[test]
fn a_declaration_claiming_cargos_ad_hoc_identifier_is_refused_at_load() {
    // The `tts` shape, written into a declaration. Refusing it here is the
    // difference between recording the defect as an identity and reporting it.
    let d = decl("\n[permissions]\nidentity = \"tts-adeff24ca3de5dea\"\n");
    let err = d.validate().expect_err("a content-hash identity must be refused");
    let chain = format!("{err:#}");
    assert!(
        chain.contains("cargo's own ad-hoc signing identifier"),
        "got: {chain}"
    );
    assert!(chain.contains("`[permissions]` section"), "got: {chain}");

    // Positive sentinel: a real identifier in the same slot validates, and an
    // EMPTY one validates too, because empty is honest rather than wrong.
    decl("\n[permissions]\nidentity = \"com.harness.threads\"\n").validate().unwrap();
    let empty = decl("\n[permissions]\nidentity = \"\"\n");
    empty.validate().unwrap();
    assert_eq!(
        empty.permissions.claimed_identity().unwrap(),
        None,
        "an empty identity is no claim, not a claim of the empty string"
    );
}

#[test]
fn a_declared_identity_is_checked_against_the_artifact_rather_than_trusted() {
    let d = decl("\n[permissions]\nidentity = \"com.harness.tts\"\n");
    d.validate().expect("the claim itself is well-formed");

    let c = Claim {
        service: d.name.clone(),
        responsibility: d.permissions.responsibility().unwrap(),
        claimed: d.permissions.claimed_identity().unwrap(),
    };
    // A well-formed claim that the artifact contradicts is still a defect: the
    // declaration being valid TOML is not evidence about the bytes.
    assert!(
        c.check(&parse_requirement(AD_HOC)).is_defect(),
        "a valid declaration whose artifact disagrees must still be a defect"
    );
    // Positive sentinel: against the artifact it does describe, it agrees.
    let threads = Claim {
        service: "threads".into(),
        responsibility: Responsibility::Inherit,
        claimed: Some(Identity::parse("com.harness.threads").unwrap()),
    };
    assert!(!threads.check(&parse_requirement(DURABLE)).is_defect());
}

// ─── The repo's own four declarations, loaded through the real loader ────────

#[test]
fn every_declaration_this_repo_ships_declares_a_usable_permission_identity() {
    // `load` runs `validate`, so this is the check that a real
    // `services/<name>.toml` cannot carry a permission section that would be
    // refused at runtime. It loads the files on disk, not a fixture.
    let empty = std::path::Path::new("/nonexistent-root-services");
    let mut seen = Vec::new();
    for name in ["echo", "tally", "threads", "tts"] {
        let d = env::decl::load(empty, name)
            .unwrap_or_else(|e| panic!("services/{name}.toml must load and validate: {e:#}"));
        let r = d.permissions.responsibility().unwrap();
        let claimed = d.permissions.claimed_identity().unwrap();
        seen.push((name, r, claimed.map(|i| i.identifier().to_string())));
    }

    // Positive sentinel with teeth: assert the SPECIFIC facts, so a file that
    // silently lost its section fails here rather than passing as "valid".
    assert_eq!(
        seen,
        vec![
            // No section at all: the default carries them.
            ("echo", Responsibility::Inherit, None),
            ("tally", Responsibility::Inherit, None),
            // Cargo-built from `services/threads` since R3.S2.8a, so it claims
            // nothing, exactly as tts: the signed substrate image that bore
            // `com.harness.threads` is no longer this service's source.
            ("threads", Responsibility::Inherit, None),
            // The gap, declared as a gap: inherit, claiming nothing.
            ("tts", Responsibility::Inherit, None),
        ],
        "the shipped declarations' permission identities changed"
    );

    // And WHY threads claims nothing: its `[build]` is a cargo source in this
    // repository, the same shape as tts — no `artifact` to read an identity
    // off, no `stamp_args`. A declaration that went back to the artifact form
    // without restoring its claim (or vice versa) fails here.
    for name in ["threads", "tts"] {
        let d = env::decl::load(empty, name).unwrap();
        let b = d.build.as_ref().unwrap_or_else(|| panic!("services/{name}.toml has a [build]"));
        assert_eq!(
            (b.package.as_deref(), b.bin.as_str(), b.profile.as_str(), b.is_artifact(), b.stamps_itself(), b.workspace.as_deref()),
            (Some(name), name, "release", false, false, None),
            "services/{name}.toml must be a cargo source in this repository (package = bin = `{name}`, release, no artifact, no stamp_args)"
        );
    }
}

#[test]
fn the_claim_threads_makes_is_borne_out_by_a_cargo_built_image() {
    // threads is cargo-built (R3.S2.8a), and cargo's output is linker-signed
    // ad-hoc with no identifier, so the honest declaration claims nothing and
    // the verdict against a cargo-built image is the tts gap — never a defect.
    //
    // The image checked is THIS test executable: cargo-built by the same
    // toolchain in the same target directory as `cauldron build threads`, and
    // always present, so the check cannot skip itself into a green.
    let image = std::env::current_exe().expect("the test executable's own path");
    let found = Subject::read(&image).expect("reading the subject off the test executable");
    // Sentinel: the subject really is cargo's ad-hoc output. If this ever
    // reads as durable, the verdicts below would be measuring something else.
    assert!(
        matches!(found, Subject::AdHoc { .. }),
        "a cargo-built image must read as ad-hoc; got: {}",
        found.describe()
    );

    let claim_of = |d: &env::decl::Declaration| Claim {
        service: d.name.clone(),
        responsibility: d.permissions.responsibility().unwrap(),
        claimed: d.permissions.claimed_identity().unwrap(),
    };

    let shipped = env::decl::load(std::path::Path::new("/nonexistent-root-services"), "threads").unwrap();
    let v = claim_of(&shipped).check(&found);
    assert_eq!(
        v,
        Verdict::NoClaimNoIdentity { found: found.clone() },
        "services/threads.toml must claim nothing, as tts does; got: {}",
        v.describe()
    );

    // Opposed (doctrine 2): the SAME file with the pre-8a claim restored, read
    // through the real loader from a root override, is a DEFECT against the
    // same image. So the verdict above depends on the declaration, not on a
    // check that cannot fail.
    let root = std::env::temp_dir().join(format!("identity-threads-old-claim-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let shipped_text = std::fs::read_to_string(&shipped.loaded_from).unwrap();
    assert!(shipped_text.contains("identity = \"\"\n"), "the shipped file states `identity = \"\"`");
    std::fs::write(
        root.join("threads.toml"),
        shipped_text.replace("identity = \"\"\n", "identity = \"com.harness.threads\"\n"),
    )
    .unwrap();
    let old = env::decl::load(&root, "threads");
    let _ = std::fs::remove_dir_all(&root);
    let old = old.expect("the old claim is well-formed and loads");
    let ov = claim_of(&old).check(&found);
    assert!(
        ov.is_defect(),
        "claiming com.harness.threads over a cargo-built image must be a defect; got: {}",
        ov.describe()
    );
}

// ─── The three durable pin shapes, from their real requirements ──────────────
//
// The first version of this module recognised only `certificate leaf = H"…"`
// and therefore reported BOTH of the requirements below as unsigned — including
// `com.anthropic.claude-code`, the identity that actually shows up on the
// operator's prompts. These are real strings from this machine.

/// `/bin/sh`: an Apple platform binary.
const APPLE: &str = "designated => identifier \"com.apple.sh\" and anchor apple\n";

/// `claude`: a Developer ID, team Q6L2SF6YDW.
const DEVID: &str = "designated => identifier \"com.anthropic.claude-code\" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = Q6L2SF6YDW\n";

#[test]
fn an_apple_anchored_binary_is_durable_not_unsigned() {
    let s = parse_requirement(APPLE);
    match &s {
        Subject::Durable { identity, pin } => {
            assert_eq!(identity.identifier(), "com.apple.sh");
            assert_eq!(pin, &Pin::AppleAnchor);
        }
        other => panic!("`anchor apple` must be durable, got {other:?}"),
    }
    assert!(s.is_durable());
    assert!(s.describe().contains("anchored to Apple"), "got: {}", s.describe());
}

#[test]
fn claude_codes_developer_id_is_durable_and_its_team_is_read() {
    // This is the identity that appears on the operator's own prompts, so
    // getting it wrong mis-reports the whole subject.
    let s = parse_requirement(DEVID);
    match &s {
        Subject::Durable { identity, pin } => {
            assert_eq!(identity.identifier(), "com.anthropic.claude-code");
            assert_eq!(pin, &Pin::DeveloperId("Q6L2SF6YDW".to_string()));
        }
        other => panic!("a Developer ID must be durable, got {other:?}"),
    }
    assert!(s.describe().contains("Q6L2SF6YDW"), "got: {}", s.describe());
}

#[test]
fn the_three_pin_shapes_are_told_apart_rather_than_collapsed() {
    // A parser that returned Durable{Other} for everything would pass each of
    // the three tests above only if they asserted `is_durable()`. They assert
    // the PIN, and this asserts the three pins are three different values.
    let pins: Vec<Pin> = [DURABLE, APPLE, DEVID]
        .iter()
        .map(|r| match parse_requirement(r) {
            Subject::Durable { pin, .. } => pin,
            other => panic!("all three must be durable, got {other:?}"),
        })
        .collect();
    assert_eq!(pins.len(), 3);
    assert_ne!(pins[0], pins[1]);
    assert_ne!(pins[1], pins[2]);
    assert_ne!(pins[0], pins[2]);
    assert!(!pins.contains(&Pin::Other), "none of the three is the fallback: {pins:?}");

    // Positive sentinel the other way: an UNBOUND identifier is still not
    // durable, so the widening did not swallow the rule it was widening.
    assert_eq!(
        parse_requirement("designated => identifier \"com.example.loose\"\n"),
        Subject::Unsigned,
        "an identifier with nothing binding it is still not an identity"
    );
    // And cargo's ad-hoc output is still ad-hoc, not Durable{Other}.
    assert!(matches!(parse_requirement(AD_HOC), Subject::AdHoc { .. }));
}
