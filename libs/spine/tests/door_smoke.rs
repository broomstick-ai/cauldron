//! The door, end to end.
//!
//! Everything here goes through `Cauldron::submit` / `Cauldron::read` — the
//! ONE write path — against the two cross-schema
//! fixtures, alpha (pinned v1) and beta (pinned v2). Assertions are over
//! STRUCTURE: a witness variant, a delta list, an outcome, a counter. No
//! test in this file asserts on prose.
//!
//! Every checker here has been forced RED at least once by a real poison
//! applied to real source, compiled and run; the poison is named in a
//! `// RED:` comment above each test.
//!
//! # Environment serialization
//!
//! One test sets `SPINE_FAULT`, which is process-global while the harness
//! runs tests in threads. Every test in this file takes [`env_shared`] and
//! that one takes [`env_exclusive`], so the fault-seam test can never run
//! beside a door call that would refuse because of its variable.

use spine::config::SpineConfig;
use spine::delta::CompiledPayload;
use spine::envelope::{Bindings, Envelope, Initiator, Instrument, Mode, Provenance};
use spine::error::SpineError;
use spine::locks::{self, Resource};
use spine::pool::NodeId;
use spine::schema_def::{fixtures, PoolName};
use spine::spine_db::{SpineDb, SPINE_FILE};
use spine::witness::{ExpectationKind, Witness};
use spine::{Cauldron, Outcome, ReadRequest, Returned};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

test_scratch::install!();

const ALPHA: &str = "fixture-alpha";
const BETA: &str = "fixture-beta";

// ---------------------------------------------------------------------------
// the worked supersede statement, end to end
// ---------------------------------------------------------------------------

/// The worked supersede example, spelled in the frozen syntax and run
/// against a real cauldron: match the doc, expect its revision, create the
/// successor, create the `supersedes` edge, set the old doc's state, return
/// the new identifier and the old revision. ONE statement, ONE commit, ONE
/// spine row — the create, the edge and the state change never observable
/// separately.
///
/// The RETURN is projected from the LANDED state, which is what
/// `REV(old) == 2` proves: the state change bumped it inside this very
/// commit, and a projection taken from the match snapshot would still say 1.
// RED: (a) `project_landed` projecting from `matched` instead of re-reading
// the pools — REV(old) came back 1; (b) dropping the `SET` delta from
// `MUTATION_TABLE` — old.state stayed draft; (c) `mark_cleared` before the
// last `mark_part_landed` — the lifecycle guard refused and the whole
// submit failed.
#[test]
fn the_worked_supersede_lands_as_one_commit_and_returns_the_landed_state() {
    let _env = env_shared();
    let c = cauldron("supersede");
    let old_id = create_doc(&c, "the original");

    let before = c.committed_commit_counter().expect("counter");

    let out = c
        .submit(commit(
            r#"MATCH (old: fixture-alpha/doc { id: $old_id })
               EXPECT REV(old) = 1
               CREATE (new: fixture-alpha/doc { title: $title, state: "draft" })
               CREATE (new) -[supersedes]-> (old)
               SET old.state = "superseded"
               RETURN new.id AS new_id, REV(old) AS old_rev"#,
            &[
                ("old_id", serde_json::json!(old_id.0)),
                ("title", serde_json::json!("the successor")),
            ],
        ))
        .expect("the supersede commits");

    let (tx, seq, returned) = match out {
        Outcome::Committed {
            tx,
            seq,
            returned,
            deduplicated,
        } => {
            assert!(!deduplicated, "a fresh ask is not a replay");
            (tx, seq, returned)
        }
        other => panic!("expected Committed, got {other:?}"),
    };

    // ONE commit: the counter moved exactly once for the whole statement.
    assert_eq!(c.committed_commit_counter().expect("counter"), before + 1);
    // ONE spine row, cleared, touching exactly one pool.
    assert_eq!(
        c.touched_pools(&tx).expect("touched"),
        vec![PoolName::new(ALPHA)]
    );
    assert!(
        c.in_landing(None).expect("in landing").is_empty(),
        "a cleared commit is not in landing"
    );

    // The RETURN projection, from the landed state: one row, the minted
    // identifier under its alias, and the old doc's revision AFTER the set.
    assert_eq!(returned.0.len(), 1, "one matched row, one returned row");
    let row = &returned.0[0];
    let new_id = row["new_id"].as_str().expect("new_id is text").to_string();
    assert_ne!(new_id, old_id.0, "the successor is a different node");
    assert_eq!(row["old_rev"], serde_json::json!(2), "SET bumped the revision");

    // All three effects landed, and they landed together.
    assert_eq!(state_of(&c, &old_id.0), Some("superseded".into()));
    assert_eq!(state_of(&c, &new_id), Some("draft".into()));
    assert_eq!(
        title_of(&c, &new_id),
        Some("the successor".into()),
        "the CREATE's own fields landed"
    );

    // LOCKS RELEASE AT THE CLEARED FLIP ONLY, and by then they are gone.
    let spine = SpineDb::open(&c_root("supersede").join(SPINE_FILE)).expect("spine");
    for resource in [
        Resource::Body {
            node: old_id.clone(),
        },
        Resource::Body {
            node: NodeId(new_id.clone()),
        },
    ] {
        assert!(
            locks::queue(&spine, &PoolName::new(ALPHA), &resource)
                .expect("queue")
                .is_empty(),
            "every lock of seq {seq} released at the cleared flip"
        );
    }
}

/// A revision that MOVED while the author was thinking refuses the WHOLE
/// statement, naming the expectation, the value expected and the value found
/// (the specified witness shape).
// RED: deleting the `found != Some(want)` arm of `emit_expect_revision` —
// the stale statement then committed over a moved node.
#[test]
fn a_moved_revision_refuses_with_expected_versus_found_and_lands_nothing() {
    let _env = env_shared();
    let c = cauldron("moved-revision");
    let old_id = create_doc(&c, "the original");
    // Somebody else moves it: revision 1 becomes revision 2.
    c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t"#,
        &[
            ("id", serde_json::json!(old_id.0)),
            ("t", serde_json::json!("edited by somebody else")),
        ],
    ))
    .expect("the interfering edit commits");

    let before = c.committed_commit_counter().expect("counter");
    let err = c
        .submit(commit(
            r#"MATCH (old: fixture-alpha/doc { id: $old_id })
               EXPECT REV(old) = 1
               CREATE (new: fixture-alpha/doc { title: $title, state: "draft" })
               CREATE (new) -[supersedes]-> (old)
               SET old.state = "superseded"
               RETURN new.id AS new_id"#,
            &[
                ("old_id", serde_json::json!(old_id.0)),
                ("title", serde_json::json!("the successor")),
            ],
        ))
        .expect_err("a stale premise refuses");

    match only_witness(&err) {
        Witness::FailedExpectation {
            expectation,
            expected,
            found,
        } => {
            assert_eq!(
                expectation,
                ExpectationKind::Revision {
                    binding: "old".into(),
                    field: None
                }
            );
            assert_eq!(expected, serde_json::json!(1));
            assert_eq!(found, serde_json::json!(2));
        }
        other => panic!("expected FailedExpectation, got {other:?}"),
    }

    // REFUSAL IS WHOLE: nothing landed anywhere, and no row was even opened
    // (the premise fails at compile, before the append).
    assert_eq!(c.committed_commit_counter().expect("counter"), before);
    assert_eq!(state_of(&c, &old_id.0), Some("draft".into()));
    assert_eq!(docs_titled(&c, "the successor"), 0);
}

// ---------------------------------------------------------------------------
// one statement, two pools, two grammars
// ---------------------------------------------------------------------------

/// The cross-schema transaction, exactly as designed: match a
/// `doc` in alpha **with a revision expectation**, create a `ticket` in beta,
/// create the cross-pool `catalogues` edge from the new ticket to the matched
/// doc, and set the doc's state. ONE commit, TWO pools, TWO grammars at TWO
/// different pinned versions — and the row's per-part stamps record which was
/// which.
///
/// The REVISION expectation is part of the design and it is load-bearing here:
/// a revision premise is compiled into the ALPHA part and re-validated under
/// locks at landing, so this fixture exercises a cross-pool commit whose
/// premise lives in one pool and whose creates live in the other. EXPECT
/// COUNT alone never reaches that path.
// RED: (a) `CompiledPart::schema_version` hard-coded to the FIRST pool's pin
// — the beta part then claimed alpha's version 1 and `validate_part`'s
// version arm refused; (b) landing only `payload.parts[0]` — the beta half
// vanished and `mark_cleared`'s guard refused; (c) `claims_for` skipping the
// `dst.pool` edge-slice claim — this test still passed, which is why the
// atomicity assertions below are on landed STATE and not on claims.
// RED (added with the revision expectation): dropping the revision premise
// from the alpha part, or emitting it against the BETA part where the matched
// doc does not live — the statement then committed with an unchecked premise,
// which EXPECT COUNT could not see.
#[test]
fn one_statement_lands_atomically_across_both_pools() {
    let _env = env_shared();
    let c = cauldron("cross-schema");
    let doc_id = create_doc(&c, "catalogued");
    // A freshly created node is at whole-node revision 1; the expectation
    // below re-establishes exactly that premise.
    assert_eq!(rev_of(&c, &doc_id.0), Some(1), "a fresh doc is at revision 1");
    let before = c.committed_commit_counter().expect("counter");

    let out = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { state: "draft" })
               EXPECT COUNT(d) = 1
               EXPECT REV(d) = 1
               CREATE (t: fixture-beta/ticket { state: "open" })
               CREATE (t) -[catalogues]-> (d)
               SET d.state = "published"
               RETURN t.id AS ticket, d.id AS doc"#,
            &[],
        ))
        .expect("the cross-schema statement commits");

    let (tx, returned) = match out {
        Outcome::Committed { tx, returned, .. } => (tx, returned),
        other => panic!("expected Committed, got {other:?}"),
    };

    // ONE commit for BOTH pools — the counter increments only when the
    // commit's LAST part has landed.
    assert_eq!(c.committed_commit_counter().expect("counter"), before + 1);
    // TWO parts, one per pool, in the frozen ascending-pool-name order.
    assert_eq!(
        c.touched_pools(&tx).expect("touched"),
        vec![PoolName::new(ALPHA), PoolName::new(BETA)]
    );
    // The per-part validated-under stamps are each pool's OWN pin: the spine
    // is version-blind and the row is the whole cross-version record.
    let spine = SpineDb::open(&c_root("cross-schema").join(SPINE_FILE)).expect("spine");
    let row = spine.find_by_hash(&tx).expect("read").expect("the row");
    assert_eq!(row.parts.len(), 2);
    assert_eq!(row.parts[0].pool, PoolName::new(ALPHA));
    assert_eq!(row.parts[0].schema_version, 1, "alpha's pin");
    assert_eq!(row.parts[1].pool, PoolName::new(BETA));
    assert_eq!(row.parts[1].schema_version, 2, "beta's pin");
    assert!(row.parts.iter().all(|p| p.landed), "every part landed");
    assert!(row.decided && row.cleared);

    // BOTH halves are visible, and they are visible together.
    let ticket = returned.0[0]["ticket"].as_str().expect("ticket id").to_string();
    assert_eq!(returned.0[0]["doc"], serde_json::json!(doc_id.0));
    assert_eq!(state_of(&c, &doc_id.0), Some("published".into()));
    assert_eq!(
        read_rows(
            &c,
            r#"MATCH (t: fixture-beta/ticket { state: "open" }) RETURN t.id"#,
            &[]
        )
        .len(),
        1,
        "the ticket landed in beta"
    );
    // The cross-pool edge is stored in BETA and names ALPHA explicitly.
    let hop = read_rows(
        &c,
        r#"MATCH (t: fixture-beta/ticket { id: $t }) -[catalogues]-> (d: fixture-alpha/doc) RETURN d.id"#,
        &[("t", serde_json::json!(ticket))],
    );
    assert_eq!(hop.len(), 1, "the cross-pool hop matches");
    assert_eq!(hop[0]["d.id"], serde_json::json!(doc_id.0));

    // The revision expectation is LOAD-BEARING, not decoration: the SAME
    // shape with a premise that does not hold refuses the whole cross-pool
    // statement, and the beta half never lands.
    let second = create_doc(&c, "second draft");
    let before = c.committed_commit_counter().expect("counter");
    let beta_before = read_rows(
        &c,
        r#"MATCH (t: fixture-beta/ticket { state: "open" }) RETURN t.id"#,
        &[],
    )
    .len();
    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { state: "draft" })
               EXPECT COUNT(d) = 1
               EXPECT REV(d) = 99
               CREATE (t: fixture-beta/ticket { state: "open" })
               CREATE (t) -[catalogues]-> (d)
               SET d.state = "published""#,
            &[],
        ))
        .expect_err("a revision premise that does not hold refuses the whole statement");
    match only_witness(&err) {
        Witness::FailedExpectation {
            expectation,
            expected,
            found,
        } => {
            assert_eq!(
                expectation,
                ExpectationKind::Revision {
                    binding: "d".into(),
                    field: None
                }
            );
            assert_eq!(expected, serde_json::json!(99));
            assert_eq!(found, serde_json::json!(1));
        }
        other => panic!("expected a revision FailedExpectation, got {other:?}"),
    }
    assert_eq!(c.committed_commit_counter().expect("counter"), before);
    assert_eq!(state_of(&c, &second.0), Some("draft".into()), "alpha untouched");
    assert_eq!(
        read_rows(
            &c,
            r#"MATCH (t: fixture-beta/ticket { state: "open" }) RETURN t.id"#,
            &[]
        )
        .len(),
        beta_before,
        "the beta half never landed either"
    );
}

