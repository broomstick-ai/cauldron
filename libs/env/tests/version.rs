//! **The version identity is the commit**, and the environment is the unit of
//! isolation.
//!
//! Every assertion here is on a value or on a refusal's own text, and every
//! guard is forced RED once with that text printed, so the proof run records
//! what the operator would actually see.
//!
//! The tests that mutate the PROCESS environment live in one function, in
//! order, following `environment.rs` — `cargo test` runs test functions in
//! parallel threads of one process, so two functions racing on `set_var` is a
//! flake, not a test.

use env::decl::{BuildRecord, Manifest, Pin, GOOD_HISTORY};
use env::paths::RESERVED_ENV_NAMES;
use env::seams::{self, Identity, PidSource, Seam, SeamState};
use env::Environment;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

const COMMIT_A: &str = "4197fc6e1d2a3b4c5d6e7f8091a2b3c4d5e6f701";
const COMMIT_B: &str = "dd991b4aa0112233445566778899aabbccddeeff";

// ───────────────────────────────────────────────────────────────── fixtures

fn seam(addr: &str, commit: &str) -> Seam {
    Seam {
        service: "echo".into(),
        addr: addr.into(),
        pid: std::process::id(),
        boot_token: "tok".into(),
        version: "v1".into(),
        commit: commit.into(),
        dirty: false,
        binary: "/nowhere/echo".into(),
        env: "dev".into(),
        data_root: "/tmp/fake/data/dev".into(),
        data_dir: "/tmp/fake/data/dev/echo".into(),
        started_at: 0,
        probe: "seam".into(),
        registered: true,
        pid_source: PidSource::SELF.into(),
        health: String::new(),
    }
}

fn ident(commit: &str) -> Identity {
    Identity {
        service: "echo".into(),
        env: "dev".into(),
        data_root: "/tmp/fake/data/dev".into(),
        boot_token: "tok".into(),
        version: "v1".into(),
        commit: commit.into(),
        pid: 1,
        addr: "127.0.0.1:1".into(),
    }
}

/// A listener that answers HELLO once with `id`.
fn answering(id: Identity) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    env::egress::grant_addr(l.local_addr().unwrap()).unwrap(); // REL-6a6: given
    let addr = l.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        if let Ok((s, _)) = l.accept() {
            let mut w = s.try_clone().unwrap();
            let mut line = String::new();
            let _ = BufReader::new(s).read_line(&mut line);
            let mut body = serde_json::to_string(&id).unwrap();
            body.push('\n');
            let _ = w.write_all(body.as_bytes());
            let _ = w.flush();
        }
    });
    addr
}

// ══════════════════════════════ the registry is clear about the version ════

/// **FORCED RED: a listener running a different commit than the registry
/// records.**
///
/// This is the whole point of putting the commit on the seam. The process is
/// alive, it is the right instance (the boot token matches), it is in the right
/// environment — and `status` would have said `live` while being wrong about
/// what is running. A registry that is confidently wrong about the version is
/// worse than one that says nothing, which is the same argument the seam table
/// itself is built on.
#[test]
fn a_listener_on_another_commit_is_wrong_version_not_live() {
    let v = seams::probe(&seam(&answering(ident(COMMIT_B)), COMMIT_A));
    println!("row={COMMIT_A} listener={COMMIT_B} -> {} :: {}", v.state.as_str(), v.detail);
    assert_eq!(
        v.state,
        SeamState::StaleWrongVersion,
        "a commit disagreement is its own verdict, not `live` and not `wrong-owner`"
    );
    assert!(v.state.is_stale(), "a registry that is wrong about the version IS the table lying");
    // The real text, asserted so the message cannot be gutted without a test
    // noticing.
    assert!(v.detail.contains("Nothing needs restarting"), "{}", v.detail);
    assert!(v.detail.contains(COMMIT_B), "it must name what the listener said: {}", v.detail);
    assert!(v.detail.contains(COMMIT_A), "and what the registry recorded: {}", v.detail);
}

/// THE POSITIVE SENTINEL for the test above. A check that refused every pair
/// would pass it and be worthless.
#[test]
fn matching_commits_still_read_live() {
    let v = seams::probe(&seam(&answering(ident(COMMIT_A)), COMMIT_A));
    println!("row==listener=={COMMIT_A} -> {}", v.state.as_str());
    assert_eq!(v.state, SeamState::Live, "the strong verdict must still be reachable");
}

/// **Silence is not disagreement, in BOTH directions.**
///
/// An empty commit on either side means "this was not stated" — a row written
/// before the field existed, or an adoption that honestly named none. Comparing
/// against an empty string would manufacture a fault out of an absence, which
/// is the same empty-equals-empty hole the boot token closes twice.
#[test]
fn an_unstated_commit_is_never_compared() {
    // The ROW says nothing.
    let v = seams::probe(&seam(&answering(ident(COMMIT_B)), ""));
    println!("row=<empty> listener={COMMIT_B} -> {}", v.state.as_str());
    assert_eq!(v.state, SeamState::Live, "an unstated row commit cannot disagree with anything");

    // The LISTENER says nothing.
    let v = seams::probe(&seam(&answering(ident("")), COMMIT_A));
    println!("row={COMMIT_A} listener=<empty> -> {}", v.state.as_str());
    assert_eq!(v.state, SeamState::Live, "a producer that predates the field has not disagreed");
}

