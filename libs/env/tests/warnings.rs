//! The warnings CONTRACT, at the unit level.
//!
//! Three obligations are proved here and nowhere else, because they are
//! properties of the types rather than of a running service:
//!
//! 1. **the supervisor's socket vocabulary is closed at two words**, and
//!    neither of them is a data verb;
//! 2. **absent is not exempt** — a declaration with no `warnings` key is still
//!    expected to serve the default shape, so a service cannot be excused by
//!    omission;
//! 3. **every refusal names what is missing.** A refusal that does not say
//!    what to add makes the contract a riddle, so this is asserted over the
//!    whole set of refusal variants rather than per call site.
//!
//! The end-to-end halves — a compliant service starting and reaching the
//! aggregate, a non-compliant one being refused — are in
//! `scripts/prove-warnings`, which needs real processes and real sockets.

use env::decl::{Declaration, WarningsDecl};
use env::seams::{Seam, SeamState, Verdict};
use env::warnings::{Aggregate, Compliance, Export, Kind, SupervisorAsk, SHAPE};

// ───────────────────────────────── the closed vocabulary

#[test]
fn the_supervisor_vocabulary_is_closed_at_two_words() {
    // The compile-time half of this lives in `env::warnings::ALL_IS_EXHAUSTIVE`
    // and a third variant does not build. This is the half that names the two
    // words, so that a RENAME is caught as well as an ADDITION.
    assert_eq!(SupervisorAsk::ALL.len(), 2, "the supervisor may say exactly two words");
    let words: Vec<&str> = SupervisorAsk::ALL.iter().map(|a| a.word()).collect();
    assert_eq!(words, vec!["HELLO", "WARNINGS"]);
    assert_eq!(env::warnings::ALL_IS_EXHAUSTIVE.len(), SupervisorAsk::ALL.len());
}

#[test]
fn no_word_the_supervisor_may_say_asks_for_data() {
    // Not a lint: these are the verbs a relay would plausibly be given, and
    // `is_supervisor_word` is the function the reference service uses to label
    // a connection, so this is the exact predicate the end-to-end proof
    // partitions on.
    for relay_verb in ["DATA", "FRAME", "FRAMES", "SUBSCRIBE", "RELAY", "FORWARD", "PROXY", "TAIL"]
    {
        assert!(
            !SupervisorAsk::is_supervisor_word(relay_verb),
            "`{relay_verb}` must not be a word the supervisor is allowed to say: the supervisor \
             subscribes to warnings and is never in the data path"
        );
    }
    assert!(SupervisorAsk::is_supervisor_word("HELLO"));
    assert!(SupervisorAsk::is_supervisor_word("WARNINGS"));
}

// ───────────────────────────────── absent is not exempt

fn decl(toml_body: &str) -> Declaration {
    toml::from_str(&format!(
        "name = \"svc\"\n{toml_body}\n[build]\nworkspace = \"/tmp/ws\"\npackage = \"p\"\nbin = \"b\"\n"
    ))
    .expect("the declaration should parse")
}

#[test]
fn a_declaration_with_no_warnings_key_still_owes_the_default_shape() {
    let d = decl("");
    assert_eq!(d.warnings, WarningsDecl::Shape(SHAPE.to_string()));
    assert_eq!(d.warnings_shape(), Some("warnings"));
}

#[test]
fn warnings_false_is_the_only_spelling_of_non_compliance() {
    assert_eq!(decl("warnings = false").warnings_shape(), None);
    // `true` is the default shape said out loud, not a second way to be exempt.
    assert_eq!(decl("warnings = true").warnings_shape(), Some("warnings"));
    assert_eq!(decl("warnings = \"warnings\"").warnings_shape(), Some("warnings"));
    assert_eq!(decl("warnings = \"diagnostics\"").warnings_shape(), Some("diagnostics"));
}

#[test]
fn an_empty_shape_name_is_refused_rather_than_read_as_false() {
    // The hole this closes: `warnings = ""` looks like a way of declaring
    // nothing, and reading it as `false` would be a silent exemption.
    let e = toml::from_str::<Declaration>(
        "name = \"svc\"\nwarnings = \"\"\n[build]\nworkspace = \"/tmp/ws\"\npackage = \"p\"\nbin = \"b\"\n",
    )
    .expect_err("an empty shape name must not parse");
    let text = e.to_string();
    assert!(text.contains("not a shape name"), "{text}");
    assert!(text.contains("warnings = false"), "the error must name the real spelling: {text}");
}

#[test]
fn a_declaration_round_trips_the_refusal_rather_than_losing_it() {
    // Serialised back out, `warnings = false` stays `false`. A round trip that
    // lost it would turn a confessed non-compliant service into a compliant
    // one on any rewrite.
    let d = decl("warnings = false");
    let s = toml::to_string_pretty(&d).unwrap();
    assert!(s.contains("warnings = false"), "{s}");
    assert_eq!(toml::from_str::<Declaration>(&s).unwrap().warnings_shape(), None);
}