/// A part that violates the BETA pool's own grammar refuses the WHOLE
/// statement with a witness naming the beta part — and NOTHING lands in
/// alpha (the first poisoned variant; refusal is whole).
// RED: making `compile` validate every part against the FIRST pool's
// grammar — beta's `draft` then passed under alpha's `doc` vocabulary.
#[test]
fn a_beta_grammar_violation_refuses_the_whole_statement_and_alpha_keeps_nothing() {
    let _env = env_shared();
    let c = cauldron("poison-beta");
    let doc_id = create_doc(&c, "untouched");
    let before = c.committed_commit_counter().expect("counter");

    let err = c
        .submit(commit(
            // `draft` is alpha's `doc` vocabulary; beta's `ticket` has only
            // open/closed. One merged vocabulary would let this through.
            r#"MATCH (d: fixture-alpha/doc { state: "draft" })
               CREATE (t: fixture-beta/ticket { state: "draft" })
               CREATE (t) -[catalogues]-> (d)
               SET d.state = "published""#,
            &[],
        ))
        .expect_err("the beta part violates beta's grammar");

    match only_witness(&err) {
        Witness::GrammarViolation { pool, part, .. } => {
            assert_eq!(pool, PoolName::new(BETA), "the witness names the BETA part");
            assert_eq!(part, Some(1), "beta is part one in the frozen pool order");
        }
        other => panic!("expected GrammarViolation, got {other:?}"),
    }
    assert_eq!(c.committed_commit_counter().expect("counter"), before);
    assert_eq!(
        state_of(&c, &doc_id.0),
        Some("draft".into()),
        "nothing landed in alpha"
    );
}

/// An alpha node type addressed INTO the beta pool refuses under BETA's
/// grammar — which is what proves validation is genuinely per-pool rather
/// than one merged vocabulary (the second poisoned variant).
// RED: making `resolve_grammars` fall back to any pool's grammar when the
// named one does not declare the type.
#[test]
fn an_alpha_type_addressed_into_beta_refuses_under_betas_grammar() {
    let _env = env_shared();
    let c = cauldron("poison-type");
    let err = c
        .submit(commit(
            r#"CREATE (x: fixture-beta/doc { title: "wrong pool" })"#,
            &[],
        ))
        .expect_err("beta declares no `doc`");
    match only_witness(&err) {
        Witness::GrammarViolation { pool, .. } => assert_eq!(pool, PoolName::new(BETA)),
        other => panic!("expected GrammarViolation, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// validate mode: one validator, two callers
// ---------------------------------------------------------------------------

/// The dry run is the SAME statement with the mode flipped: it runs the
/// whole validator — parse, coverage, match, premises, per-pool grammar —
/// and lands NOTHING. No pool changes, no counter movement, and no spine row
/// at all (an interior choice: nothing is at stake,
/// so nothing is recorded).
// RED: (a) deleting the `Mode::Validate` early return from `exec::execute` —
// the dry run committed; (b) returning `Outcome::Validated` BEFORE `compile`
// — the poisoned dry run below reported success.
#[test]
fn validate_mode_is_a_dry_run_that_lands_nothing() {
    let _env = env_shared();
    let c = cauldron("dry-run");
    let doc_id = create_doc(&c, "catalogued");
    let before = c.committed_commit_counter().expect("counter");

    let statement = r#"MATCH (d: fixture-alpha/doc { state: "draft" })
                       EXPECT COUNT(d) = 1
                       CREATE (t: fixture-beta/ticket { state: "open" })
                       CREATE (t) -[catalogues]-> (d)
                       SET d.state = "published"
                       RETURN t.id AS ticket"#;

    let dry = validate_only(statement, &[]);
    let tx = match c.submit(dry).expect("the dry run passes") {
        Outcome::Validated { tx } => tx,
        other => panic!("expected Validated, got {other:?}"),
    };

    // Nothing landed, nothing was recorded, nothing is in flight.
    assert_eq!(c.committed_commit_counter().expect("counter"), before);
    assert_eq!(state_of(&c, &doc_id.0), Some("draft".into()));
    assert_eq!(
        read_rows(&c, r#"MATCH (t: fixture-beta/ticket) RETURN t.id"#, &[]).len(),
        0,
        "no ticket was minted"
    );
    let spine = SpineDb::open(&c_root("dry-run").join(SPINE_FILE)).expect("spine");
    assert!(
        spine.find_by_hash(&tx).expect("read").is_none(),
        "validate mode writes no spine row"
    );

    // ONE VALIDATOR, TWO CALLERS: the same statement now commits, and the
    // dry run's disposition was therefore a true one.
    assert!(matches!(
        c.submit(commit(statement, &[])).expect("commits"),
        Outcome::Committed { .. }
    ));
    assert_eq!(c.committed_commit_counter().expect("counter"), before + 1);
}

/// And the dry run REFUSES what the commit would refuse, with the same
/// witness — a dry run that only ever says yes is not a validator.
// RED: skipping `compile` in validate mode (see above).
#[test]
fn validate_mode_refuses_what_commit_mode_would_refuse() {
    let _env = env_shared();
    let c = cauldron("dry-run-refuses");
    create_doc(&c, "untouched");
    let statement = r#"MATCH (d: fixture-alpha/doc { state: "draft" })
                       CREATE (t: fixture-beta/ticket { state: "draft" })
                       CREATE (t) -[catalogues]-> (d)"#;
    let dry = only_witness(&c.submit(validate_only(statement, &[])).expect_err("refuses"));
    let wet = only_witness(&c.submit(commit(statement, &[])).expect_err("refuses"));
    assert_eq!(dry, wet, "one validator, two callers, one verdict");
}

// ---------------------------------------------------------------------------
// the empty match, and refusal by name
// ---------------------------------------------------------------------------

/// A match that finds zero rows SUCCEEDS touching nothing, and an author who needs emptiness to be an error pins it with a
/// count expectation, which turns it into an ordinary expected-versus-found
/// refusal.
// RED: making `run_match` return an error on an empty `nodes_of` result.
#[test]
fn the_empty_match_succeeds_and_a_count_expectation_turns_it_into_a_refusal() {
    let _env = env_shared();
    let c = cauldron("empty-match");
    let before = c.committed_commit_counter().expect("counter");

    let out = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { state: "published" })
               SET d.state = "superseded"
               RETURN d.id"#,
            &[],
        ))
        .expect("an empty match is not an error");
    match out {
        Outcome::Committed { tx, returned, .. } => {
            assert_eq!(returned, Returned::default(), "nothing to return");
            assert!(
                c.touched_pools(&tx).expect("touched").is_empty(),
                "zero deltas means zero parts means no pool touched"
            );
        }
        other => panic!("expected Committed, got {other:?}"),
    }
    // Nothing landed, so the committed-commit counter did not move: it
    // counts a commit's LAST PART landing, and there was no part.
    assert_eq!(c.committed_commit_counter().expect("counter"), before);

    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { state: "published" })
               EXPECT COUNT(d) = 1
               SET d.state = "superseded""#,
            &[],
        ))
        .expect_err("emptiness pinned by a count is a refusal");
    match only_witness(&err) {
        Witness::FailedExpectation {
            expectation,
            expected,
            found,
        } => {
            assert!(matches!(expectation, ExpectationKind::Count { .. }));
            assert_eq!(expected, serde_json::json!(1));
            assert_eq!(found, serde_json::json!(0));
        }
        other => panic!("expected a count FailedExpectation, got {other:?}"),
    }
}

/// A grammatically valid but STAGED-OUT form refuses BY NAME through the
/// door — a coverage gap refuses; a dialect misreads.
// RED: emptying `COVERAGE_TABLE` — the staged-out forms were then attempted
// and misread instead of named.
#[test]
fn a_staged_out_form_refuses_by_name_through_the_door() {
    let _env = env_shared();
    let c = cauldron("staged-out");
    for (statement, form) in [
        (
            r#"MATCH (d: fixture-alpha/doc { title: "x" }) RETURN d.id"#,
            "match:field-value",
        ),
        (
            r#"MATCH (a: fixture-alpha/author) -[wrote]-> (d: fixture-alpha/doc)
               MATCH (d) -[supersedes]-> (e: fixture-alpha/doc) RETURN e.id"#,
            "match:multi-hop",
        ),
        (
            r#"MATCH (d: fixture-alpha/doc) WHERE EXISTS (d) -[supersedes]-> () RETURN d.id"#,
            "where:edge-existence",
        ),
    ] {
        let err = c
            .submit(commit(statement, &[]))
            .expect_err("a staged-out form refuses");
        match only_witness(&err) {
            Witness::UnimplementedForm { form: got } => assert_eq!(got, form),
            other => panic!("expected UnimplementedForm({form}), got {other:?}"),
        }
    }
}

/// An envelope written against another format version refuses naming BOTH
/// versions, before anything is parsed.
// RED: comparing `format_version` with `>=` instead of `==`.
#[test]
fn a_version_mismatch_refuses_naming_both_versions() {
    let _env = env_shared();
    let c = cauldron("version");
    let mut env = commit(r#"MATCH (d: fixture-alpha/doc) RETURN d.id"#, &[]);
    env.format_version = 99;
    match only_witness(&c.submit(env).expect_err("refuses")) {
        Witness::VersionMismatch {
            envelope_version,
            supported_version,
        } => {
            assert_eq!(envelope_version, 99);
            assert_eq!(supported_version, spine::envelope::ENVELOPE_FORMAT_VERSION);
        }
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
}

/// `external` is OUT OF SCOPE this build — external
/// systems never write directly, and the enum value exists only as reserved room.
/// The DOOR, not just the envelope gate, refuses it: a full `submit` carrying
/// an `Initiator::External` comes back [`SpineError::Usage`] naming the scope,
/// and — the half a unit test on `check_provenance` cannot see — the spine is
/// untouched. No row by that identity, nothing pending, no counter movement.
///
/// It is `Usage` and not a refusal witness deliberately: there is no
/// witness shape for caller misuse of the API, and the harness — not the
/// statement author — is the one who got this wrong.
// RED: (a) deleting the `Initiator::External` arm of `check_provenance` — the
// submit committed, and an out-of-scope boundary's traffic landed a row
// stamped with provenance the build has no machinery to have earned;
// (b) moving the provenance gate AFTER the append — the refusal still came
// back Usage, and the `pending()`/`find_by_hash` assertions below fired,
// which is the whole reason this is a door test and not a unit test.
#[test]
fn an_external_initiator_refuses_at_the_door_and_appends_nothing() {
    let _env = env_shared();
    let name = "external-initiator";
    let c = cauldron(name);
    let doc = create_doc(&c, "untouched by outsiders");
    let before = c.committed_commit_counter().expect("counter");

    let mut env = commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.state = "published""#,
        &[("id", serde_json::json!(doc.0))],
    );
    env.provenance.initiator = Initiator::External;
    let hash = env.hash_identity().expect("hash");

    match c.submit(env).expect_err("an external initiator is out of scope in this build") {
        SpineError::Usage { message } => {
            assert!(message.contains("out of scope"), "got: {message}");
            assert!(message.contains("external"), "got: {message}");
        }
        other => panic!("expected Usage, got {other:?}"),
    }

    // NOTHING was appended: the gate runs before the row exists, so there is
    // no aborted attempt to explain and no identity to dedup against later.
    let spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
    assert!(
        spine.find_by_hash(&hash).expect("read").is_none(),
        "the refused ask left no row by its identity"
    );
    assert!(
        spine.pending().expect("pending").is_empty(),
        "the refused ask left nothing pending"
    );
    assert!(c.in_landing(None).expect("in landing").is_empty());
    assert_eq!(c.committed_commit_counter().expect("counter"), before);
    assert_eq!(state_of(&c, &doc.0), Some("draft".into()), "nothing landed");

    // The SAME statement with a harness-supplied agent stamp commits — so the
    // refusal is about the initiator and about nothing else.
    c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.state = "published""#,
        &[("id", serde_json::json!(doc.0))],
    ))
    .expect("the same statement with an in-scope initiator commits");
    assert_eq!(state_of(&c, &doc.0), Some("published".into()));
}

// ---------------------------------------------------------------------------
// the machine-owned schema-version stamp, on explicit ask
// ---------------------------------------------------------------------------