// ═══════════════════════════════════════════ good versions, and rollback ══

/// **N = 2 is where this is proven, because N < 2 cannot exercise it.**
///
/// `previous_good` arbitrates between entries in a history, and a history of
/// nought or one has no previous — those are the cases that cannot test the
/// thing. So every assertion below is made at two or more, and the empty and
/// single cases are asserted to return `None` rather than something.
#[test]
fn previous_good_is_the_rollback_target_and_needs_two() {
    let mut m = Manifest::default();

    // N = 0: nothing to roll back to, and it says so rather than inventing one.
    assert_eq!(m.previous_good("echo"), None, "an environment that ran nothing has no previous");

    // N = 1: still nothing. This is the case a careless implementation returns
    // the CURRENT version for, which is a rollback that changes nothing and
    // reports success.
    m.record_good("echo", "aaaaaaaaaaaa");
    assert_eq!(m.good("echo"), vec!["aaaaaaaaaaaa"]);
    assert_eq!(m.previous_good("echo"), None, "one good version is not a rollback target");

    // N = 2: now there is one, and it is the OLDER of the two.
    m.record_good("echo", "bbbbbbbbbbbb");
    assert_eq!(m.good("echo"), vec!["bbbbbbbbbbbb", "aaaaaaaaaaaa"], "newest first");
    assert_eq!(m.previous_good("echo").as_deref(), Some("aaaaaaaaaaaa"));

    // N = 3, to prove the window moves rather than growing a tail.
    m.record_good("echo", "cccccccccccc");
    assert_eq!(m.previous_good("echo").as_deref(), Some("bbbbbbbbbbbb"));

    // Starting the SAME version again is not news: good is a property of a
    // version, not a count of boots. If this pushed a duplicate, the previous
    // good would become the version you are already on.
    m.record_good("echo", "cccccccccccc");
    assert_eq!(m.good("echo"), vec!["cccccccccccc", "bbbbbbbbbbbb", "aaaaaaaaaaaa"]);
    assert_eq!(m.previous_good("echo").as_deref(), Some("bbbbbbbbbbbb"));

    // Re-proving an OLDER version good MOVES it to the head rather than
    // duplicating it. A history with one label twice could not answer "what was
    // the previous one" at all.
    m.record_good("echo", "aaaaaaaaaaaa");
    assert_eq!(m.good("echo"), vec!["aaaaaaaaaaaa", "cccccccccccc", "bbbbbbbbbbbb"]);
    assert_eq!(m.previous_good("echo").as_deref(), Some("cccccccccccc"));

    // Per service, not per environment-wide blob.
    assert_eq!(m.previous_good("tally"), None, "another service's history is its own");
}

/// `latest` is a QUESTION — it resolves to a different build tomorrow — so it is
/// never recorded as a known-good version. A rollback target that moves is not a
/// rollback target.
#[test]
fn latest_is_never_recorded_as_good() {
    let mut m = Manifest::default();
    m.record_good("echo", "latest");
    m.record_good("echo", "");
    assert!(m.good("echo").is_empty(), "`latest` and `` are not versions: {:?}", m.good("echo"));
    m.record_good("echo", "aaaaaaaaaaaa");
    m.record_good("echo", "latest");
    assert_eq!(m.good("echo"), vec!["aaaaaaaaaaaa"], "`latest` must not displace a real label");
}

/// The history is bounded, so `env.toml` stays a file the operator can read on
/// a machine that restarts often.
#[test]
fn the_good_history_is_capped() {
    let mut m = Manifest::default();
    for i in 0..GOOD_HISTORY * 3 {
        m.record_good("echo", &format!("commit{i:04}"));
    }
    assert_eq!(m.good("echo").len(), GOOD_HISTORY);
    // And it kept the NEWEST, not the first ones it saw.
    assert_eq!(m.good("echo")[0], format!("commit{:04}", GOOD_HISTORY * 3 - 1));
}

/// A `Pin` round-trips through TOML with its history, and an `env.toml` written
/// before `good` existed still parses — the operator's file is not invalidated
/// by this change.
#[test]
fn an_env_toml_without_a_history_still_parses() {
    let old = "[services.echo]\nversion = \"aaaaaaaaaaaa\"\n";
    let m: Manifest = toml::from_str(old).expect("an env.toml predating `good` must still load");
    assert_eq!(m.pin("echo").as_deref(), Some("aaaaaaaaaaaa"));
    assert!(m.good("echo").is_empty(), "absent history is empty history, not a parse error");

    let mut m = m;
    m.record_good("echo", "bbbbbbbbbbbb");
    m.record_good("echo", "cccccccccccc");
    let text = toml::to_string_pretty(&m).unwrap();
    println!("round-tripped env.toml:\n{text}");
    let back: Manifest = toml::from_str(&text).unwrap();
    assert_eq!(back.previous_good("echo").as_deref(), Some("bbbbbbbbbbbb"));

    // `set_version` creates the entry for a service that was never pinned.
    let mut m = Manifest::default();
    m.set_version("tally", "dddddddddddd");
    assert_eq!(m.pin("tally").as_deref(), Some("dddddddddddd"));
    assert_eq!(Pin::default().version, "latest");
}