// ───────────────────────────────── every refusal names what is missing

fn export(service: &str, warn: u64, error: u64) -> Export {
    Export {
        shape: SHAPE.into(),
        shape_version: 1,
        producer: service.into(),
        counts_since: "1760000000".into(),
        warn,
        error,
        sink_errors: 0,
        sink_attached: true,
        kinds: vec![Kind {
            target: "testkit/seam-probe/src/main.rs".into(),
            line: 42,
            level: "warn".into(),
            count: warn,
            message: "unknown request `FOO`".into(),
        }],
    }
}

#[test]
fn every_refusal_names_what_is_missing_and_no_success_does() {
    let refusals = vec![
        Compliance::RefusedByDeclaration { service: "svc".into() },
        Compliance::NotServed { shape: "warnings".into(), detail: "ERR unknown request".into() },
        Compliance::WrongShape { asked: "warnings".into(), answered: "gossip".into() },
        Compliance::WrongProducer { expected: "echo".into(), answered: "tally".into() },
        Compliance::Unparseable { detail: "expected value".into() },
    ];
    for c in &refusals {
        let m = c.missing().unwrap_or_else(|| panic!("{c:?} must name what is missing"));
        assert!(m.len() > 40, "a refusal must be a sentence, not a token: {m}");
        assert!(c.is_violation(), "{c:?} is a contract failure");
    }
    // The two that are NOT failures, and they must stay distinguishable: one
    // is compliance, the other is only the absence of anything to ask.
    for ok in [
        Compliance::Served { export: Box::new(export("svc", 0, 0)) },
        Compliance::NoProducer { state: "down".into() },
    ] {
        assert!(ok.missing().is_none(), "{ok:?} is not a refusal");
        assert!(!ok.is_violation());
    }
}

#[test]
fn a_refusal_for_a_missing_shape_prints_the_answer_the_service_owes() {
    // The cost of refusing instead of warning is a two-step for every new
    // service; what keeps it bounded is that the refusal hands over the exact
    // line to answer with. If this assertion goes, that argument goes with it.
    let m = Compliance::NotServed { shape: "warnings".into(), detail: "ERR".into() }
        .missing()
        .unwrap();
    for owed in ["\"shape\":\"warnings\"", "\"producer\"", "\"counts_since\"", "\"warn\""] {
        assert!(m.contains(owed), "the refusal must show `{owed}`: {m}");
    }
}

// ───────────────────────────────── the check does not trust

fn seam(service: &str, addr: &str) -> Seam {
    Seam {
        service: service.into(),
        addr: addr.into(),
        pid: 1,
        boot_token: "t".into(),
        version: "v".into(),
        commit: "4197fc6e1d2a3b4c5d6e7f8091a2b3c4d5e6f701".into(),
        dirty: false,
        binary: "b".into(),
        env: "dev".into(),
        data_root: "/tmp/r".into(),
        data_dir: "/tmp/r/s".into(),
        started_at: 0,
        probe: "seam".into(),
        // A REGISTERED seam: written BY the service, from inside itself,
        // after its own bind. Spelled out rather than defaulted so that a
        // test about an ADOPTED seam cannot be written by accident.
        registered: true,
        pid_source: env::seams::PidSource::SELF.into(),
        health: String::new(),
    }
}

fn verdict(state: SeamState) -> Verdict {
    Verdict { state, detail: String::new(), identity: None, pid_alive: Some(true) }
}

#[test]
fn a_declaration_that_serves_nothing_never_reaches_a_socket() {
    // The address is deliberately unroutable. If `check` tried to ask, this
    // test would either hang or fail; it returns instantly because a
    // declaration that confesses non-compliance has nothing to be asked.
    let c = env::warnings::check(&seam("svc", "127.0.0.1:1"), &verdict(SeamState::Live), None);
    assert!(matches!(c, Compliance::RefusedByDeclaration { .. }));
}

#[test]
fn a_down_seam_is_not_a_contract_violation() {
    let c = env::warnings::check(
        &seam("svc", "127.0.0.1:1"),
        &verdict(SeamState::Down),
        Some("warnings"),
    );
    assert!(matches!(c, Compliance::NoProducer { .. }));
    assert!(!c.is_violation(), "a service that is not running has not broken the contract");
}

#[test]
fn a_stale_seam_is_never_asked_for_warnings() {
    // Attribution: a stale seam's address may be held by anybody, and crediting
    // a stranger's counts to this service would be worse than having none.
    for state in [SeamState::StaleNoListener, SeamState::StaleWrongOwner, SeamState::StaleWrongEnv]
    {
        let c = env::warnings::check(
            &seam("svc", "127.0.0.1:1"),
            &verdict(state.clone()),
            Some("warnings"),
        );
        assert!(matches!(c, Compliance::NoProducer { .. }), "{state:?} must not be asked");
    }
}