/// The stamp becomes visible exactly when it matters — on
/// explicit ask. `RETURN b.schema_version` IS that explicit ask, and it
/// answers the same number from both sides of a commit:
///
/// - a node this statement MINTS projects its pool's PINNED version (taken
///   from the payload part it lands with, so the ask needs no follow-up read);
/// - a node this statement MATCHED projects the node's own stored stamp;
/// - a RETURN that does not name it carries NO such column, in any spelling —
///   it is in no default render.
///
/// The pin is read off the fixture grammar itself, never hardcoded, so moving
/// alpha's pin moves this test with it.
// RED: before `read_path`/`project_created` grew their `FIELD_SCHEMA_VERSION`
// arms, an explicit ask fell through to the declared-fields map and answered
// `null` — indistinguishable from "no such field", which made the
// visible-on-ask demand silently false. Deleting either arm turns the
// created or the matched half red; adding `schema_version` to
// `NodeRecord::render_default` turns the no-such-column half red.
#[test]
fn the_schema_version_stamp_is_projected_on_explicit_ask_from_both_sides_of_a_commit() {
    let _env = env_shared();
    let c = cauldron("schema-version-ask");
    // ALPHA's pin, from the fixture grammar — not a literal.
    let alpha_pin = fixtures::fixture_alpha().version;
    assert_ne!(
        alpha_pin,
        fixtures::fixture_beta().version,
        "the two fixtures are pinned apart on purpose"
    );

    // (1) THE CREATED SIDE — a node that does not exist yet answers its
    // pool's pinned version, in the same statement that mints it.
    let (_, _, returned) = expect_committed(
        c.submit(commit(
            r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" })
               RETURN d.id AS id, d.schema_version AS stamp"#,
            &[("t", serde_json::json!("stamped"))],
        ))
        .expect("the create commits"),
        false,
    );
    assert_eq!(returned.0.len(), 1);
    let id = returned.0[0]["id"].as_str().expect("id").to_string();
    assert_eq!(
        returned.0[0]["stamp"],
        serde_json::json!(alpha_pin),
        "a minted node projects its pool's pinned version"
    );

    // (2) THE MATCHED SIDE — the landed node's OWN stamp, read back through
    // the door, is the same number.
    let matched = read_rows(
        &c,
        r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.schema_version AS stamp"#,
        &[("id", serde_json::json!(id.clone()))],
    );
    assert_eq!(matched.len(), 1);
    assert_eq!(
        matched[0]["stamp"],
        serde_json::json!(alpha_pin),
        "the create's projection and the stored stamp are one number"
    );

    // The un-aliased spelling is the same ask, under its source-text column.
    let unaliased = read_rows(
        &c,
        r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.schema_version"#,
        &[("id", serde_json::json!(id.clone()))],
    );
    assert_eq!(unaliased[0]["d.schema_version"], serde_json::json!(alpha_pin));

    // (3) IN NO DEFAULT RENDER — a RETURN that does not name the stamp has no
    // such column, under any spelling of the column name.
    for statement in [
        r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.id"#,
        r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.id, d.title, d.state, REV(d)"#,
    ] {
        let rows = read_rows(&c, statement, &[("id", serde_json::json!(id.clone()))]);
        assert_eq!(rows.len(), 1);
        assert!(
            !rows[0].keys().any(|k| k.contains("schema_version")),
            "the stamp is in NO default render; `{statement}` gave {:?}",
            rows[0].keys().collect::<Vec<_>>()
        );
    }

    // (4) And the stamp is a first-class premise too: WHERE over it holds
    // against the pin and is false of anything else — so "visible on ask"
    // means visible to the matcher, not only to the projection.
    assert_eq!(
        read_rows(
            &c,
            r#"MATCH (d: fixture-alpha/doc { id: $id }) WHERE d.schema_version = $v RETURN d.id"#,
            &[
                ("id", serde_json::json!(id.clone())),
                ("v", serde_json::json!(alpha_pin)),
            ],
        )
        .len(),
        1,
        "WHERE over the stamp holds at the pin"
    );
    let err = c
        .read(&ReadRequest {
            statement:
                r#"MATCH (d: fixture-alpha/doc { id: $id }) WHERE d.schema_version = $v RETURN d.id"#
                    .to_string(),
            bindings: {
                let mut b = Bindings::new();
                b.insert("id".into(), serde_json::json!(id.clone()));
                b.insert("v".into(), serde_json::json!(alpha_pin + 1));
                b
            },
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect_err("WHERE is a PREMISE, not a filter: a false premise refuses");
    match only_witness(&err) {
        Witness::FailedCondition { binding, .. } => assert_eq!(binding, "d"),
        other => panic!("expected FailedCondition, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// exactly-once
// ---------------------------------------------------------------------------

/// The same envelope submitted twice lands ONCE, and the second submission
/// returns the ORIGINAL outcome from the row alone — no new machinery.
// RED: making `dedup_outcome` re-run the pipeline instead of replaying the
// stored projection — the doc was created twice and the counter moved twice.
#[test]
fn the_same_envelope_twice_lands_once_and_replays_the_original_outcome() {
    let _env = env_shared();
    let c = cauldron("exactly-once");
    let before = c.committed_commit_counter().expect("counter");
    let env = commit(
        r#"CREATE (d: fixture-alpha/doc { title: "once", state: "draft" }) RETURN d.id"#,
        &[],
    );

    let first = c.submit(env.clone()).expect("first");
    let second = c.submit(env).expect("second");

    let (tx1, seq1, r1) = expect_committed(first, false);
    let (tx2, seq2, r2) = expect_committed(second, true);
    assert_eq!(tx1, tx2, "one hash identity");
    assert_eq!(seq1, seq2, "one row, one sequence number");
    assert_eq!(r1, r2, "the ORIGINAL outcome, replayed");
    assert_eq!(c.committed_commit_counter().expect("counter"), before + 1);
    assert_eq!(docs_titled(&c, "once"), 1, "one landing, not two");
}

/// An envelope whose attempt ABORTED can be submitted again, and the retry
/// proceeds as a FRESH attempt with a fresh sequence number (dedup keys on
/// "did this LAND"). An abort landed nothing, so it is not an answer.
///
/// The abort is forced deterministically: a foreign transaction holds a
/// WRITE lock on the doc's body, the door appends its row and then exceeds
/// its wait ceiling, and the abort path records the attempt and releases.
// RED: (a) restoring the `UNIQUE` hash column — the retry died on a
// constraint violation; (b) making `dedup_outcome` answer `Some(Err(..))`
// for an aborted row, which is the earlier behaviour — the retry came
// back as a `Usage` refusal instead of committing.
#[test]
fn an_envelope_whose_attempt_aborted_can_be_submitted_again() {
    let _env = env_shared();
    let name = "abort-retry";
    let c = cauldron_with(
        name,
        SpineConfig {
            lock_wait_ceiling: std::time::Duration::from_millis(200),
            lock_poll_interval: std::time::Duration::from_millis(5),
            ..SpineConfig::default()
        },
    );
    let doc = create_doc(&c, "contended");
    let mut spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");

    // A foreign transaction pins the doc's body with a WRITE lock.
    let blocker_env = commit("-- the blocker", &[]);
    let blocker_hash = blocker_env.hash_identity().expect("hash");
    let blocker_seq = spine
        .append_pending(&blocker_env, &blocker_hash, &empty_payload())
        .expect("blocker row");
    let mut held = locks::ClaimSet::new();
    held.insert(locks::Claim {
        pool: PoolName::new(ALPHA),
        resource: Resource::Body { node: doc.clone() },
        mode: locks::LockMode::Write,
    });
    locks::acquire(&spine, blocker_seq, &held, &SpineConfig::default()).expect("hold");

    // The door appends its row, waits, exceeds the ceiling, and ABORTS.
    let env = commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t"#,
        &[
            ("id", serde_json::json!(doc.0)),
            ("t", serde_json::json!("retried")),
        ],
    );
    let hash = env.hash_identity().expect("hash");
    match c.submit(env.clone()) {
        Err(SpineError::Busy { .. }) => {}
        other => panic!("expected Busy at the wait ceiling, got {other:?}"),
    }
    let attempts = spine.attempts_of(&hash).expect("history");
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].attempt, 1);
    assert!(
        !attempts[0].decided && attempts[0].cleared,
        "the abandoned attempt is RECORDED ABORTED and kept"
    );
    assert_eq!(title_of(&c, &doc.0), Some("contended".into()), "nothing landed");

    // The contention clears; the SAME envelope goes in again.
    locks::release_at_clear(&spine, blocker_seq).expect("release");
    let out = c.submit(env).expect("the retry is a fresh attempt, not a refusal");
    let (_, seq2, _) = expect_committed(out, false);
    assert_eq!(title_of(&c, &doc.0), Some("retried".into()));

    let attempts = spine.attempts_of(&hash).expect("history");
    assert_eq!(attempts.len(), 2, "one identity, two attempts");
    assert_eq!(attempts[1].attempt, 2);
    assert_eq!(attempts[1].seq, seq2);
    assert!(attempts[1].seq > attempts[0].seq, "a FRESH sequence number");
    assert!(attempts[1].decided && attempts[1].cleared);
}

/// THE RESTART, as specified. The door's optimistic match binds
/// one draft doc; while it is queued for locks, another transaction lands a
/// SECOND draft doc; the re-match under the granted locks therefore compiles
/// a different payload, and the attempt RESTARTS — its row recorded aborted,
/// a FRESH row appended with a FRESH sequence number carrying the new
/// payload. Nothing is upgraded in place and no claim set grows.
///
/// The window is opened deterministically, not by sleeping: the interfering
/// thread waits until the door's claim is physically QUEUED in the lock
/// table before it does anything.
// RED: (a) making the restart reuse the row's seq instead of appending a
// fresh one — `attempts_of` returned one row and the fresh-number assertion
// failed; (b) removing the payload-equality test so the attempt proceeds on
// a stale payload — only one of the two docs was published while the row
// claimed both.
#[test]
fn a_restart_appends_a_fresh_row_and_records_the_abandoned_attempt_aborted() {
    let _env = env_shared();
    let name = "restart";
    let c = cauldron_with(
        name,
        SpineConfig {
            lock_wait_ceiling: std::time::Duration::from_secs(20),
            lock_poll_interval: std::time::Duration::from_millis(5),
            ..SpineConfig::default()
        },
    );
    let first = create_doc(&c, "first draft");
    let path = c_root(name).join(SPINE_FILE);
    let mut spine = SpineDb::open(&path).expect("spine");

    // Pin the matched doc's body so the door queues behind it.
    let blocker_env = commit("-- the blocker", &[]);
    let blocker_hash = blocker_env.hash_identity().expect("hash");
    let blocker_seq = spine
        .append_pending(&blocker_env, &blocker_hash, &empty_payload())
        .expect("blocker row");
    let mut held = locks::ClaimSet::new();
    held.insert(locks::Claim {
        pool: PoolName::new(ALPHA),
        resource: Resource::Body {
            node: first.clone(),
        },
        mode: locks::LockMode::Write,
    });
    locks::acquire(&spine, blocker_seq, &held, &SpineConfig::default()).expect("hold");

    let env = commit(
        r#"MATCH (d: fixture-alpha/doc { state: "draft" })
           SET d.state = "published"
           RETURN d.id"#,
        &[],
    );
    let hash = env.hash_identity().expect("hash");

    let out = std::thread::scope(|scope| {
        scope.spawn(|| {
            let side = SpineDb::open(&path).expect("side spine");
            // Wait for the DOOR's claim to be physically queued — no sleep
            // races: this is the state that says "the optimistic match has
            // happened and the grant has not".
            let started = std::time::Instant::now();
            loop {
                let q = locks::queue(
                    &side,
                    &PoolName::new(ALPHA),
                    &Resource::Body {
                        node: first.clone(),
                    },
                )
                .expect("queue");
                if q.iter().any(|e| e.seq != blocker_seq && !e.granted) {
                    break;
                }
                assert!(started.elapsed().as_secs() < 15, "the door never queued");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            // Move the world under it: a SECOND draft doc, landed.
            create_doc(&c, "second draft");
            // Then let the door through.
            let releaser = SpineDb::open(&path).expect("releaser");
            locks::release_at_clear(&releaser, blocker_seq).expect("release");
            // This line used to be `let _ = releaser.set_quiesced(false);`,
            // uncommented and unexplained. It was DEAD and it was a LAUNDER:
            // nothing in this fixture quiesces (`recover::quiesce` is the
            // only writer of the flag and no test here calls it), so it
            // cleared a flag that was never set — and had some path set one
            // spuriously, it would have wiped the evidence and let this test
            // pass anyway. The honest form is the assertion.
            // RED: setting the flag anywhere on the restart path — the door
            // would refuse `Busy` instead of restarting, and the old line hid
            // exactly that by clearing it first.
            assert!(
                !spine::recover::is_quiesced(&releaser).expect("quiesce flag"),
                "a contended restart must not quiesce the cauldron"
            );
        });
        c.submit(env)
    })
    .expect("the restarted statement commits");

    let (_, seq_final, returned) = expect_committed(out, false);
    // The re-match saw BOTH docs, so both were published — the statement
    // never silently narrowed itself to the row set it first saw.
    assert_eq!(returned.0.len(), 2, "both drafts were in the landing match");
    assert_eq!(state_of(&c, &first.0), Some("published".into()));
    assert_eq!(
        read_rows(
            &c,
            r#"MATCH (d: fixture-alpha/doc { state: "draft" }) RETURN d.id"#,
            &[]
        )
        .len(),
        0,
        "no draft survived"
    );

    // THE LIFECYCLE: attempt 1 aborted and kept, attempt 2 committed,
    // and its sequence number is genuinely fresher.
    let attempts = spine.attempts_of(&hash).expect("history");
    assert_eq!(attempts.len(), 2, "one restart means two attempts");
    assert_eq!(attempts[0].attempt, 1);
    assert!(
        !attempts[0].decided && attempts[0].cleared,
        "the abandoned attempt's row is recorded aborted"
    );
    assert!(
        attempts[0].parts.iter().all(|p| !p.landed),
        "an aborted attempt landed nothing"
    );
    assert_eq!(attempts[1].attempt, 2);
    assert_eq!(attempts[1].seq, seq_final);
    assert!(
        attempts[1].seq > attempts[0].seq,
        "a FRESH sequence number, not the abandoned one"
    );
    assert!(attempts[1].decided && attempts[1].cleared);
}

/// EXACTLY-ONCE UNDER CONCURRENCY, at the door and at N >= 2 real threads:
/// two processes-worth of writers submit the SAME envelope at the same
/// instant and exactly ONE landing results. The arbitration is the spine's
/// own record — `idx_tx_live` admits one in-flight attempt per identity —
/// and there is no global semaphore anywhere.
/// The losers are made to arrive DETERMINISTICALLY inside the winner's
/// in-flight window rather than by racing a barrier and hoping: a foreign
/// lock holds the winner mid-commit, and the other three submit only once
/// the winner's claim is physically queued. (A barrier version of this test
/// passed even with the in-flight dedup arm removed — the four submissions
/// simply serialized and never overlapped. That is why this shape exists.)
// RED: removing the "another attempt is in flight" arm from BOTH places
// dedup expresses it (`authoritative_seq` and `append_or_existing`) — the
// losers then drove straight into `idx_tx_live` and came back `Hard`, which
// is what a second in-flight attempt at one identity looks like.
#[test]
fn two_concurrent_submissions_of_one_envelope_produce_exactly_one_landing() {
    let _env = env_shared();
    let name = "concurrent-door";
    let c = cauldron_with(
        name,
        SpineConfig {
            lock_wait_ceiling: std::time::Duration::from_secs(20),
            lock_poll_interval: std::time::Duration::from_millis(5),
            ..SpineConfig::default()
        },
    );
    let doc = create_doc(&c, "raced");
    let path = c_root(name).join(SPINE_FILE);
    let mut spine = SpineDb::open(&path).expect("spine");

    // A foreign transaction pins the doc's body, so whichever submitter wins
    // the append is held mid-commit with its row LIVE on disk.
    let blocker_env = commit("-- the blocker", &[]);
    let blocker_hash = blocker_env.hash_identity().expect("hash");
    let blocker_seq = spine
        .append_pending(&blocker_env, &blocker_hash, &empty_payload())
        .expect("blocker row");
    let mut held = locks::ClaimSet::new();
    held.insert(locks::Claim {
        pool: PoolName::new(ALPHA),
        resource: Resource::Body { node: doc.clone() },
        mode: locks::LockMode::Write,
    });
    locks::acquire(&spine, blocker_seq, &held, &SpineConfig::default()).expect("hold");

    let env = commit(
        r#"MATCH (d: fixture-alpha/doc { id: $id }) SET d.title = $t"#,
        &[
            ("id", serde_json::json!(doc.0)),
            ("t", serde_json::json!("landed once")),
        ],
    );
    let hash = env.hash_identity().expect("hash");

    let door = &c;
    let results: Vec<Result<Outcome, SpineError>> = std::thread::scope(|scope| {
        let first = env.clone();
        let winner = scope.spawn(move || door.submit(first));
        // Wait until the winner's claim is physically queued: its row is
        // appended and live, and it is now blocked. This is the window the
        // in-flight dedup arm exists for.
        let started = std::time::Instant::now();
        loop {
            let q = locks::queue(
                &spine,
                &PoolName::new(ALPHA),
                &Resource::Body { node: doc.clone() },
            )
            .expect("queue");
            if q.iter().any(|e| e.seq != blocker_seq && !e.granted) {
                break;
            }
            assert!(started.elapsed().as_secs() < 15, "the winner never queued");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // THREE more real submitters, all inside that window.
        let losers: Vec<_> = (0..3)
            .map(|_| {
                let env = env.clone();
                scope.spawn(move || door.submit(env))
            })
            .collect();
        let mut out: Vec<Result<Outcome, SpineError>> = losers
            .into_iter()
            .map(|h| h.join().expect("join"))
            .collect();
        // Only now does the winner get through.
        let releaser = SpineDb::open(&path).expect("releaser");
        locks::release_at_clear(&releaser, blocker_seq).expect("release");
        // Same story as the restart fixture above: this was
        // `let _ = releaser.set_quiesced(false);`, with no comment and no
        // effect. `recover::quiesce` is the only writer of the flag and this
        // test never calls it, so the line cleared nothing — while standing
        // ready to launder a spurious quiesce set by five concurrent doors
        // contending on one identity, which is precisely the condition this
        // test exists to police.
        // RED: any path that quiesces under contention — the winner would
        // come back `Busy` and the "exactly one landing" count would fall to
        // zero, but only once this assertion stops clearing the flag first.
        assert!(
            !spine::recover::is_quiesced(&releaser).expect("quiesce flag"),
            "concurrent submissions of one identity must not quiesce the cauldron"
        );
        out.push(winner.join().expect("join"));
        out
    });

    let fresh = results
        .iter()
        .filter(|r| matches!(r, Ok(Outcome::Committed { deduplicated: false, .. })))
        .count();
    assert_eq!(fresh, 1, "exactly one submitter opened the attempt: {results:?}");
    for r in &results {
        match r {
            // A loser either replayed the winner's outcome or was told it was
            // in flight. Neither is a second landing, and neither is a
            // storage error.
            Ok(Outcome::Committed { .. }) | Err(SpineError::Busy { .. }) => {}
            other => panic!("a loser must dedup or wait, got {other:?}"),
        }
    }
    assert_eq!(title_of(&c, &doc.0), Some("landed once".into()), "ONE landing");
    let attempts = spine.attempts_of(&hash).expect("history");
    assert_eq!(attempts.len(), 1, "one attempt was ever opened");
    assert!(attempts[0].decided && attempts[0].cleared);
}

// ---------------------------------------------------------------------------
// recovery, quiesce, fail-closed halt
// ---------------------------------------------------------------------------

/// Roll-forward replays the row's COMPILED DELTAS and never the statement
/// text. The row below carries a payload that is perfectly good and a
/// statement text that is a VALID statement which would do DIFFERENT WORK
/// (set every draft doc to `superseded`, create nothing) — so a recovery
/// that re-matched would land the wrong thing silently, and one that
/// replays the payload lands the doc and the ticket. That is the hazard
/// the recovery design names in as many words.
///
/// Recovery PARSES the text — for the RETURN projection it
/// computes at its cleared flip — and still never MATCHES it, so the text
/// here must parse (an unparseable text on a decided row is record
/// corruption and refuses loudly; `tests/recovery.rs` proves that arm). The
/// swapped statement has no RETURN, so the recovered projection is empty,
/// and the row says it was recovered.
// RED: (a) making `recover` re-match the parsed statement and land THAT —
// no doc titled "rolled forward", no ticket; (b) making roll-forward skip
// parts whose landed flag is false — nothing landed and the assertions
// below failed.
#[test]
fn recovery_rolls_forward_from_the_payload_never_from_the_statement_text() {
    let _env = env_shared();
    let name = "roll-forward";
    let c = cauldron(name);
    // Build a decided-but-unlanded transaction the way a crash would leave
    // one: the row is on disk with its payload, the decided marker is
    // flipped, and no part has landed.
    let payload = two_pool_payload();
    let poisoned = Envelope {
        statement: r#"MATCH (d: fixture-alpha/doc { state: "draft" }) SET d.state = "superseded""#
            .into(),
        ..commit("", &[])
    };
    let hash = poisoned.hash_identity().expect("hash");
    let seq = {
        let mut spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
        let seq = spine
            .append_pending(&poisoned, &hash, &payload)
            .expect("append");
        spine.mark_decided(seq).expect("decided");
        seq
    };

    let report = c.recover().expect("recovery runs");
    assert_eq!(report.rolled_forward, vec![hash], "one row rolled forward");
    assert!(report.aborted.is_empty());

    // Both parts landed, from the payload alone.
    assert_eq!(docs_titled(&c, "rolled forward"), 1);
    assert_eq!(
        read_rows(&c, r#"MATCH (t: fixture-beta/ticket) RETURN t.id"#, &[]).len(),
        1
    );
    let spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
    let row = spine.find_by_hash(&hash).expect("read").expect("row");
    assert!(row.cleared && row.parts.iter().all(|p| p.landed));
    assert_eq!(row.seq, seq);
    // The swapped text's MATCH never ran: no doc is `superseded`, and the
    // rolled-forward doc is exactly what the payload said.
    assert_eq!(docs_titled(&c, "rolled forward"), 1);
    assert!(
        read_rows(&c, r#"MATCH (d: fixture-alpha/doc) RETURN d.state"#, &[])
            .iter()
            .all(|r| r["d.state"] != serde_json::json!("superseded")),
        "a re-matching recovery would have superseded the draft"
    );
    // Recovered, with a real (empty — no RETURN) projection, never a marker.
    assert!(row.recovered, "the row carries the audit fact");
    assert_eq!(row.result, Some(serde_json::json!([])));

    // IDEMPOTENT: run it twice, same result. The second run has nothing to
    // do — the pool-side `applied_parts` marker and the cleared flip both
    // say so — and it re-applies nothing.
    let again = c.recover().expect("recovery runs twice");
    assert_eq!(again, spine::recover::RecoveryReport::default());
    assert_eq!(docs_titled(&c, "rolled forward"), 1, "not applied twice");
}

/// RECOVERY IS PER ROW, not per identity — the obligation the attempt-aware
/// record creates. An identity that RESTARTED leaves several rows:
/// abandoned attempts recorded aborted, and at most one decided attempt.
/// Recovery must roll the decided one forward, leave every aborted one
/// untouched with its landed flags still false, and land the payload EXACTLY
/// ONCE even though both rows carry the same hash — which is the case the
/// pool-side `applied_parts` marker, keyed on that hash, could have got
/// wrong.
// RED once by a TWO-PART mutation, and the single-part version is recorded
// as a NEGATIVE RESULT because it is instructive: making `recover` roll
// un-decided rows forward flips NOTHING here, since an aborted attempt is
// CLEARED and so is not in `pending()`'s worklist at all. To make the
// abandoned payload land you must ALSO widen the worklist
// (`WHERE cleared = 0` -> `WHERE cleared = 0 OR decided = 0`). With both,
// attempt 1's stale payload lands a second doc and `docs_titled("abandoned
// draft")` comes back 1. Verified.
#[test]
fn recovery_handles_several_attempts_at_one_identity() {
    let _env = env_shared();
    let name = "multi-attempt-recovery";
    let c = cauldron(name);
    // A parseable statement text (recovery parses the text of a
    // decided row for its RETURN projection — `-- a restarted statement`
    // was provenance-only prose and would now refuse as an altered record).
    // No RETURN, so the projection is empty; the payloads are hand-built.
    let env = commit(
        r#"CREATE (d: fixture-alpha/doc { title: "restarted", state: "draft" })"#,
        &[],
    );
    let hash = env.hash_identity().expect("hash");

    // Attempt 1: appended with a payload it would have landed, then ABORTED
    // (decided = 0, cleared = 1) exactly as the door's restart records it.
    // Attempt 2: appended with a DIFFERENT payload and left DECIDED and
    // un-landed — the crash the roll-forward exists for. The payloads differ
    // ON PURPOSE: it is the only way to tell "recovery replayed THIS ROW" from
    // "recovery replayed this identity", and the second is a real hazard here
    // because the pool-side idempotence marker is keyed on the HASH.
    let (aborted, decided) = {
        let mut spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
        let one = spine
            .append_pending(&env, &hash, &abandoned_payload())
            .expect("attempt 1");
        let row = spine.row_at(one).expect("row");
        spine::recover::abort_row(&mut spine, &row).expect("abort attempt 1");
        let two = spine
            .append_pending(&env, &hash, &two_pool_payload())
            .expect("attempt 2 — an abort never blocks one");
        spine.mark_decided(two).expect("decided");
        (one, two)
    };
    assert!(decided > aborted, "the second attempt's number is fresher");

    // Recovery's worklist holds ONE row for this identity — the un-cleared
    // one — because `idx_tx_live` cannot admit two.
    let report = c.recover().expect("recovery runs");
    assert_eq!(
        report.rolled_forward,
        vec![hash],
        "the DECIDED attempt rolled forward"
    );
    assert!(
        report.aborted.is_empty(),
        "the abandoned attempt was already cleared and is not re-walked"
    );

    // THE DECIDED ROW'S payload landed, exactly once — and the ABANDONED
    // attempt's payload landed nowhere at all, even though it carries the very
    // same hash identity.
    assert_eq!(docs_titled(&c, "rolled forward"), 1);
    assert_eq!(
        docs_titled(&c, "abandoned draft"),
        0,
        "an aborted attempt's payload is never replayed — roll-forward is per ROW"
    );
    assert_eq!(
        read_rows(&c, r#"MATCH (t: fixture-beta/ticket) RETURN t.id"#, &[]).len(),
        1
    );

    let spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
    let attempts = spine.attempts_of(&hash).expect("history");
    assert_eq!(attempts.len(), 2, "both rows kept forever");
    assert_eq!(attempts[0].seq, aborted);
    assert!(
        !attempts[0].decided && attempts[0].cleared,
        "attempt 1 is still the record of an abort"
    );
    assert!(
        attempts[0].parts.iter().all(|p| !p.landed),
        "and its landed flags are still false — recovery never touched it"
    );
    assert_eq!(attempts[1].seq, decided);
    assert!(attempts[1].decided && attempts[1].cleared);
    assert!(attempts[1].parts.iter().all(|p| p.landed));
    // ONE landing for the identity, and the counter says so.
    assert_eq!(c.committed_commit_counter().expect("counter"), 1);

    // Idempotent over a multi-attempt identity too.
    assert_eq!(
        c.recover().expect("twice"),
        spine::recover::RecoveryReport::default()
    );
    assert_eq!(docs_titled(&c, "rolled forward"), 1, "not applied twice");
}

/// A row found UN-DECIDED aborts: nothing landed (the per-pool atomic step
/// guarantees it), and the row REMAINS as the record of the aborted attempt.
// RED: making `abort_row` delete the row — `find_by_hash` then answered
// None and the abort stopped being a recorded fact.
#[test]
fn an_undecided_row_aborts_lands_nothing_and_is_kept_forever() {
    let _env = env_shared();
    let name = "abort";
    let c = cauldron(name);
    let payload = two_pool_payload();
    let env = commit("", &[]);
    let hash = env.hash_identity().expect("hash");
    {
        let mut spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
        spine.append_pending(&env, &hash, &payload).expect("append");
    }

    let report = c.recover().expect("recovery runs");
    assert_eq!(report.aborted, vec![hash]);
    assert!(report.rolled_forward.is_empty());
    assert_eq!(docs_titled(&c, "rolled forward"), 0, "nothing landed");

    let spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
    let row = spine.find_by_hash(&hash).expect("read").expect("kept");
    assert!(!row.decided, "an aborted row never decided");
    assert!(
        row.parts.iter().all(|p| !p.landed),
        "the landed flags stay false — the abort is itself a recorded fact"
    );
    // And recovery is idempotent over aborts too.
    assert_eq!(
        c.recover().expect("twice"),
        spine::recover::RecoveryReport::default()
    );
}

/// FAIL-CLOSED: a halted spine refuses every write, naming the halt, and the
/// cauldron is effectively READ-ONLY — reads still answer.
// RED: dropping the halt gate from `exec::execute`'s phase 1 — writes
// continued on a halted cauldron.
#[test]
fn a_halted_spine_refuses_every_write_by_name_and_stays_readable() {
    let _env = env_shared();
    let name = "halted";
    let c = cauldron(name);
    create_doc(&c, "before the halt");
    {
        let mut spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
        spine.set_halted("simulated disk refusal").expect("halt");
    }
    assert_eq!(
        c.halted().expect("halted"),
        Some("simulated disk refusal".to_string())
    );
    match c.submit(commit(
        r#"CREATE (d: fixture-alpha/doc { title: "after", state: "draft" })"#,
        &[],
    )) {
        Err(SpineError::Halted { reason }) => assert!(reason.contains("simulated disk refusal")),
        other => panic!("expected Halted, got {other:?}"),
    }
    // The DOOR'S OWN gate refuses first, before anything is parsed: a
    // statement that could never parse still comes back `Halted` and never
    // as a syntax witness. Without this the test could not tell the door's
    // phase-1 gate from `SpineDb::append_or_existing`'s own halt check,
    // which sits four phases later — and the red sweep proved it could not
    // (mutation M14 removed the gate and nothing flipped).
    match c.submit(commit("!!! this is not ladle !!!", &[])) {
        Err(SpineError::Halted { .. }) => {}
        other => panic!("the halt gate must precede the parse, got {other:?}"),
    }
    // Effectively READ-ONLY, not dead: the read door still answers.
    assert_eq!(docs_titled(&c, "before the halt"), 1);
    assert_eq!(docs_titled(&c, "after"), 0);
}

/// THE HOLD: a writer's work over several commits holds the cauldron in the
/// durable halted state under its own reason. Every other handle (here, and
/// any other process or older binary, which read the same halt) is refused
/// naming it; the holder's own commits pass; the commit that ends the work
/// lifts it in the same transaction as its decided flip; a crash leaves it
/// for the next holder to adopt by its exact reason; and a spine halted for
/// another reason is never held over.
// RED: lifting the hold anywhere but inside the decided flip's transaction,
// or letting a handle that does not own the reason pass it.
#[test]
fn a_hold_refuses_everyone_but_its_holder_and_ends_with_its_last_commit() {
    let _env = env_shared();
    let name = "held";
    let mut holder = cauldron(name);
    let other = Cauldron::open(&c_root(name), SpineConfig::default()).expect("a second handle");
    create_doc(&holder, "before the hold");
    let reason = "merge MG-1 into this cauldron is pending (a test hold)";
    holder.hold(reason).expect("hold");
    assert_eq!(holder.held(), Some(reason));
    assert_eq!(other.halted().expect("halted"), Some(reason.to_string()), "everyone else reads it as the halt");
    assert_eq!(holder.halted().expect("halted"), None, "its holder writes through it");
    match other.submit(commit(r#"CREATE (d: fixture-alpha/doc { title: "theirs", state: "draft" })"#, &[])) {
        Err(SpineError::Halted { reason: r }) => assert_eq!(r, reason),
        other => panic!("expected Halted, got {other:?}"),
    }
    create_doc(&holder, "the holder's first");
    assert!(holder.halted().expect("halted").is_none() && other.halted().expect("halted").is_some(), "an ordinary commit keeps it");
    // Another hold is not set over it.
    let mut third = Cauldron::open(&c_root(name), SpineConfig::default()).expect("a third handle");
    assert!(matches!(third.hold("someone else's"), Err(SpineError::Usage { .. })));
    // A crash leaves it: a new handle adopts it by its exact reason only.
    assert!(!third.adopt_hold("not the reason").expect("adopt"));
    assert!(third.adopt_hold(reason).expect("adopt"));
    drop(holder);
    third.lift_hold_at_next_commit();
    create_doc(&third, "the holder's last");
    assert_eq!(other.halted().expect("halted"), None, "the last commit lifted it");
    create_doc(&other, "after the hold");
    // Released outright, and only its own.
    let mut h = Cauldron::open(&c_root(name), SpineConfig::default()).expect("handle");
    h.hold("a second hold").expect("hold");
    assert!(h.release_hold().expect("release"));
    assert_eq!(other.halted().expect("halted"), None);
    for t in ["before the hold", "the holder's first", "the holder's last", "after the hold"] {
        assert_eq!(docs_titled(&other, t), 1, "{t}");
    }
    assert_eq!(docs_titled(&other, "theirs"), 0);
}

/// QUIESCE: no new commit begins; in-flight commits finish; `resume` lifts
/// it (the backup hook, so a whole-cauldron backup never contains a
/// half-landed commit).
// RED: making `is_quiesced` always answer false — new commits began during
// a quiesce and the backup window stopped being a window.
#[test]
fn quiesce_stops_new_commits_and_resume_lifts_it() {
    let _env = env_shared();
    let c = cauldron("quiesce");
    let token = c.quiesce().expect("an idle cauldron drains immediately");
    match c.submit(commit(
        r#"CREATE (d: fixture-alpha/doc { title: "during", state: "draft" })"#,
        &[],
    )) {
        Err(SpineError::Busy { message }) => assert!(message.contains("quiesced")),
        other => panic!("expected Busy, got {other:?}"),
    }
    assert_eq!(docs_titled(&c, "during"), 0);
    c.resume(&token).expect("resume");
    assert!(matches!(
        c.submit(commit(
            r#"CREATE (d: fixture-alpha/doc { title: "during", state: "draft" })"#,
            &[],
        ))
        .expect("commits after resume"),
        Outcome::Committed { .. }
    ));
    assert_eq!(docs_titled(&c, "during"), 1);
}

// ---------------------------------------------------------------------------
// the seam, at the door
// ---------------------------------------------------------------------------

/// The production build PROVES the seam absent: a set `SPINE_FAULT` makes
/// the door refuse loudly by name, before anything is parsed or appended,
/// and nothing lands. In the armed build the same variable arms the stage
/// instead — so this test asserts the property its own build owes.
// RED: replacing the non-feature `gate_with` body with `Ok(())` — the
// production door then ignored a set SPINE_FAULT silently, which is the
// exact failure this accommodation exists to make impossible.
#[test]
fn the_door_proves_the_fault_seam_absent_or_refusing() {
    let _env = env_exclusive();

    #[cfg(not(feature = "fault-injection"))]
    {
        let c = cauldron("fault-gate");
        let before = c.committed_commit_counter().expect("counter");
        std::env::set_var("SPINE_FAULT", "before-cleared");
        let env = commit(
            r#"CREATE (d: fixture-alpha/doc { title: "armed", state: "draft" })"#,
            &[],
        );
        let hash = env.hash_identity().expect("hash");
        match c.submit(env) {
            Err(SpineError::FaultSeamRefused { variable, value }) => {
                assert_eq!(variable, spine::fault::FAULT_ENV, "it names which seam variable");
                assert_eq!(value, "before-cleared");
            }
            other => panic!("a production door must refuse a set SPINE_FAULT, got {other:?}"),
        }
        assert_eq!(c.committed_commit_counter().expect("counter"), before);
        assert_eq!(docs_titled(&c, "armed"), 0, "the refusal landed nothing");
        // And it refused AT THE DOOR, before anything was appended: no spine
        // row exists for this ask at all. Without this the test could not
        // tell the door's phase-2 gate from `fault::at_stage`'s refusal four
        // phases later, which would have left an aborted row behind — and
        // the red sweep proved it could not (mutation M16).
        let spine = SpineDb::open(&c_root("fault-gate").join(SPINE_FILE)).expect("spine");
        assert!(
            spine.find_by_hash(&hash).expect("read").is_none(),
            "the production door refuses BEFORE the row append"
        );
    }
    #[cfg(feature = "fault-injection")]
    {
        // The armed build cannot exercise the door in-process: a matching
        // stage ABORTS the process, which is the whole point. The aimed kill
        // is proven out of process by `the_kill_between_two_pools_landings_*`
        // below; here the armed build asserts the gate's own half — a named
        // stage arms the seam, a misspelled one is loud rather than a silent
        // no-op that would make every kill test a fake green.
        std::env::set_var("SPINE_FAULT", "row-appended");
        assert!(spine::fault::gate().is_ok(), "a named stage arms the seam");
        std::env::set_var("SPINE_FAULT", "typo");
        assert!(
            spine::fault::gate().is_err(),
            "a misspelled stage is loud, never a silent no-op"
        );
    }

    std::env::remove_var("SPINE_FAULT");
}

/// THE AIMED KILL, and with it the CRASH-WINDOW OBLIGATION discharged:
/// demonstrate that a crash between two files' landings is possible in the
/// configuration actually used. A child process runs the two-pool
/// cross-schema statement under `SPINE_FAULT=between-landings` and is killed
/// by the seam AFTER alpha's part has landed and BEFORE beta's. The parent
/// then observes the half-landed state the storage engine genuinely allows —
/// two files, two separate transactions, no ATTACH anywhere — and runs
/// recovery, which rolls the un-landed part FORWARD from the row.
///
/// This is the one window that distinguishes "the commit point is the ROW"
/// from "the commit point is the storage engine": an outside process-kill
/// cannot be aimed here, and a suite without this kill cannot tell the two
/// designs apart.
// RED: (a) `#[cfg(not(feature))]`-ing the `BetweenLandings` seam call out of
// `exec` — the child then exited 0 and the half-landed assertions failed;
// (b) making recovery abort a DECIDED row instead of rolling it forward —
// the ticket never appeared and the row stayed un-cleared.
#[cfg(all(unix, feature = "fault-injection"))]
#[test]
fn the_kill_between_two_pools_landings_leaves_the_row_to_roll_forward() {
    use std::os::unix::process::ExitStatusExt;
    let _env = env_exclusive();
    let dir = c_root("kill-between");
    let _ = std::fs::remove_dir_all(&dir);
    // The child builds the cauldron itself, so the parent holds no handle
    // into a database a dying process is writing.
    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["the_kill_child_body", "--exact", "--nocapture"])
        .env("SPINE_FAULT", "between-landings")
        .env("SPINE_KILL_CHILD", dir.to_string_lossy().to_string())
        .status()
        .expect("spawn the child");
    assert_eq!(
        status.signal(),
        Some(6),
        "the seam aborts the process AT the stage (SIGABRT), it does not return"
    );

    // THE HALF-LANDED WINDOW, on disk: alpha has its part, beta does not,
    // and the ROW is what says so.
    let c = Cauldron::open_unrecovered(&dir, SpineConfig::default()).expect("reopen after the kill");
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let stuck = spine
        .pending()
        .expect("pending")
        .into_iter()
        .find(|r| r.decided)
        .expect("a decided, un-cleared row survives the kill");
    assert_eq!(stuck.parts.len(), 2);
    assert!(stuck.parts[0].landed, "alpha's part landed before the kill");
    assert!(!stuck.parts[1].landed, "beta's part did not");
    assert!(!stuck.cleared);
    assert_eq!(
        read_rows(&c, r#"MATCH (t: fixture-beta/ticket) RETURN t.id"#, &[]).len(),
        0,
        "beta holds nothing yet — the pools genuinely disagree in this window"
    );

    // ROLL FORWARD, from the row's own payload.
    let report = c.recover().expect("recovery");
    assert_eq!(report.rolled_forward, vec![stuck.hash]);
    assert_eq!(
        read_rows(&c, r#"MATCH (t: fixture-beta/ticket) RETURN t.id"#, &[]).len(),
        1,
        "everything landed, or nothing — and the row told which"
    );
    let row = spine
        .find_by_hash(&stuck.hash)
        .expect("read")
        .expect("row kept");
    assert!(row.cleared && row.parts.iter().all(|p| p.landed));
    // Idempotent: a second recovery pass changes nothing.
    assert_eq!(
        c.recover().expect("twice"),
        spine::recover::RecoveryReport::default()
    );
}

/// The child half of the kill test. Inert unless `SPINE_KILL_CHILD` names a
/// directory, so it is an ordinary passing no-op in every normal run.
#[test]
fn the_kill_child_body() {
    let Ok(dir) = std::env::var("SPINE_KILL_CHILD") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let c = Cauldron::create(
        &dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        SpineConfig::default(),
    )
    .expect("create");
    // A single-part commit first: it never reaches `between-landings`
    // (that stage fires only before the SECOND part), so the setup survives
    // the armed seam — which is itself a small proof that the seam is AIMED
    // and not a blunt kill switch.
    create_doc(&c, "catalogued");
    let _ = c.submit(commit(
        r#"MATCH (d: fixture-alpha/doc { state: "draft" })
           CREATE (t: fixture-beta/ticket { state: "open" })
           CREATE (t) -[catalogues]-> (d)
           SET d.state = "published""#,
        &[],
    ));
    unreachable!("the seam must have aborted this process at `between-landings`");
}

// ---------------------------------------------------------------------------
// bulk read
// ---------------------------------------------------------------------------

/// Bulk read is the statement's read form: MATCH plus WHERE plus RETURN,
/// lock-free, holding nothing. A WRITE statement handed to the read door is
/// refused — one door per direction.
// RED: (a) dropping the `is_read_only` gate — a mutating statement ran on
// the read path with no provenance and no row; (b) making `run_match` filter
// on WHERE — the conditional read below returned one row instead of
// refusing, which is exactly the WHERE-is-not-a-filter seam.
#[test]
fn bulk_read_returns_projections_and_refuses_a_write_statement() {
    let _env = env_shared();
    let c = cauldron("bulk-read");
    create_doc(&c, "alpha one");
    create_doc(&c, "alpha two");

    let rows = read_rows(
        &c,
        r#"MATCH (d: fixture-alpha/doc { state: "draft" }) RETURN d.title, REV(d)"#,
        &[],
    );
    assert_eq!(rows.len(), 2);
    let titles: Vec<&str> = rows.iter().map(|r| r["d.title"].as_str().unwrap()).collect();
    assert!(titles.contains(&"alpha one") && titles.contains(&"alpha two"));
    assert_eq!(rows[0]["REV(d)"], serde_json::json!(1));

    // WHERE is a PREMISE on the read path too, never a filter: a condition
    // false of ANY bound row refuses the whole read.
    let err = c
        .read(&ReadRequest {
            statement: r#"MATCH (d: fixture-alpha/doc) WHERE d.title = "alpha one" RETURN d.id"#
                .into(),
            bindings: Bindings::new(),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect_err("a premise false of one bound row refuses");
    match only_witness(&err) {
        Witness::FailedCondition { binding, .. } => assert_eq!(binding, "d"),
        other => panic!("expected FailedCondition, got {other:?}"),
    }

    // The read door is a READ door.
    assert!(matches!(
        c.read(&ReadRequest {
            statement: r#"MATCH (d: fixture-alpha/doc) SET d.state = "published""#.into(),
            bindings: Bindings::new(),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        }),
        Err(SpineError::Usage { .. })
    ));
}

/// WHERE IS NOT A FILTER on the write path either, and this is the seam
/// most likely to be the place for the two halves of this build
/// to disagree. A bulk SET whose WHERE is false of one bound row refuses the
/// WHOLE statement and narrows nothing.
// RED: adding a WHERE filter to `run_match` — the statement below then
// committed, quietly setting one of the two docs and leaving the other, and
// `Witness::FailedCondition` became unreachable dead code.
#[test]
fn where_is_a_premise_not_a_filter_and_a_bulk_set_never_narrows_itself() {
    let _env = env_shared();
    let c = cauldron("where-premise");
    let keep = create_doc(&c, "in the band");
    let other = create_doc(&c, "out of the band");
    let before = c.committed_commit_counter().expect("counter");

    let err = c
        .submit(commit(
            r#"MATCH (d: fixture-alpha/doc { state: "draft" })
               WHERE d.title = "in the band"
               SET d.state = "published""#,
            &[],
        ))
        .expect_err("the premise is false of one bound row");
    match only_witness(&err) {
        Witness::FailedCondition { condition, binding } => {
            assert_eq!(binding, "d");
            assert_eq!(condition, r#"d.title = "in the band""#);
        }
        other => panic!("expected FailedCondition, got {other:?}"),
    }
    // Refusal is WHOLE: neither row moved, not even the one the condition
    // was true of.
    assert_eq!(c.committed_commit_counter().expect("counter"), before);
    assert_eq!(state_of(&c, &keep.0), Some("draft".into()));
    assert_eq!(state_of(&c, &other.0), Some("draft".into()));
}

// ---------------------------------------------------------------------------
// the validated read, its bounded escalation, and the
//              single-pool exemption
// ---------------------------------------------------------------------------

/// THE COLD PATH, RUN WARM (the escalation path is expected
/// cold, and a cold path that has never run warm is not proven).
///
/// A transaction left DECIDED and un-landed across both pools makes every
/// cross-pool validated read inconsistent by the landing-state check — the
/// half of the consistency check that a bare counter check would slip. The read
/// therefore retries to its bound and then ESCALATES: it appends its own
/// spine row for a sequence number, takes real read locks at that number,
/// and completes by the same ordering that prevents deadlock.
///
/// The storm is called off DETERMINISTICALLY, from inside the read's own
/// query closure on its fourth invocation — no sleeps, no races: this test
/// runs the validated-read driver directly for exactly that control.
// RED: (a) deleting the `in_landing` half of `window_is_consistent` — the
// read returned a mixed state on its first attempt and never escalated;
// (b) making `escalated` return the value without acquiring — the
// `RealReadLocks` assertion failed; (c) making the lock-free loop retry
// forever instead of breaking at `retry_bound` — the test hung to nothing.
#[test]
fn a_commit_storm_escalates_the_validated_read_to_real_read_locks() {
    let _env = env_shared();
    let name = "escalation";
    let config = SpineConfig {
        retry_bound: 2,
        lock_poll_interval: std::time::Duration::from_millis(1),
        ..SpineConfig::default()
    };
    let c = cauldron_with(name, config.clone());
    create_doc(&c, "read me");

    // A decided, un-landed transaction across BOTH pools: the read's own
    // pools are in landing state, so no cut is consistent while it stands.
    let payload = two_pool_payload();
    let env = commit("", &[]);
    let hash = env.hash_identity().expect("hash");
    let stuck = {
        let mut spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
        let seq = spine.append_pending(&env, &hash, &payload).expect("append");
        spine.mark_decided(seq).expect("decided");
        seq
    };
    assert_eq!(c.in_landing(None).expect("in landing").len(), 1);

    let pools = vec![PoolName::new(ALPHA), PoolName::new(BETA)];
    let mut claims = locks::ClaimSet::new();
    for (pool, ty) in [(ALPHA, "doc"), (BETA, "ticket")] {
        claims.insert(locks::Claim {
            pool: PoolName::new(pool),
            resource: Resource::TypeSlice {
                node_type: ty.into(),
            },
            mode: locks::LockMode::Read,
        });
    }

    let mut calls = 0usize;
    let result = spine::read::run_validated(&c, &pools, &config, &claims, |snaps| {
        calls += 1;
        if calls == 4 {
            // The storm abates — inside the escalated phase, so the very
            // next attempt is the one that certifies a consistent cut.
            let mut spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
            for part in 0..2u32 {
                spine.mark_part_landed(stuck, part).expect("land");
            }
            spine
                .mark_cleared(stuck, &serde_json::json!({"storm": "over"}))
                .expect("clear");
        }
        Ok(snaps.of(&PoolName::new(ALPHA))?.nodes_of("doc", None)?.len())
    })
    .expect("the escalated read completes");

    assert_eq!(calls, 5, "three lock-free attempts, then two escalated ones");
    assert_eq!(result.value, 1);
    assert_eq!(result.retries, 3);
    let reader_one = match result.escalation {
        spine::read::Escalation::RealReadLocks { seq } => {
            assert!(seq > stuck, "the reader took a number of its own, later than the storm's");
            seq
        }
        other => panic!("expected RealReadLocks, got {other:?}"),
    };
    // The reader left NOTHING behind: its locks are released, and its row is
    // cleared while never decided — so it never entered `in_landing` and
    // never moved the committed-commit counter (an escalated read must not
    // invalidate anybody else's read).
    assert!(c.in_landing(None).expect("in landing").is_empty());
    let spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
    assert!(spine.pending().expect("pending").is_empty());
    assert!(locks::queue(
        &spine,
        &PoolName::new(ALPHA),
        &Resource::TypeSlice {
            node_type: "doc".into()
        }
    )
    .expect("queue")
    .is_empty());

    // THE ESCALATING READER'S ROW COEXISTS WITH THE DEDUP PREDICATE. Its row is cleared-and-never-decided — the ABORT
    // shape — so it occupies neither `idx_tx_live` nor `idx_tx_committed`,
    // can never be mistaken for a landed commit, and never moves the
    // committed-commit counter. A reader that could be deduped against, or
    // that bumped the counter, would invalidate everybody else's reads.
    let counter_before = c.committed_commit_counter().expect("counter");
    // A SECOND escalated read in the same cauldron: its unique nonce gives it
    // its own identity, so it opens its own row rather than colliding with
    // the first one's.
    let payload = two_pool_payload();
    let env2 = commit("-- second storm", &[]);
    let hash2 = env2.hash_identity().expect("hash");
    let stuck2 = {
        let mut spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
        let seq = spine.append_pending(&env2, &hash2, &payload).expect("append");
        spine.mark_decided(seq).expect("decided");
        seq
    };
    let mut calls2 = 0usize;
    let second = spine::read::run_validated(&c, &pools, &config, &claims, |snaps| {
        calls2 += 1;
        if calls2 == 4 {
            let mut spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
            for part in 0..2u32 {
                spine.mark_part_landed(stuck2, part).expect("land");
            }
            spine
                .mark_cleared(stuck2, &serde_json::json!({"storm": "over"}))
                .expect("clear");
        }
        Ok(snaps.of(&PoolName::new(ALPHA))?.nodes_of("doc", None)?.len())
    })
    .expect("a second escalated read completes on its own identity");
    let reader_two = match second.escalation {
        spine::read::Escalation::RealReadLocks { seq } => {
            assert!(seq > stuck2, "its own, later number");
            assert_ne!(
                seq, reader_one,
                "each escalation opens its OWN identity — the nonce is unique, so the second \
                 reader is never deduped onto the first"
            );
            seq
        }
        other => panic!("expected RealReadLocks, got {other:?}"),
    };
    // Exactly one commit landed between the two reads (the storm's), and
    // neither reader contributed to it.
    assert_eq!(
        c.committed_commit_counter().expect("counter"),
        counter_before + 1
    );
    let spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
    assert!(spine.pending().expect("pending").is_empty());

    // THE ASSERTION THE COEXISTENCE CLAIM ACTUALLY NEEDS. Two distinct
    // sequence numbers alone would NOT catch a broken nonce — under the new
    // dedup predicate the first reader's row is cleared-and-undecided, so a
    // colliding identity would simply open ATTEMPT 2 and still get a fresher
    // number. What proves the nonce is doing its job is that each reader's
    // row is its own identity's FIRST attempt.
    let row_one = spine.row_at(reader_one).expect("reader one's row");
    let row_two = spine.row_at(reader_two).expect("reader two's row");
    assert_ne!(
        row_one.hash, row_two.hash,
        "two escalations are two different asks and must never share an identity"
    );
    assert_eq!(row_one.attempt, 1, "each escalation is its own attempt one");
    assert_eq!(row_two.attempt, 1, "and so is the second — not attempt 2");
    for row in [&row_one, &row_two] {
        assert!(
            !row.decided && row.cleared,
            "an escalated reader's row is the ABORT shape: in neither dedup index, so it can \
             never be replayed as a landing and never blocks a writer's identity"
        );
        assert!(row.parts.is_empty(), "a reader has no parts and lands nothing");
    }
}

/// A SINGLE-POOL read needs neither locks nor validation — one pool snapshot
/// is consistent by the per-pool atomic landing step. It
/// answers immediately even while a cross-pool transaction is mid-landing
/// on that very pool, which is exactly the exemption's claim.
// RED: deleting the `pools.len() <= 1` short-circuit from `run_validated` —
// the single-pool read then inherited the cross-pool validation, retried to
// its bound against the stuck transaction and escalated.
#[test]
fn a_single_pool_read_needs_neither_locks_nor_validation() {
    let _env = env_shared();
    let name = "single-pool";
    let c = cauldron_with(
        name,
        SpineConfig {
            retry_bound: 1,
            lock_poll_interval: std::time::Duration::from_millis(1),
            ..SpineConfig::default()
        },
    );
    create_doc(&c, "one pool only");
    {
        let mut spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
        let env = commit("", &[]);
        let hash = env.hash_identity().expect("hash");
        let seq = spine
            .append_pending(&env, &hash, &two_pool_payload())
            .expect("append");
        spine.mark_decided(seq).expect("decided");
    }
    let out = c
        .read(&ReadRequest {
            statement: r#"MATCH (d: fixture-alpha/doc) RETURN d.title"#.into(),
            bindings: Bindings::new(),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("a single-pool read is never invalidated");
    assert_eq!(out.rows.0.len(), 1);
    assert_eq!(out.retries, 0);
    assert!(!out.escalated, "it never entered a queue");
}

/// The one invariant between two configuration numbers is CHECKED, not
/// assumed: a threshold above the bound would silently mean "never
/// escalate", and a cold path that can never run is worse than one that
/// runs rarely.
// RED: deleting `SpineConfig::check`'s first arm — the impossible
// configuration was then accepted silently.
#[test]
fn the_config_invariant_is_checked_loudly_at_create() {
    let _env = env_shared();
    let dir = c_root("bad-config");
    let _ = std::fs::remove_dir_all(&dir);
    let bad = SpineConfig {
        restart_bound: 2,
        escalation_threshold: 9,
        ..SpineConfig::default()
    };
    assert!(matches!(
        Cauldron::create(&dir, &[fixtures::fixture_alpha()], bad),
        Err(SpineError::Usage { .. })
    ));
    assert!(!dir.exists(), "a refused create leaves no cauldron behind");
}

// ---------------------------------------------------------------------------
// the wedge fix, and replaying a recovered commit
// ---------------------------------------------------------------------------

/// THE AT-MOST-ONE PROBE STATEMENT, and the whole of what "refusal is whole"
/// means for it.
///
/// `current_of` is the fixture's at-most-one slice: at most one may arrive at any one
/// author. `d0` already holds it. The statement below asks for a second one
/// and names no removal — an ordinary mistyped statement, no concurrency, no
/// fault injection. It used to compile clean (grammar cannot see occupancy),
/// append, acquire, FLIP DECIDED, and only then be refused by the landing
/// transaction — leaving the row decided forever, its locks held forever, and
/// every cross-pool validated read in the cauldron invalidated forever, with
/// recovery rolling it forward into the same refusal on every pass.
///
/// Now the occupancy check runs at `exec`'s PHASE 10b, under the granted
/// slice WRITE locks and BEFORE the decided flip. The witness is unchanged;
/// what changed is that the refusal is WHOLE. This test asserts all five
/// halves of whole: the witness, no row left decided, nothing landed, the
/// locks released, and the reads unaffected.
// RED, twice, and the two reds are different on purpose:
// (a) deleting the `check_occupancy_under_locks(cauldron, payload)?` call at
//     exec's phase 10b — the statement decided, the landing refused, and the
//     door returned exec's INTERNAL INVARIANT `Hard` instead of a refusal, so
//     `only_witness` reds on the first assertion. That is the invariant arm
//     of the fix proving it is live, not decoration.
// (b) deleting the phase-10b call AND unwrapping
//     `occupancy_after_decided_is_an_invariant_violation` back to a bare `?`,
//     which is the crate EXACTLY as the wedge was found: the witness
//     assertions then pass and the test reds on "no un-cleared row survives
//     the refusal" — the wedge itself.
#[test]
fn the_at_most_one_probe_refuses_before_the_decided_flip_and_wedges_nothing() {
    let _env = env_shared();
    let name = "at-most-one-pre-decide";
    // A SHORT lock ceiling, deliberately: it is what makes "the locks were
    // released" a real assertion rather than a hope. A held lock fails this
    // test in half a second instead of hanging it.
    let c = cauldron_with(
        name,
        SpineConfig {
            lock_wait_ceiling: std::time::Duration::from_millis(500),
            ..SpineConfig::default()
        },
    );

    let author = create_author(&c, "ada");
    let d0 = create_doc(&c, "the current one");
    let d1 = create_doc(&c, "the pretender");
    c.submit(commit(
        SET_CURRENT,
        &[
            ("a", serde_json::json!(author.0)),
            ("d", serde_json::json!(d0.0)),
        ],
    ))
    .expect("the first occupant lands");

    let before = c.committed_commit_counter().expect("counter");

    // THE PROBE.
    let err = c
        .submit(commit(
            SET_CURRENT,
            &[
                ("a", serde_json::json!(author.0)),
                ("d", serde_json::json!(d1.0)),
            ],
        ))
        .expect_err("a second occupant of an at-most-one slice is refused");

    // (1) THE WITNESS — the same structured shape the landing check produced,
    // so nothing about the refusal was weakened by moving it earlier.
    let w = only_witness(&err);
    match &w {
        Witness::GrammarViolation { pool, part, .. } => {
            assert_eq!(pool, &PoolName::new(ALPHA));
            assert_eq!(*part, Some(0), "the witness still names the part");
        }
        other => panic!("expected a grammar violation, got {other:?}"),
    }
    assert_eq!(
        spine::schema_def::grammar_fault_tag(&w),
        Some("at_most_one_violated"),
        "the frozen fault tag, asserted on structure and never on prose"
    );

    // (2) NO ROW IS LEFT DECIDED. The attempt's row is kept forever as the
    // record of an abort — decided = 0, cleared = 1, no part
    // landed — and nothing at all is in landing.
    let spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
    assert!(
        spine.pending().expect("pending").is_empty(),
        "no un-cleared row survives the refusal"
    );
    assert!(
        c.in_landing(None).expect("in_landing").is_empty(),
        "and nothing is in landing"
    );
    let attempt = spine
        .find_by_hash(
            &commit(
                SET_CURRENT,
                &[
                    ("a", serde_json::json!(author.0)),
                    ("d", serde_json::json!(d1.0)),
                ],
            )
            .hash_identity()
            .expect("hash"),
        )
        .expect("read")
        .expect("the refused attempt is recorded");
    assert!(
        !attempt.decided && attempt.cleared,
        "the refused attempt is an ABORT, never a decided transaction"
    );
    assert!(attempt.parts.iter().all(|p| !p.landed));

    // (3) NOTHING LANDED. The slice still holds exactly its original
    // occupant, and the committed-commit counter never moved.
    let occupants = read_rows(
        &c,
        r#"MATCH (d: fixture-alpha/doc) -[current_of]-> (a: fixture-alpha/author) RETURN d.id"#,
        &[],
    );
    assert_eq!(occupants.len(), 1, "exactly one occupant, still");
    assert_eq!(occupants[0]["d.id"], serde_json::json!(d0.0));
    assert_eq!(c.committed_commit_counter().expect("counter"), before);

    // (4) THE LOCKS WERE RELEASED — proven by a later write on the very body
    // the refused statement claimed, under the 500ms ceiling above. Under the
    // old behaviour this came back `Busy` naming the wedged holder.
    c.submit(commit(
        r#"MATCH (a: fixture-alpha/author { id: $a }) SET a.name = $n"#,
        &[
            ("a", serde_json::json!(author.0)),
            ("n", serde_json::json!("ada l.")),
        ],
    ))
    .expect("a later write on the same author is granted, so the locks went");
    // And the slice itself is writable again: the reassignment the caller
    // should have written commits.
    c.submit(commit(
        REASSIGN_CURRENT,
        &[
            ("a", serde_json::json!(author.0)),
            ("new", serde_json::json!(d1.0)),
        ],
    ))
    .expect("the correctly-spelled reassignment commits");
    let occupants = read_rows(
        &c,
        r#"MATCH (d: fixture-alpha/doc) -[current_of]-> (a: fixture-alpha/author) RETURN d.id"#,
        &[],
    );
    assert_eq!(occupants.len(), 1);
    assert_eq!(occupants[0]["d.id"], serde_json::json!(d1.0));

    // (5) VALIDATED READS ARE UNAFFECTED. A cross-pool read over both pools
    // answers, rather than giving up `Busy` after N invalidated attempts —
    // which is what an un-cleared decided row made every read in the whole
    // cauldron do, forever.
    let cross = c
        .read(&ReadRequest {
            statement: r#"MATCH (d: fixture-alpha/doc) MATCH (t: fixture-beta/ticket)
                          RETURN d.id, t.id"#
                .to_string(),
            bindings: Bindings::new(),
            format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        })
        .expect("a cross-pool validated read still answers");
    assert!(
        !cross.escalated,
        "and answers warm, with nothing to retry on"
    );
}

/// AND RECOVERY HAS NOTHING TO WEDGE ON. The second half of the wedge: the
/// only recorded mitigation for the wedge was `Cauldron::recover()`, and it
/// was a deterministic infinite failure — it rolled the decided row forward,
/// hit the same occupancy refusal, and returned the same `GrammarViolation`
/// on every pass, forever.
///
/// With the check moved before the decided flip there is nothing for recovery
/// to find: the refused statement left an aborted row, which is cleared, so
/// it is not in the worklist at all. Recovery is a no-op, twice.
// RED: deleting the phase-10b call — `c.recover()` came back
// `Err(Refused { GrammarViolation { at_most_one_violated … } })` on both
// calls and `pending()` still held the decided row afterwards.
#[test]
fn recovery_after_an_occupancy_refusal_has_nothing_to_wedge_on() {
    let _env = env_shared();
    let name = "at-most-one-recovery";
    let c = cauldron(name);

    let author = create_author(&c, "ada");
    let d0 = create_doc(&c, "the current one");
    let d1 = create_doc(&c, "the pretender");
    c.submit(commit(
        SET_CURRENT,
        &[
            ("a", serde_json::json!(author.0)),
            ("d", serde_json::json!(d0.0)),
        ],
    ))
    .expect("the first occupant lands");
    let _ = c
        .submit(commit(
            SET_CURRENT,
            &[
                ("a", serde_json::json!(author.0)),
                ("d", serde_json::json!(d1.0)),
            ],
        ))
        .expect_err("refused");

    for pass in 1..=2 {
        assert_eq!(
            c.recover().expect("recovery RUNS — it does not refuse"),
            spine::recover::RecoveryReport::default(),
            "recovery pass {pass} has nothing to roll forward and nothing to abort"
        );
    }
    let spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
    assert!(spine.pending().expect("pending").is_empty());
    assert!(c.in_landing(None).expect("in_landing").is_empty());
    // And the cauldron still takes writes after recovery — it was never
    // degraded in the first place.
    let d2 = create_doc(&c, "after recovery");
    assert_eq!(title_of(&c, &d2.0).as_deref(), Some("after recovery"));
}

/// A DUPLICATE OF A RECOVERED COMMIT IS ANSWERED WITH THE ORIGINAL OUTCOME —
/// the same `Committed { deduplicated: true }` arm as any landed row (a
/// duplicate returns the original outcome from the record alone).
///
/// This used to be an open question: a row recovery rolled
/// forward held recovery's own marker in `result`, and the door refused the
/// duplicate by name (`RecoveredCommitReplyUnruled`). The question
/// dissolved: recovery now records which nodes each binding bound, projects
/// RETURN at its own cleared flip through the one projection the live door
/// runs, and stores a real projection. So this asserts three things: the
/// duplicate replays the projection; the row says it was RECOVERED as an
/// audit fact distinct from `result`'s shape; and — the contrast that keeps
/// the two apart — a result column that really is garbage still answers
/// `hard`, on a row that was ALSO recovered.
///
/// The crash is constructed the way `recovery_rolls_forward_from_the_payload…`
/// constructs one — a row on disk with its payload and the decided marker
/// flipped — so this runs in BOTH configurations rather than only under a
/// real kill. The payload's minted map is set by hand to match its CREATE
/// delta, exactly as the compiler would have set it.
// RED: (a) making `recover` clear the row with `mark_cleared` and the old
// `{"recovered": true}` marker — the duplicate came back `hard` ("will not
// parse") and `row.recovered` was false; (b) making `recover` store an EMPTY
// projection — the `returned` equality below failed on zero rows; (c) making
// `dedup_outcome` swallow a parse failure into `Returned::default()` — the
// corruption contrast collapsed, the poisoned control answering Committed.
#[test]
fn a_duplicate_of_a_recovered_commit_replays_the_original_outcome_and_corruption_still_answers_hard(
) {
    let _env = env_shared();
    let name = "recovered-dedup";
    let c = cauldron(name);

    // A real, well-formed envelope WITH a RETURN, crashed after its decided
    // flip and rolled forward by real recovery.
    let landed = commit(
        r#"CREATE (d: fixture-alpha/doc { title: "rolled forward", state: "draft" })
           RETURN d.id AS doc, d.title AS title, d.state AS state"#,
        &[],
    );
    let landed_hash = landed.hash_identity().expect("hash");
    let mut landed_payload = two_pool_payload();
    landed_payload.minted.insert(
        "d".to_string(),
        spine::pool::QualifiedNodeId {
            pool: PoolName::new(ALPHA),
            id: NodeId("n-rolled-alpha".into()),
        },
    );
    // A second identity, recovered the same way, whose result column is then
    // poisoned into GENUINE garbage — the control.
    let corrupt = commit(
        r#"CREATE (d: fixture-alpha/doc { title: "abandoned draft" })"#,
        &[],
    );
    let corrupt_hash = corrupt.hash_identity().expect("hash");
    let (landed_seq, corrupt_seq) = {
        let mut spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
        let a = spine
            .append_pending(&landed, &landed_hash, &landed_payload)
            .expect("append");
        spine.mark_decided(a).expect("decided");
        let b = spine
            .append_pending(&corrupt, &corrupt_hash, &abandoned_payload())
            .expect("append");
        spine.mark_decided(b).expect("decided");
        (a, b)
    };
    let report = c.recover().expect("recovery rolls both forward");
    assert_eq!(report.rolled_forward.len(), 2);
    assert_eq!(c.committed_commit_counter().expect("counter"), 2);

    // THE ROW: recovered as an audit fact, holding a REAL projection.
    let expected = Returned(vec![std::collections::BTreeMap::from([
        ("doc".to_string(), serde_json::json!("n-rolled-alpha")),
        ("title".to_string(), serde_json::json!("rolled forward")),
        ("state".to_string(), serde_json::json!("draft")),
    ])]);
    {
        let spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
        let row = spine.row_at(landed_seq).expect("row");
        assert!(row.decided && row.cleared && row.recovered, "recovered, as a column");
        assert_eq!(
            row.result,
            Some(serde_json::to_value(&expected).expect("json")),
            "the result column holds a projection, never a placeholder"
        );
        assert!(spine.row_at(corrupt_seq).expect("row").recovered);
    }

    // Poison the control's result column into something that is neither a
    // projection nor anything else: real corruption, and nothing else.
    {
        let conn = rusqlite::Connection::open(c_root(name).join(SPINE_FILE)).expect("direct SQL");
        conn.execute(
            "UPDATE transactions SET result = ?1 WHERE seq = ?2",
            rusqlite::params![r#"{"nonsense":true}"#, corrupt_seq.0],
        )
        .expect("poison one result column");
    }

    // THE DUPLICATE: the original outcome, from the row alone.
    match c.submit(landed).expect("a recovered commit is answered, not refused") {
        Outcome::Committed {
            tx,
            seq,
            returned,
            deduplicated,
        } => {
            assert!(deduplicated, "answered AS a duplicate");
            assert_eq!(tx, landed_hash);
            assert_eq!(seq, landed_seq, "from the very row recovery cleared");
            assert_eq!(returned, expected, "THE ORIGINAL OUTCOME — the projection, replayed");
        }
        other => panic!("expected Committed, got {other:?}"),
    }

    // AND CORRUPTION IS STILL CORRUPTION. The control's row really is
    // unparseable, and it answers `hard` — recovered-ness never softened it.
    let control = c
        .submit(corrupt)
        .expect_err("a genuinely corrupt result column is still a hard failure");
    assert_eq!(control.kind(), "hard");

    // Neither duplicate landed anything a second time.
    assert_eq!(c.committed_commit_counter().expect("counter"), 2);
    assert_eq!(docs_titled(&c, "rolled forward"), 1);
    assert_eq!(docs_titled(&c, "abandoned draft"), 1);
    let spine = SpineDb::open(&c_root(name).join(SPINE_FILE)).expect("spine");
    assert_eq!(spine.attempts_of(&landed_hash).expect("history").len(), 1);
    assert_eq!(spine.attempts_of(&corrupt_hash).expect("history").len(), 1);
}

/// The at-most-one probe, verbatim: a second `current_of` into an author that already
/// has one, with no removal. Shared with `lock_behaviour.rs`'s spelling.
const SET_CURRENT: &str = r#"MATCH (a: fixture-alpha/author { id: $a })
   MATCH (d: fixture-alpha/doc { id: $d })
   CREATE (d) -[current_of]-> (a)"#;

/// What the caller SHOULD have written — the occupant found by the slice, not
/// named by the caller.
const REASSIGN_CURRENT: &str = r#"MATCH (a: fixture-alpha/author { id: $a })
   MATCH (old: fixture-alpha/doc) -[current_of]-> (a)
   MATCH (new: fixture-alpha/doc { id: $new })
   REMOVE (old) -[current_of]-> (a)
   CREATE (new) -[current_of]-> (a)"#;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

static ENV: RwLock<()> = RwLock::new(());

fn env_shared() -> RwLockReadGuard<'static, ()> {
    ENV.read().unwrap_or_else(|e| e.into_inner())
}

fn env_exclusive() -> RwLockWriteGuard<'static, ()> {
    ENV.write().unwrap_or_else(|e| e.into_inner())
}

fn c_root(name: &str) -> std::path::PathBuf {
    test_scratch::tmp_path(format!("spine-door-{name}-{}", std::process::id()))
}

fn cauldron(name: &str) -> Cauldron {
    cauldron_with(name, SpineConfig::default())
}

fn cauldron_with(name: &str, config: SpineConfig) -> Cauldron {
    let dir = c_root(name);
    let _ = std::fs::remove_dir_all(&dir);
    Cauldron::create(
        &dir,
        &[fixtures::fixture_alpha(), fixtures::fixture_beta()],
        config,
    )
    .expect("create the cauldron")
}

fn envelope(statement: &str, params: &[(&str, serde_json::Value)], mode: Mode) -> Envelope {
    let mut bindings = Bindings::new();
    for (k, v) in params {
        bindings.insert((*k).to_string(), v.clone());
    }
    Envelope {
        statement: statement.to_string(),
        bindings,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "door-smoke".into(),
                dispatch: "task-05".into(),
            },
            instrument: Instrument::door(),
        },
        mode,
        idempotency_nonce: None,
    }
}

fn commit(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    envelope(statement, params, Mode::Commit)
}

fn validate_only(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    envelope(statement, params, Mode::Validate)
}

fn expect_committed(
    out: Outcome,
    want_dedup: bool,
) -> (spine::envelope::TxHash, spine::spine_db::Seq, Returned) {
    match out {
        Outcome::Committed {
            tx,
            seq,
            returned,
            deduplicated,
        } => {
            assert_eq!(deduplicated, want_dedup);
            (tx, seq, returned)
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn only_witness(err: &SpineError) -> Witness {
    match err {
        SpineError::Refused { witnesses } => {
            assert_eq!(witnesses.len(), 1, "one refusal, one witness");
            witnesses[0].clone()
        }
        other => panic!("expected a structured refusal, got {other:?}"),
    }
}

fn create_doc(c: &Cauldron, title: &str) -> NodeId {
    let out = c
        .submit(commit(
            r#"CREATE (d: fixture-alpha/doc { title: $t, state: "draft" }) RETURN d.id"#,
            &[("t", serde_json::json!(title))],
        ))
        .expect("create a doc");
    match out {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0]["d.id"].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn create_author(c: &Cauldron, name: &str) -> NodeId {
    let out = c
        .submit(commit(
            r#"CREATE (a: fixture-alpha/author { name: $n }) RETURN a.id"#,
            &[("n", serde_json::json!(name))],
        ))
        .expect("create an author");
    match out {
        Outcome::Committed { returned, .. } => {
            NodeId(returned.0[0]["a.id"].as_str().expect("id").to_string())
        }
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn read_rows(
    c: &Cauldron,
    statement: &str,
    params: &[(&str, serde_json::Value)],
) -> Vec<std::collections::BTreeMap<String, serde_json::Value>> {
    let mut bindings = Bindings::new();
    for (k, v) in params {
        bindings.insert((*k).to_string(), v.clone());
    }
    c.read(&ReadRequest {
        statement: statement.to_string(),
        bindings,
        format_version: spine::envelope::ENVELOPE_FORMAT_VERSION,
    })
    .expect("bulk read")
    .rows
    .0
}

fn state_of(c: &Cauldron, id: &str) -> Option<String> {
    let rows = read_rows(
        c,
        r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.state"#,
        &[("id", serde_json::json!(id))],
    );
    rows.first()
        .and_then(|r| r["d.state"].as_str().map(str::to_string))
}

/// A doc's WHOLE-NODE revision, read back through the read door.
fn rev_of(c: &Cauldron, id: &str) -> Option<i64> {
    let rows = read_rows(
        c,
        r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN REV(d) AS r"#,
        &[("id", serde_json::json!(id))],
    );
    rows.first().and_then(|r| r["r"].as_i64())
}

fn title_of(c: &Cauldron, id: &str) -> Option<String> {
    let rows = read_rows(
        c,
        r#"MATCH (d: fixture-alpha/doc { id: $id }) RETURN d.title"#,
        &[("id", serde_json::json!(id))],
    );
    rows.first()
        .and_then(|r| r["d.title"].as_str().map(str::to_string))
}

fn docs_titled(c: &Cauldron, title: &str) -> usize {
    read_rows(
        c,
        r#"MATCH (d: fixture-alpha/doc) RETURN d.title"#,
        &[],
    )
    .iter()
    .filter(|r| r["d.title"] == serde_json::json!(title))
    .count()
}

/// A payload with no parts — enough to open a spine row whose only purpose
/// is to carry a sequence number for a lock claim.
fn empty_payload() -> CompiledPayload {
    CompiledPayload {
        parts: Vec::new(),
        minted: std::collections::BTreeMap::new(),
    }
}

/// A one-pool payload distinguishable from [`two_pool_payload`] by what it
/// would land — used to prove that an ABANDONED attempt's payload is never
/// replayed, even though it shares its identity's hash.
fn abandoned_payload() -> CompiledPayload {
    use spine::delta::{CompiledPart, Delta};
    use std::collections::BTreeMap;
    CompiledPayload {
        parts: vec![CompiledPart {
            pool: PoolName::new(ALPHA),
            schema_version: 1,
            deltas: vec![Delta::CreateNode {
                id: NodeId("n-abandoned-alpha".into()),
                node_type: "doc".into(),
                state: Some("draft".into()),
                fields: BTreeMap::from([(
                    "title".to_string(),
                    serde_json::json!("abandoned draft"),
                )]),
            }],
            premises: Vec::new(),
            far_versions: BTreeMap::new(),
        }],
        minted: BTreeMap::new(),
    }
}

/// A two-pool compiled payload, built by hand the way a crashed door would
/// have left one on the row: an alpha `doc` and a beta `ticket`, ordered by
/// ascending pool name (the frozen landing order).
fn two_pool_payload() -> CompiledPayload {
    use spine::delta::{CompiledPart, Delta};
    use std::collections::BTreeMap;
    CompiledPayload {
        parts: vec![
            CompiledPart {
                pool: PoolName::new(ALPHA),
                schema_version: 1,
                deltas: vec![Delta::CreateNode {
                    id: NodeId("n-rolled-alpha".into()),
                    node_type: "doc".into(),
                    state: Some("draft".into()),
                    fields: BTreeMap::from([(
                        "title".to_string(),
                        serde_json::json!("rolled forward"),
                    )]),
                }],
                premises: Vec::new(),
                far_versions: BTreeMap::new(),
            },
            CompiledPart {
                pool: PoolName::new(BETA),
                schema_version: 2,
                deltas: vec![Delta::CreateNode {
                    id: NodeId("n-rolled-beta".into()),
                    node_type: "ticket".into(),
                    state: Some("open".into()),
                    fields: BTreeMap::new(),
                }],
                premises: Vec::new(),
                far_versions: BTreeMap::new(),
            },
        ],
        minted: BTreeMap::new(),
    }
}