/// **AN UNPINNED SERVICE HAS NO VERSION, AND SAYING `latest` WAS A LIE THE
/// OPERATOR READ.**
///
/// `Manifest::pin` used to return the literal string `latest` for a service
/// absent from `env.toml`, so `cauldron status --json` reported
/// `pinned = "latest"` for `threads` and `tts` in an environment that pins
/// neither — the same conflation the `ensure_manifest` ruling deleted from the
/// code path, surviving on the surface the operator reads.
///
/// **This test asserts the CORRECTED behaviour and not the defect.** An
/// assertion that the defect holds goes red the day it is fixed, which is the
/// whole class these proofs exist to remove.
///
/// The discrimination the test turns on is that `latest` is a LEGAL PIN. A
/// manifest can hold it, `Pin::default()` carries it, and an operator may
/// write it by hand. So "pinned to `latest`" and "pinned to nothing" are two
/// different states, and no `String` return could have told them apart — which
/// is why the fix is the return TYPE and not a different default.
#[test]
fn an_unpinned_service_has_no_pin_and_latest_is_still_a_real_one() {
    let mut m = Manifest::default();
    m.set_version("echo", "aaaaaaaaaaaa");
    m.set_version("asked", "latest");

    assert_eq!(m.pin("echo").as_deref(), Some("aaaaaaaaaaaa"), "a pinned label is the label");
    assert_eq!(
        m.pin("asked").as_deref(),
        Some("latest"),
        "`latest` written down IS a pin — a question this environment is asking on purpose"
    );
    assert_eq!(
        m.pin("tally"),
        None,
        "a service absent from env.toml names NO version; `latest` here was the defect"
    );

    // The two states are distinguishable, which is the whole point. Written as
    // an inequality so it cannot pass by both sides being the same wrong thing.
    assert_ne!(
        m.pin("asked"),
        m.pin("tally"),
        "`latest` and `None` must not collapse into one answer: the first says this \
         environment runs the service on whatever is newest, the second says it does not run \
         it at all"
    );

    // And it round-trips: a saved and reloaded manifest still has no entry for
    // a service nothing pinned, rather than acquiring one with a default.
    let text = toml::to_string_pretty(&m).unwrap();
    let back: Manifest = toml::from_str(&text).unwrap();
    assert_eq!(back.pin("tally"), None, "a round trip must not invent an entry");
    assert_eq!(back.pin("asked").as_deref(), Some("latest"));
    assert!(!text.contains("tally"), "an unpinned service is not in the file at all:\n{text}");
}

/// A build record written before commits were the identity must READ, and must
/// say it names no commit rather than present an empty string as one.
#[test]
fn a_build_record_without_a_commit_says_so() {
    let legacy = r#"{"service":"echo","label":"9f3e215dcafebabe","bin":"echo",
        "package":"seam-probe","profile":"dev","workspace":"/w","built_at":1,
        "source_commit":null}"#;
    let r: BuildRecord = serde_json::from_str(legacy).expect("an older build.json must still parse");
    assert!(!r.names_a_commit(), "an absent commit is not a commit");
    assert_eq!(r.commit_short(), "-", "and it is never rendered as one");

    let fresh = BuildRecord {
        service: "echo".into(),
        label: "4197fc6e1d2a".into(),
        bin: "echo".into(),
        package: "seam-probe".into(),
        profile: "dev".into(),
        workspace: "/w".into(),
        built_at: 2,
        commit: COMMIT_A.into(),
        dirty: false,
        content: "0123456789abcdef".into(),
        source_commit: Some(COMMIT_A.into()),
        declaration_sha256: String::new(),
        declaration_source: String::new(),
    };
    assert!(fresh.names_a_commit());
    assert_eq!(fresh.commit_short(), "4197fc6e1d2a");
    let dirty = BuildRecord { dirty: true, ..fresh };
    assert_eq!(dirty.commit_short(), "4197fc6e1d2a+dirty", "dirtiness is never hidden");
    // And a record written before R3.S2.0b names no snapshot: that is
    // `Snapshot::Absent`, not an error, and not a snapshot either.
    assert!(r.declaration_sha256.is_empty(), "a legacy record records no snapshot hash");
}

// ═══════════════════════════ the declaration a build directory carries ═════

/// A hosted-artifact declaration: the form that was measured to die under a
/// changed shipped file (tts, 2026-10-10).
const SNAP_DECL: &str = "name = \"snap\"\nregisters = false\nprobe = \"connect\"\n\
                         [launch]\naddr = \"127.0.0.1:1\"\n[build]\nartifact = \"/bin/sh\"\n\
                         bin = \"sh\"\n";