#[test]
fn a_service_that_is_not_listening_is_a_violation_not_an_absence() {
    // Live by verdict, unreachable in fact: the seam says there is a producer,
    // so a failure to answer is the contract failing and not the service being
    // absent. Port 1 on loopback refuses instantly.
    let c = env::warnings::check(
        &seam("svc", "127.0.0.1:1"),
        &verdict(SeamState::Live),
        Some("warnings"),
    );
    assert!(matches!(c, Compliance::NotServed { .. }), "{c:?}");
    assert!(c.is_violation());
    assert!(c.missing().unwrap().contains("warnings"));
}

// ───────────────────────────────── the aggregate

#[test]
fn the_aggregate_states_its_own_coverage_and_names_its_gaps() {
    let mut a = Aggregate::default();
    a.add("echo", "warnings", &Compliance::Served { export: Box::new(export("echo", 3, 1)) });
    a.add("tally", "warnings", &Compliance::Served { export: Box::new(export("tally", 10, 0)) });
    a.add(
        "mute",
        "warnings",
        &Compliance::NotServed { shape: "warnings".into(), detail: "ERR".into() },
    );
    a.add("off", "warnings", &Compliance::NoProducer { state: "down".into() });

    assert_eq!(a.warn, 13);
    assert_eq!(a.error, 1);
    assert_eq!(a.total, 14);
    // A total whose coverage is unknown is a number that looks complete and is
    // not — which is how a 47 MB log fooled everybody for five weeks.
    assert_eq!(a.coverage(), (2, 4));
    assert_eq!(a.violations, 1, "the absent service is a gap, not a violation");
    let mute = a.gaps.iter().find(|g| g.service == "mute").unwrap();
    assert_eq!(mute.kind, "violation");
    assert!(mute.reason.contains("does not serve"), "{}", mute.reason);
    let off = a.gaps.iter().find(|g| g.service == "off").unwrap();
    assert_eq!(off.kind, "no-producer");
}

#[test]
fn warn_and_error_are_never_summed_into_one_number_on_the_wire() {
    // Summing in the producer or the aggregate destroys the distinction with no
    // way back: 40 warnings and 40 errors are different sentences. `total()` is
    // the FACE's convenience and is carried beside the split, never instead.
    let mut a = Aggregate::default();
    a.add("echo", "warnings", &Compliance::Served { export: Box::new(export("echo", 7, 2)) });
    assert_eq!((a.warn, a.error, a.total), (7, 2, 9));
    assert_eq!(a.producers[0].warn, 7);
    assert_eq!(a.producers[0].error, 2);
}

#[test]
fn the_breakdown_is_ordered_by_count_and_carries_the_producer() {
    // Two services can warn from the same file and line — the fixture is one
    // binary run twice — so a kind without its producer is two defects merged.
    let mut a = Aggregate::default();
    a.add("echo", "warnings", &Compliance::Served { export: Box::new(export("echo", 2, 0)) });
    a.add("tally", "warnings", &Compliance::Served { export: Box::new(export("tally", 9, 0)) });
    assert_eq!(a.top_kinds.len(), 2);
    assert_eq!(a.top_kinds[0].service, "tally");
    assert_eq!(a.top_kinds[0].count, 9);
    assert_eq!(a.top_kinds[1].service, "echo");
}

#[test]
fn an_export_is_parsed_from_exactly_what_the_reference_shape_publishes() {
    // The field names are the reference implementation's. A service that
    // already serves that shape re-encodes rather than re-derives, so this is
    // the wire compatibility assertion.
    let line = r#"{"shape":"warnings","shape_version":1,"producer":"echo",
      "counts_since":"1760000000","warn":29644,"error":53,"sink_errors":0,
      "sink_attached":true,
      "kinds":[{"target":"threads::runtime","line":812,"level":"error","count":53,
                "message":"update: A RESTART HAS BEEN PENDING FOR 10 MINUTES"}]}"#;
    let e: Export = serde_json::from_str(line).unwrap();
    assert_eq!(e.warn, 29644);
    assert_eq!(e.error, 53);
    assert_eq!(e.total(), 29697);
    assert_eq!(e.kinds[0].line, 812);
    // The number that made the case: one call site, 18 message strings, 53
    // records. Keyed by call site it is ONE kind.
    assert_eq!(e.kinds[0].count, 53);
}

// ──────────────────────── the PRODUCER's compile-time obligation
//
// The headline red — a producer with no `ServesWarnings` impl does not build —
// cannot be a `#[test]`, because a test that fails to compile is not a test.
// It lives in `scripts/prove-compile-gate`, which deletes the impl from the
// reference service, captures what rustc says, and restores it.
//
// What CAN be asserted here are the `const fn`s those compile-time assertions
// are built out of, plus the one property that makes them worth having: that
// they are evaluated in `const` context and not at runtime.

/// **THE CHECKERS RUN IN `const` CONTEXT, WHICH IS THE WHOLE CLAIM.**
///
/// If either of these were an ordinary function these items would not compile,
/// so their presence is the evidence that `attest_warnings!`'s assertions are
/// BUILD failures and not runtime ones. (Copied in spirit from
/// `shapewire`'s `operation::tests::_PROOF`, which makes the same argument
/// about `well_formed`.)
const _SHAPE_CHECK_IS_CONST: bool = env::warnings::shape_is_usable("warnings");
const _WORD_CHECK_IS_CONST: bool = env::warnings::is_supervisor_word_const("WARNINGS");

#[test]
fn a_shape_name_the_declaration_would_reject_is_rejected_on_the_producer_side_too() {
    use env::warnings::shape_is_usable;
    // The exact rule `decl::WarningsDecl` applies to `warnings = "<name>"`.
    // Stated here as well because the two are enforced by completely different
    // machinery -- a serde deserializer and a `const` assertion -- and the one
    // thing that must be true of them is that they agree.
    for good in ["warnings", "diagnostics", "threads/warnings", "my-shape", "my_shape", "v2"] {
        assert!(shape_is_usable(good), "`{good}` is a usable shape name");
        assert!(
            toml::from_str::<Declaration>(&format!(
                "name = \"svc\"\nwarnings = \"{good}\"\n[build]\nworkspace = \"/tmp/ws\"\n\
                 package = \"p\"\nbin = \"b\"\n"
            ))
            .is_ok(),
            "and the DECLARATION parser agrees about `{good}`"
        );
    }
    // An empty shape name is the case that matters: it is the spelling of "I
    // declared something" that says nothing, and it is refused on both sides.
    // `warnings = false` is the only real spelling of non-compliance.
    for bad in ["", " ", "shape name", "shape!", "shape.name", "sh@pe"] {
        assert!(!shape_is_usable(bad), "`{bad}` must not be a usable shape name");
    }
}

#[test]
fn the_shadowing_check_names_exactly_the_supervisors_own_two_words() {
    use env::warnings::is_supervisor_word_const;
    // `shadows_a_supervisor_word` is this predicate applied over a verb table.
    // Asserting the predicate against `SupervisorAsk::ALL` rather than against
    // a literal list is what keeps the compile-time assertion single-sourced:
    // a renamed variant moves both at once.
    for a in SupervisorAsk::ALL {
        assert!(
            is_supervisor_word_const(a.word()),
            "`{}` is a word the supervisor owns, so a service may not claim it",
            a.word()
        );
        // The runtime twin the reference service uses to label a connection has
        // to agree, or the proof's partition and the build check would disagree
        // about what "supervisor-shaped" means.
        assert!(SupervisorAsk::is_supervisor_word(a.word()));
    }
    // And the verbs a RELAY would plausibly be given are not supervisor words,
    // so a service is free to serve them -- which is why the data path in the
    // reference service is legal and the assertion about it is not vacuous.
    for own in ["DATA", "FRAME", "SUBSCRIBE", "WARN", "ERROR", "PING", "ENV", "WHOAMI"] {
        assert!(!is_supervisor_word_const(own), "`{own}` is a service's own verb to serve");
    }
}

#[test]
fn an_attested_shape_is_the_one_the_producer_declared() {
    // A stand-in producer, implemented here rather than imported, so this test
    // is the SECOND implementation of the trait in the repo. One implementation
    // proves a trait can be satisfied; two prove the obligation is not shaped
    // around the reference service.
    struct Stub;
    impl env::warnings::ServesWarnings for Stub {
        const SHAPE: &'static str = "diagnostics";
        fn warnings(&self, _asked: &str) -> anyhow::Result<Export> {
            anyhow::bail!("a stub serves nothing")
        }
    }
    impl env::warnings::Service for Stub {
        const VERBS: &'static [env::warnings::Verb<Stub>] = &[];
        fn service_env(&self) -> &env::seams::ServiceEnv {
            unreachable!("a stub never registers")
        }
        fn addr(&self) -> std::net::SocketAddr {
            unreachable!("a stub never binds")
        }
    }
    let attested = env::warnings::attest_warnings!(Stub);
    assert_eq!(
        attested.shape(),
        "diagnostics",
        "the attestation carries the shape the producer declared at compile time"
    );
    assert_ne!(
        attested.shape(),
        SHAPE,
        "and it is the PRODUCER's declaration, not the contract's default"
    );
}