fn snap_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("cauldron-snap-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn snap_record(sha: &str) -> BuildRecord {
    BuildRecord {
        service: "snap".into(),
        label: "unstamped-0123456789abcdef".into(),
        bin: "sh".into(),
        package: String::new(),
        profile: "dev".into(),
        workspace: "/bin/sh".into(),
        built_at: 3,
        commit: String::new(),
        dirty: false,
        content: "0123456789abcdef".into(),
        source_commit: None,
        declaration_sha256: sha.into(),
        declaration_source: "/somewhere/services/snap.toml".into(),
    }
}

/// **The snapshot is the declaration a build runs under, and it is CHECKED,
/// never trusted.** Present-and-matching loads; a changed byte is refused
/// naming BOTH hashes; a file with no recorded hash is refused as
/// unverifiable; a file that is gone is `Absent` carrying the hash the record
/// still names — the one-line reversal of a backfill, visible as such.
#[test]
fn a_declaration_snapshot_is_checked_against_its_record() {
    use env::decl::{self, Snapshot};
    let dir = snap_dir("checked");
    let sha = decl::write_snapshot(&dir, SNAP_DECL).unwrap();
    assert_eq!(sha, decl::sha256_hex(SNAP_DECL.as_bytes()));
    assert_eq!(sha.len(), 64, "sha256, hex");
    assert_eq!(
        std::fs::read_to_string(dir.join(decl::DECLARATION_SNAPSHOT)).unwrap(),
        SNAP_DECL,
        "byte for byte: comments, order and defaults-not-written survive"
    );

    // PRESENT, matching: the declaration is the snapshot's, loaded_from is the
    // snapshot, and the launch-addr the shipped file might have dropped is here.
    let rec = snap_record(&sha);
    match decl::snapshot(&dir, &rec, "snap").unwrap() {
        Snapshot::Present { decl: d, sha256, source } => {
            assert_eq!(sha256, sha);
            assert_eq!(source, "/somewhere/services/snap.toml");
            assert!(!d.registers);
            assert_eq!(d.launch.as_ref().unwrap().addr, "127.0.0.1:1");
            assert_eq!(d.loaded_from, dir.join(decl::DECLARATION_SNAPSHOT));
            assert_eq!(d.text, SNAP_DECL, "the exact bytes travel on the declaration");
        }
        Snapshot::Absent { .. } => panic!("the snapshot is there"),
    }

    // TAMPERED: one appended comment keeps it parseable and changes the hash.
    // Refused, naming the hash found and the hash recorded.
    std::fs::write(dir.join(decl::DECLARATION_SNAPSHOT), format!("{SNAP_DECL}# tampered\n")).unwrap();
    let e = decl::snapshot(&dir, &rec, "snap").err().expect("a changed snapshot is refused");
    let msg = format!("{e:#}");
    let actual = decl::sha256_hex(format!("{SNAP_DECL}# tampered\n").as_bytes());
    assert!(msg.contains("REFUSED"), "{msg}");
    assert!(msg.contains(&actual), "names the hash of the bytes found: {msg}");
    assert!(msg.contains(&sha), "and the hash the record names: {msg}");

    // UNRECORDED: a file nobody hashed cannot be checked, so it is refused —
    // a hand-copied declaration.toml is not a snapshot.
    std::fs::write(dir.join(decl::DECLARATION_SNAPSHOT), SNAP_DECL).unwrap();
    let e = decl::snapshot(&dir, &snap_record(""), "snap").err().expect("unrecorded is refused");
    assert!(format!("{e:#}").contains("records no `declaration_sha256`"), "{e:#}");

    // GONE: the documented reversal. Absent, and it says the record still
    // names a hash, so `status` can say "GONE" rather than "predates".
    std::fs::remove_file(dir.join(decl::DECLARATION_SNAPSHOT)).unwrap();
    match decl::snapshot(&dir, &rec, "snap").unwrap() {
        Snapshot::Absent { recorded_sha256 } => assert_eq!(recorded_sha256, sha),
        Snapshot::Present { .. } => panic!("the file was deleted"),
    }
    // LEGACY: no file, no hash — a build made before snapshots existed.
    match decl::snapshot(&dir, &snap_record(""), "snap").unwrap() {
        Snapshot::Absent { recorded_sha256 } => assert!(recorded_sha256.is_empty()),
        Snapshot::Present { .. } => panic!("nothing to be present"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A snapshot that does not VALIDATE is refused on the same terms as a
/// shipped file that does not: the parser is shared, so the two cannot drift.
#[test]
fn a_snapshot_that_contradicts_itself_is_refused_like_a_shipped_file() {
    use env::decl;
    let dir = snap_dir("invalid");
    // registers = false with probe = "seam" is the contradiction `validate`
    // refuses by name.
    let bad = SNAP_DECL.replace("probe = \"connect\"", "probe = \"seam\"");
    let sha = decl::write_snapshot(&dir, &bad).unwrap();
    let e = decl::snapshot(&dir, &snap_record(&sha), "snap").err().expect("refused");
    assert!(format!("{e:#}").contains("does not link"), "{e:#}");
    // And a snapshot that names ANOTHER service is refused: a build dir for
    // `snap` carrying `tts`'s declaration is a mix-up, not a contract.
    let other = SNAP_DECL.replace("name = \"snap\"", "name = \"other\"");
    let sha = decl::write_snapshot(&dir, &other).unwrap();
    let e = decl::snapshot(&dir, &snap_record(&sha), "snap").err().expect("refused");
    assert!(format!("{e:#}").contains("names service `other` but was loaded as `snap`"), "{e:#}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ════════════════════════════════════ environments, in the plural ═════════

/// **Two environments are constructed from one root, and the second is never
/// reached by mutating the process environment.**
#[test]
fn for_name_builds_a_sibling_environment_and_validates_it() {
    let root = std::env::temp_dir().join(format!("cauldron-ver-{}", std::process::id()));
    let here = Environment {
        root: root.clone(),
        root_source: env::paths::Source::CauldronHome,
        name: "dev".into(),
        name_source: env::paths::Source::CauldronEnv,
        data_root: root.join("data").join("dev"),
    };

    let other = here.for_name("other").unwrap();
    assert_eq!(other.root, here.root, "the root is carried over, not re-resolved");
    assert_eq!(other.data_root, root.join("data").join("other"));
    assert_ne!(other.data_root, here.data_root);
    // The name's provenance is honest: it came from a listing, not a variable.
    assert_eq!(other.name_source, env::paths::Source::Default);
    println!("sibling: {}", other.banner());

    // FORCED RED 1: the reserved name is refused as an environment here exactly
    // as `resolve` refuses it, so a listing cannot smuggle `shared` in.
    let e = here.for_name("shared").unwrap_err();
    println!("for_name(shared) -> {e:#}");
    assert!(format!("{e:#}").contains("RESERVED"), "{e:#}");
    assert!(RESERVED_ENV_NAMES.contains(&"shared"));

    // FORCED RED 2: an unusable name.
    for bad in ["", "Prod", "a/b", "..", &"x".repeat(33)] {
        let e = here.for_name(bad).unwrap_err();
        println!("for_name({bad:?}) -> {}", format!("{e:#}").lines().next().unwrap_or(""));
        assert!(format!("{e:#}").contains("REFUSED"), "{bad:?} must be refused: {e:#}");
    }
}

/// `environments()` is a LISTING, and the predicate is the same one `resolve`
/// applies — read in the other direction.
#[test]
fn environments_lists_what_exists_and_skips_what_is_not_one() {
    let root = std::env::temp_dir().join(format!("cauldron-ver-list-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let here = Environment {
        root: root.clone(),
        root_source: env::paths::Source::CauldronHome,
        name: "dev".into(),
        name_source: env::paths::Source::CauldronEnv,
        data_root: root.join("data").join("dev"),
    };

    // A missing `data/` is an empty list, not an error: "no environment has
    // been used in this root yet" is a true statement.
    assert!(here.environments().unwrap().is_empty());
    // ...but the environment the caller is STANDING IN is never missing from a
    // listing of environments, even before it has a directory.
    assert_eq!(here.environments_including_self().unwrap(), vec!["dev".to_string()]);

    for d in ["dev", "other", "prod", "shared", "Not-An-Env", "also bad"] {
        std::fs::create_dir_all(root.join("data").join(d)).unwrap();
    }
    std::fs::write(root.join("data").join("a-file-not-a-dir"), "x").unwrap();

    let got = here.environments().unwrap();
    println!("environments -> {got:?}");
    assert_eq!(got, vec!["dev".to_string(), "other".to_string(), "prod".to_string()]);
    assert!(!got.contains(&"shared".to_string()), "the reserved sibling is not an environment");
    assert!(!got.contains(&"Not-An-Env".to_string()), "a name no CAULDRON_ENV could set");
    assert!(!got.contains(&"a-file-not-a-dir".to_string()));
    let _ = std::fs::remove_dir_all(&root);
}

/// **TWO SERVICES OF THE SAME NAME, IN TWO ENVIRONMENTS, AT ONCE** — asserted
/// on the files, with every direction of leakage forced red.
///
/// This is the capability the operator asked for, and the honest finding is
/// that the registry already keyed it: the table lives INSIDE the data root, so
/// the path a reader opens already decides the environment and there is no name
/// to get wrong. What is asserted here is that the key holds in all four
/// directions at once.
#[test]
fn one_service_name_two_environments_neither_reaching_the_other() {
    let root = std::env::temp_dir().join(format!("cauldron-ver-two-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let here = Environment {
        root: root.clone(),
        root_source: env::paths::Source::CauldronHome,
        name: "dev".into(),
        name_source: env::paths::Source::CauldronEnv,
        data_root: root.join("data").join("dev"),
    };
    let a = here.for_name("dev").unwrap();
    let b = here.for_name("other").unwrap();
    for e in [&a, &b] {
        std::fs::create_dir_all(&e.data_root).unwrap();
    }

    // The SAME service name registers in both, each with its own address, its
    // own commit, and its own data root.
    for (e, port, commit) in [(&a, "127.0.0.1:41001", COMMIT_A), (&b, "127.0.0.1:41002", COMMIT_B)]
    {
        seams::update(&e.name, &e.data_root, &e.seams(), |t| {
            let mut s = seam(port, commit);
            s.env = e.name.clone();
            s.data_root = e.data_root.display().to_string();
            s.data_dir = e.service_data("echo").unwrap().display().to_string();
            t.seams.insert("echo".into(), s);
            Ok(())
        })
        .unwrap();
    }

    let ta = seams::Table::load(&a.name, &a.data_root, &a.seams()).unwrap();
    let tb = seams::Table::load(&b.name, &b.data_root, &b.seams()).unwrap();
    assert!(ta.seams.contains_key("echo") && tb.seams.contains_key("echo"), "both exist at once");
    assert_ne!(ta.seams["echo"].addr, tb.seams["echo"].addr, "different ports");
    assert_ne!(ta.seams["echo"].commit, tb.seams["echo"].commit, "different versions");
    assert_eq!(ta.seams["echo"].env, "dev");
    assert_eq!(tb.seams["echo"].env, "other");
    println!(
        "dev/echo   {} commit {}\nother/echo {} commit {}",
        ta.seams["echo"].addr, ta.seams["echo"].commit, tb.seams["echo"].addr,
        tb.seams["echo"].commit
    );

    // FORCED RED, the PATH half: `dev`'s table read as `other` is refused
    // before the file is even opened, because the path is not inside `other`'s
    // data root. Asserted on the SPECIFIC refusal rather than on the word
    // "REFUSED", because both halves below say that word and a test that cannot
    // tell them apart is not testing either.
    let e = seams::Table::load("other", &b.data_root, &a.seams()).unwrap_err();
    println!("dev's table, path-checked against other -> {e:#}");
    assert!(
        format!("{e:#}").contains("outside this environment's data root"),
        "the path half refuses by CONTAINMENT, before any read: {e:#}"
    );

    // FORCED RED, the CONTENT half: the same bytes COPIED into `other`'s own
    // directory, where the path check passes and only the restated `env` can
    // catch it. This is the belt to the path's braces, and it is the half that
    // catches a table somebody moved rather than one somebody mis-addressed.
    let smuggled = b.data_root.join("smuggled.json");
    std::fs::copy(a.seams(), &smuggled).unwrap();
    let e = seams::Table::load(&b.name, &b.data_root, &smuggled).unwrap_err();
    println!("dev's table COPIED into other's data root -> {e:#}");
    assert!(
        format!("{e:#}").contains("A seam table IS its environment"),
        "the content half refuses on the restated env, not on the path: {e:#}"
    );
    assert!(format!("{e:#}").contains("belongs to environment `dev`"), "{e:#}");

    // FORCED RED, the structural half: neither environment can be handed a path
    // in the other's data root. This is `require_inside`, which refuses by
    // CONTAINMENT and not by name — so it refuses an environment nobody listed.
    let e = env::guard::require_inside(&a.data_root, &b.data_root.join("echo")).unwrap_err();
    println!("dev asked for other's echo dir -> {e:#}");
    assert!(format!("{e:#}").contains("outside this environment's data root"), "{e:#}");
    let e = env::guard::require_inside(&b.data_root, &a.data_root.join("echo")).unwrap_err();
    assert!(format!("{e:#}").contains("outside this environment's data root"), "{e:#}");

    // THE POSITIVE SENTINEL: each environment CAN be handed its own. A guard
    // that refused everything would pass both assertions above.
    assert!(env::guard::require_inside(&a.data_root, &a.data_root.join("echo")).is_ok());
    assert!(env::guard::require_inside(&b.data_root, &b.data_root.join("echo")).is_ok());

    // And the listing sees both.
    assert_eq!(here.environments().unwrap(), vec!["dev".to_string(), "other".to_string()]);
    let _ = std::fs::remove_dir_all(&root);
}

// ══════════════════════ the service side: nothing is defaulted ════════════

/// **A service refuses to run without a version identity.**
///
/// One function, in order, because these mutate the PROCESS environment — see
/// the module note.
#[test]
fn a_service_without_a_commit_refuses_to_start() {
    let root = std::env::temp_dir().join(format!("cauldron-ver-svc-{}", std::process::id()));
    let data_root = root.join("data").join("dev");
    let data_dir = data_root.join("echo");
    std::fs::create_dir_all(&data_dir).unwrap();

    let set_all = || {
        std::env::set_var("CAULDRON_SERVICE", "echo");
        std::env::set_var("CAULDRON_ENV", "dev");
        std::env::set_var("CAULDRON_DATA_ROOT", &data_root);
        std::env::set_var("CAULDRON_SERVICE_DATA_DIR", &data_dir);
        std::env::set_var("CAULDRON_SEAMS", data_root.join("seams.json"));
        std::env::set_var("CAULDRON_BOOT_TOKEN", "tok");
        std::env::set_var("CAULDRON_VERSION", "4197fc6e1d2a");
        std::env::set_var("CAULDRON_COMMIT", COMMIT_A);
        std::env::set_var("CAULDRON_DIRTY", "0");
        std::env::set_var("CAULDRON_BIND", "127.0.0.1:0");
    };

    // 1. THE POSITIVE SENTINEL FIRST. A complete environment resolves, and the
    //    commit arrives. Without this, every refusal below could be a refusal
    //    of everything.
    set_all();
    let se = seams::ServiceEnv::from_env().expect("a complete environment must resolve");
    assert_eq!(se.commit, COMMIT_A);
    assert!(!se.dirty);
    // ...and it reaches the HANDSHAKE, which is what makes the registry's claim
    // checkable from outside the process.
    let id = se.identity(&"127.0.0.1:1".parse().unwrap());
    assert_eq!(id.commit, COMMIT_A, "the commit must be in what the service answers HELLO with");
    println!("1. complete -> commit {}", se.commit);

    // 2. FORCED RED: no commit at all. `CAULDRON_COMMIT` is required like every
    //    other variable here, because a service that can default its version
    //    identity can register a row the registry is wrong about.
    set_all();
    std::env::remove_var("CAULDRON_COMMIT");
    let e = seams::ServiceEnv::from_env().unwrap_err();
    println!("2. no commit -> {e:#}");
    assert!(format!("{e:#}").contains("REFUSED TO START"), "{e:#}");
    assert!(format!("{e:#}").contains("CAULDRON_COMMIT"), "it must name the variable: {e:#}");

    // 3. FORCED RED: an empty commit is the same as no commit. An empty string
    //    is not a version identity.
    set_all();
    std::env::set_var("CAULDRON_COMMIT", "   ");
    let e = seams::ServiceEnv::from_env().unwrap_err();
    println!("3. blank commit -> {}", format!("{e:#}").lines().next().unwrap_or(""));
    assert!(format!("{e:#}").contains("CAULDRON_COMMIT"), "{e:#}");

    // 4. FORCED RED: an unparseable dirty flag is NOT read as clean. A flag
    //    whose broken value means "fine" reports clean when it is broken.
    set_all();
    std::env::set_var("CAULDRON_DIRTY", "maybe");
    let e = seams::ServiceEnv::from_env().unwrap_err();
    println!("4. CAULDRON_DIRTY=maybe -> {e:#}");
    assert!(format!("{e:#}").contains("not a yes or a no"), "{e:#}");
    assert!(!format!("{e:#}").contains("REFUSED TO START: this is a cauldron service"));

    // 5. ...and the flag's real values work, in both directions, so (4) is not
    //    a refusal of everything.
    for (word, want) in [("1", true), ("true", true), ("0", false), ("false", false)] {
        set_all();
        std::env::set_var("CAULDRON_DIRTY", word);
        let se = seams::ServiceEnv::from_env().unwrap();
        assert_eq!(se.dirty, want, "CAULDRON_DIRTY={word}");
    }
    println!("5. dirty flag parses 1/true/0/false");

    for k in seams::SERVICE_VARS {
        std::env::remove_var(k);
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// **R9.C4 (gap C-4): ROLLBACK PREFERS A RELEASE BUILD.** tts was pinned to a
/// `-release` build with only dev and unstamped builds behind it, so a
/// rollback would have landed on a debug build. When any release build other
/// than the pin is on the good list, the target is the newest such one — even
/// when a dev or unstamped build was proven good more recently. When none is,
/// the target is the newest non-pinned good build of any kind (a proven dev
/// build beats parking), which is what it always was.
#[test]
fn rollback_target_skips_dev_and_unstamped_builds_when_a_release_build_is_good() {
    let mut m = Manifest::default();
    // good, newest first: [dev, unstamped, release]
    m.record_good("tts", "111111111111-release");
    m.record_good("tts", "unstamped-0123456789abcdef");
    m.record_good("tts", "222222222222-dev");
    assert_eq!(m.good("tts")[0], "222222222222-dev", "premise: a dev build is the newest good one");
    assert_eq!(
        m.rollback_target("tts", "333333333333-release").as_deref(),
        Some("111111111111-release"),
        "a rollback skipped the good release build for a newer dev one"
    );

    // [release, dev]: the release build, whichever way round.
    let mut m2 = Manifest::default();
    m2.record_good("tts", "222222222222-dev");
    m2.record_good("tts", "111111111111-release");
    assert_eq!(m2.rollback_target("tts", "333333333333-release").as_deref(), Some("111111111111-release"));

    // A DIRTY release build is not a release build.
    let mut m3 = Manifest::default();
    m3.record_good("tts", "111111111111-release");
    m3.record_good("tts", "222222222222-release-dirty.abcdef01");
    assert_eq!(m3.rollback_target("tts", "333333333333-release").as_deref(), Some("111111111111-release"));

    // OPPOSING: no release build other than the pin — the newest non-pinned
    // good build of any kind, not None (and not the pin).
    let mut m4 = Manifest::default();
    m4.record_good("tts", "aaaaaaaaaaaa-dev");
    m4.record_good("tts", "bbbbbbbbbbbb-dev");
    assert_eq!(m4.rollback_target("tts", "333333333333-release").as_deref(), Some("bbbbbbbbbbbb-dev"));
    assert_eq!(m4.rollback_target("tts", "bbbbbbbbbbbb-dev").as_deref(), Some("aaaaaaaaaaaa-dev"));
    let mut m5 = Manifest::default();
    m5.record_good("tts", "aaaaaaaaaaaa-dev");
    m5.record_good("tts", "111111111111-release");
    assert_eq!(
        m5.rollback_target("tts", "111111111111-release").as_deref(),
        Some("aaaaaaaaaaaa-dev"),
        "the pinned release build is the one failing; the only other good build is dev"
    );
}

// ───────────────────────────── REL-4.S8c: `latest` is per environment (N2)

fn put_build(dir: &std::path::Path, label: &str, built_at: u64) {
    let mut r = snap_record("");
    r.label = label.into();
    r.built_at = built_at;
    std::fs::create_dir_all(dir.join(label)).unwrap();
    std::fs::write(dir.join(label).join("build.json"), serde_json::to_string_pretty(&r).unwrap())
        .unwrap();
}

fn latest(dir: &std::path::Path, env_name: &str) -> String {
    env::decl::resolve_version(dir, "latest", env_name).unwrap().0
}

/// **Another environment's per-environment copy never moves `latest`; the
/// environment's OWN copy is its `latest`.** Inspect REL-4.S8b-I N2: the
/// `<label>+<env>` copy carried its source's `built_at`, so `latest` TIED and
/// `readdir` order decided whether prod ran the shared build or dev's
/// ad-hoc-signed copy. Each arm is opposed: dev gets its copy where prod does
/// not, prod gets its own copy where dev does not, and a newer SHARED build
/// still wins for both (`latest` still means newest).
#[test]
fn latest_is_per_environment_and_another_environments_copy_never_moves_it() {
    let d = snap_dir("latest-per-env");
    // The tie S8b produced: a copy with its source's timestamp.
    put_build(&d, "abc-release", 100);
    put_build(&d, "abc-release+dev", 100);
    assert_eq!(latest(&d, "dev"), "abc-release+dev", "a tie goes to the environment's own copy");
    assert_eq!(latest(&d, "prod"), "abc-release", "dev's copy moved prod's latest");
    // A NEWER copy for dev (`build --env-copy` stamps its own built_at).
    put_build(&d, "abc-release+dev.2", 200);
    assert_eq!(latest(&d, "dev"), "abc-release+dev.2");
    assert_eq!(latest(&d, "prod"), "abc-release", "dev's newer copy moved prod's latest");
    assert_eq!(latest(&d, "staging"), "abc-release", "dev's copy moved staging's latest");
    // prod's own copy is prod's candidate, and not dev's.
    put_build(&d, "abc-release+prod", 150);
    assert_eq!(latest(&d, "prod"), "abc-release+prod");
    assert_eq!(latest(&d, "dev"), "abc-release+dev.2");
    // A newer shared build is everybody's latest.
    put_build(&d, "def-release", 300);
    assert_eq!(latest(&d, "dev"), "def-release");
    assert_eq!(latest(&d, "prod"), "def-release");
    // An exact pin is unaffected by whose copy it is.
    assert_eq!(
        env::decl::resolve_version(&d, "abc-release+dev", "prod").unwrap().0,
        "abc-release+dev"
    );
    // A tie with a shared build of ANOTHER commit: the own copy wins it on
    // being the environment's own, not on sorting after its source's name.
    let d2 = snap_dir("latest-tie-other-commit");
    put_build(&d2, "zzz-release", 100);
    put_build(&d2, "abc-release+dev", 100);
    assert_eq!(latest(&d2, "dev"), "abc-release+dev", "a tie went to the shared build");
    assert_eq!(latest(&d2, "prod"), "zzz-release");
    let _ = std::fs::remove_dir_all(&d2);
    // The owner parse the filter rests on.
    use env::decl::env_copy_owner as owner;
    assert_eq!(owner("abc-release"), None);
    assert_eq!(owner("abc-release+dev"), Some("dev"));
    assert_eq!(owner("abc-release+dev.12"), Some("dev"));
    assert_eq!(owner("abc-release-dirty.0123abcd+dev.2"), Some("dev"));
    assert_eq!(env::decl::env_copy_label("abc-release", "dev", 1), "abc-release+dev");
    assert_eq!(env::decl::env_copy_label("abc-release", "dev", 3), "abc-release+dev.3");
    let _ = std::fs::remove_dir_all(&d);
}
