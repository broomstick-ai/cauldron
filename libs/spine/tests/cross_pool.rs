//! **The cross-pool grammar**: outbound reach, target lookup, and
//! ghost-edge minting, proven across THREE pools of TWO kinds plus the
//! projected `spine` namespace.
//!
//! The fixtures here are TEST FIXTURES, NOT THE REAL SCHEMAS (that applies
//! to `fixture_workspace` and `fixture_blueprint` exactly as
//! to alpha and beta): two workspace instances (`fixture-ws-a`,
//! `fixture-ws-b`, one kind, one grammar SHAPE, pinned DIFFERENTLY — ws-a at
//! 3, ws-b at 5, so case (a)'s far pin cannot be confused with the source's)
//! and one blueprint (`fixture-bp`, pinned at 4).
//!
//! Two worked cases, made real across pools:
//!
//! - **(a)** an issue in one workspace `blocked_by` an issue in ANOTHER
//!   workspace — one statement, two pools of one kind, one reach
//!   declaration covering both;
//! - **(b)** a milestone in a workspace `depends_on` a migration, and a
//!   release, in a blueprint — the workspace-points-at-blueprint seam.
//!
//! Plus the ghost-edge case: an issue
//! `landed_by` the projected `spine/<hash>` of an earlier commit.
//!
//! Every negative is asserted on STRUCTURE — the frozen grammar-fault tag
//! (`grammar_fault_tag`) and the witness's `pool` — never on prose, and after every refusal the whole-cauldron fingerprint proves that
//! NOTHING landed in ANY pool. Every checker here was forced RED at least
//! once; the poison is named in a `// RED:` comment above each test.

use spine::checkers::{self, Finding};
use spine::config::SpineConfig;
use spine::delta::CompiledPart;
use spine::envelope::{
    Bindings, Envelope, Initiator, Instrument, Mode, Provenance, ENVELOPE_FORMAT_VERSION,
};
use spine::error::SpineError;
use spine::ladle::compile::{PROJECTED_PATTERN_FORM, STAGED_OUT_FORMS};
use spine::pool::{NodeId, Pool};
use spine::project;
use spine::schema_def::fixtures::{
    cross_pool_grammars, fixture_blueprint, fixture_workspace, fixture_workspace_at, BP, WS_A,
    WS_B, WS_B_PIN, WORKSPACE_PIN,
};
use spine::schema_def::{grammar_fault_tag, PoolGrammar, PoolKind, PoolName, TypeRef};
use spine::spine_db::{SpineDb, SPINE_FILE};
use spine::witness::Witness;
use spine::{Cauldron, Outcome, ReadRequest};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

test_scratch::install!();

// ---------------------------------------------------------------------------
// case (a): an issue blocked by an issue in ANOTHER workspace
// ---------------------------------------------------------------------------

/// One statement, two pools of ONE kind: the edge lands in ws-a's part (the
/// source end owns the edge), and the part records ws-b's pin as the far
/// version its cross-pool edge validated against — "check the schema version
/// on it", made durable on the row.
///
/// The SAME edge kind, under the SAME declaration, also lands inside one
/// workspace: a reach by kind covers the declaring pool too ("issue blocked
/// by an issue" is one relationship, one declaration), and a purely local
/// edge records no far version at all.
///
/// The two workspaces are pinned DIFFERENTLY (ws-a 3, ws-b 5), so
/// the far-pin assertion below proves "check the schema version on it" ON
/// ITS OWN: a build recording the SOURCE's pin as the far pin writes
/// `{ws-b: 3}` and fails here, without leaning on case (b) or the ghost case.
// RED: (a) `compile` no longer filling `far_versions` — the recorded map
// came back empty; (b) `EdgeKindDef::permits` ignoring same-kind reaches for
// a local far end — the in-workspace `blocked_by` refused
// `wrong_endpoint_type`; (c) `validate_reach` skipping the far-type lookup
// — the milestone-typed negative below stopped refusing; (d)
// `compile` recording the SOURCE grammar's version as the far pin — the
// far-pin assertion fired `{ws-b: 3}` != `{ws-b: 5}` (with the two
// workspaces sharing pin 3 this test would stay GREEN under that mutation).
#[test]
fn case_a_an_issue_blocked_by_an_issue_in_another_workspace_lands_with_the_far_pin_recorded() {
    let dir = root("case-a");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    let pin = |p: &str| c.grammar(&PoolName::new(p)).expect("pinned").version;
    // The precondition that makes this case discriminating by itself: if the
    // fixtures ever drift back to one shared pin, say so here rather than
    // letting the far-pin assertion go quietly non-discriminating again.
    assert_ne!(
        pin(WS_A),
        pin(WS_B),
        "the two workspaces must be pinned differently, or the far pin cannot be told from the source's"
    );
    assert_eq!((pin(WS_A), pin(WS_B)), (WORKSPACE_PIN, WS_B_PIN));
    let a = create_issue(&c, WS_A, "ws-a: ship the thing");
    let b = create_issue(&c, WS_B, "ws-b: the thing it waits on");

    let (tx, _) = committed(&c, commit(
        r#"MATCH (a: fixture-ws-a/issue { id: $a })
           MATCH (b: fixture-ws-b/issue { id: $b })
           CREATE (a) -[blocked_by]-> (b)
           RETURN a.id, b.id"#,
        &[("a", serde_json::json!(a.0)), ("b", serde_json::json!(b.0))],
    ));

    // The row's ONE part is ws-a's (the source end), stamped with ws-a's own
    // pin, and it records ws-b's pin as the far version.
    let parts = parts_of(&dir, &tx);
    assert_eq!(parts.len(), 1, "one part: the edge lives in the SOURCE end's pool");
    assert_eq!(parts[0].pool, PoolName::new(WS_A));
    assert_eq!(parts[0].schema_version, pin(WS_A));
    assert_eq!(
        parts[0].far_versions,
        BTreeMap::from([(PoolName::new(WS_B), pin(WS_B))]),
        "the FAR pool's own pinned version is recorded on the compiled part, not the source's"
    );
    // The edge is stored in ws-a and names ws-b explicitly; ws-b holds no
    // edge (one landing, one file).
    let edges = edges_from(&dir, WS_A, &a);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].kind, "blocked_by");
    assert_eq!(edges[0].dst.pool, PoolName::new(WS_B));
    assert_eq!(edges[0].dst.id, b);
    assert!(edges_from(&dir, WS_B, &b).is_empty());

    // The read door walks the cross-pool hop.
    let rows = read(&c,
        r#"MATCH (a: fixture-ws-a/issue { id: $a }) -[blocked_by]-> (b: fixture-ws-b/issue) RETURN b.id"#,
        &[("a", serde_json::json!(a.0))],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["b.id"], serde_json::json!(b.0));

    // The declaring pool included: the same kind, inside ONE workspace.
    let a2 = create_issue(&c, WS_A, "ws-a: another");
    let (tx2, _) = committed(&c, commit(
        r#"MATCH (a: fixture-ws-a/issue { id: $a })
           MATCH (x: fixture-ws-a/issue { id: $x })
           CREATE (a) -[blocked_by]-> (x)"#,
        &[("a", serde_json::json!(a.0)), ("x", serde_json::json!(a2.0))],
    ));
    let parts = parts_of(&dir, &tx2);
    assert_eq!(parts.len(), 1);
    assert!(
        parts[0].far_versions.is_empty(),
        "a local far end records no far version; got {:?}",
        parts[0].far_versions
    );
    assert_eq!(edges_from(&dir, WS_A, &a).len(), 2);

    // The cauldron answers "which pools are of kind K".
    assert_eq!(
        c.pools_of_kind(&PoolKind::new("workspace")),
        vec![&PoolName::new(WS_A), &PoolName::new(WS_B)]
    );
    assert_eq!(c.pools_of_kind(&PoolKind::new("blueprint")), vec![&PoolName::new(BP)]);
    assert!(c.pools_of_kind(&PoolKind::new("engine")).is_empty());
    assert!(c.pools_of_kind(&PoolKind::new(project::PROJECTED_KIND)).is_empty());
    assert_eq!(c.kind_of(&PoolName::new(BP)), Some(&PoolKind::new("blueprint")));
    assert_eq!(c.kind_of(&PoolName::new("nowhere")), None);
}

// ---------------------------------------------------------------------------
// case (b): a milestone depending on a blueprint's migration
// and release
// ---------------------------------------------------------------------------

/// Two reaches of one edge kind toward one kind, two far types, one
/// statement, two edges — both land, and the part records the blueprint's
/// pin once.
// RED: `depends_on` declared with only the `migration` reach — the release
// edge refused `reach_undeclared` and the statement landed nothing.
#[test]
fn case_b_a_milestone_depends_on_a_blueprint_migration_and_a_release() {
    let dir = root("case-b");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    let m = create_milestone(&c, WS_A, "v1");
    let mig = create_bp(&c, "migration", "0001-init", "planned");
    let rel = create_bp(&c, "release", "1.0.0", "cut");

    let (tx, _) = committed(&c, commit(
        r#"MATCH (m: fixture-ws-a/milestone { id: $m })
           MATCH (g: fixture-bp/migration { id: $g })
           MATCH (r: fixture-bp/release { id: $r })
           CREATE (m) -[depends_on]-> (g)
           CREATE (m) -[depends_on]-> (r)"#,
        &[
            ("m", serde_json::json!(m.0)),
            ("g", serde_json::json!(mig.0)),
            ("r", serde_json::json!(rel.0)),
        ],
    ));
    let parts = parts_of(&dir, &tx);
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0].pool, PoolName::new(WS_A));
    assert_eq!(
        parts[0].far_versions,
        BTreeMap::from([(PoolName::new(BP), fixture_blueprint().version)])
    );
    let edges = edges_from(&dir, WS_A, &m);
    assert_eq!(edges.len(), 2);
    assert!(edges.iter().all(|e| e.kind == "depends_on" && e.dst.pool == PoolName::new(BP)));
    let targets: Vec<&NodeId> = edges.iter().map(|e| &e.dst.id).collect();
    assert!(targets.contains(&&mig) && targets.contains(&&rel));
}

/// Case (b) with the far node MINTED in the same statement: the milestone
/// `depends_on` a migration the SAME statement creates in the blueprint. The
/// far node exists nowhere yet, so its actual type comes from the MINTED
/// branch of `compile`'s `far_type` closure — the branch no other test
/// reaches. The row carries TWO parts in the frozen ascending-pool-name
/// order: `fixture-bp` first (the create, at the blueprint's own pin, no far
/// record), `fixture-ws-a` second (the edge, at ws-a's pin, recording
/// `far_versions {fixture-bp: <bp pin>}`).
///
/// The same with a minted `module` refuses `reach_undeclared`, stamped with
/// the SOURCE pool and naming the ws-a PART (part 1): the minted far type is
/// judged exactly as a matched one is.
///
/// Written `MATCH ... CREATE (g) ... CREATE (m) -[..]-> (g)`: spelling the
/// CREATE before the MATCH is what the frozen clause order (MATCH,
/// WHERE, EXPECT, CREATE/SET/REMOVE, RETURN) refuses as malformed.
// RED: the minted branch of `compile`'s `far_type` closure removed — the far
// node read as UNKNOWN and the landing statement refused `unknown_node`
// stamped `fixture-bp` (the far pool, via `GrammarFault::in_pool`): the
// `committed` helper's `expect` fired on that witness.
#[test]
fn case_b_toward_a_far_node_minted_in_the_same_statement_lands_two_parts() {
    use spine::delta::Delta;
    let dir = root("case-b-minted");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    let bp_pin = c.grammar(&PoolName::new(BP)).expect("pinned").version;
    let ws_pin = c.grammar(&PoolName::new(WS_A)).expect("pinned").version;
    let m = create_milestone(&c, WS_A, "v2");

    let (tx, returned) = committed(&c, commit(
        r#"MATCH (m: fixture-ws-a/milestone { id: $m })
           CREATE (g: fixture-bp/migration { name: $n, state: "planned" })
           CREATE (m) -[depends_on]-> (g)
           RETURN g.id"#,
        &[("m", serde_json::json!(m.0)), ("n", serde_json::json!("0002-minted"))],
    ));
    let g = returned_id(&returned, "g.id");
    let parts = parts_of(&dir, &tx);
    assert_eq!(
        parts.iter().map(|p| p.pool.clone()).collect::<Vec<_>>(),
        vec![PoolName::new(BP), PoolName::new(WS_A)],
        "two parts, in the frozen ascending-pool-name order"
    );
    // Part 0: the blueprint's create, at the blueprint's own pin.
    assert_eq!(parts[0].schema_version, bp_pin);
    assert!(parts[0].far_versions.is_empty(), "the create reaches nowhere");
    assert!(
        matches!(
            parts[0].deltas.as_slice(),
            [Delta::CreateNode { id, node_type, .. }] if *id == g && node_type == "migration"
        ),
        "{:?}",
        parts[0].deltas
    );
    // Part 1: the workspace's edge, at ws-a's pin, recording the far pin.
    assert_eq!(parts[1].schema_version, ws_pin);
    assert_eq!(
        parts[1].far_versions,
        BTreeMap::from([(PoolName::new(BP), bp_pin)]),
        "the minted far node's pool pin is recorded exactly as a matched one's"
    );
    let edges = edges_from(&dir, WS_A, &m);
    assert_eq!(edges.len(), 1);
    assert_eq!((edges[0].kind.as_str(), &edges[0].dst.pool, &edges[0].dst.id), (
        "depends_on",
        &PoolName::new(BP),
        &g
    ));
    // The minted migration is really in the blueprint's file.
    let rows = read(&c,
        r#"MATCH (g: fixture-bp/migration { id: $g }) RETURN g.name"#,
        &[("g", serde_json::json!(g.0))],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["g.name"], serde_json::json!("0002-minted"));

    // NEGATIVE: the same shape toward a minted `module`, which no reach names.
    let before = Fingerprint::of(&c, &dir);
    let env = commit(
        r#"MATCH (m: fixture-ws-a/milestone { id: $m })
           CREATE (x: fixture-bp/module { name: $n })
           CREATE (m) -[depends_on]-> (x)"#,
        &[("m", serde_json::json!(m.0)), ("n", serde_json::json!("never"))],
    );
    let hash = env.hash_identity().expect("hash identity");
    let err = c.submit(env).expect_err("a minted `module` is as unreachable as a matched one");
    expect_fault(&err, WS_A, "reach_undeclared");
    match only_witness(&err) {
        Witness::GrammarViolation { part, .. } => assert_eq!(
            part,
            Some(1),
            "the witness names the ws-a PART (bp is part 0 by the frozen order)"
        ),
        other => panic!("expected a GrammarViolation, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "depends_on toward a minted module");
    assert_no_attempt(&dir, &hash, "depends_on toward a minted module");
}

// ---------------------------------------------------------------------------
// ghost-edge minting: provenance toward the projected namespace
// ---------------------------------------------------------------------------

/// An issue `landed_by` the `spine/<hash>` of an EARLIER commit: the ghost
/// is matched BY IDENTIFIER (resolved through `project::resolve_ghost` — the
/// one matcher, both doors), the reach toward kind `spine` is judged by the
/// same validator as every other cross-pool edge (the projected namespace's
/// grammar is CODE), and the part records the projection's version, which is
/// the envelope format version. The same edge toward a hash naming NO row
/// refuses `GhostTargetUnknown` — `project::guard_edge_target`'s one caller
/// — and lands nothing.
// RED: (a) `exec::guard_ghost_edge_targets` removed — the edge toward the
// absent row "succeeded" as an empty match landing nothing, and the
// `expect_err` fired; (b) `compile` writing the far version as the spine
// pool's `0` — the recorded-version assertion fired; (c) `read::match_ghost`
// resolving nothing — the landing statement matched no ghost and, with the
// guard passing, minted zero edges: the edge-count assertion fired.
#[test]
fn a_ghost_edge_lands_toward_a_row_and_refuses_toward_none() {
    let dir = root("ghost");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    let i = create_issue(&c, WS_A, "landed by an earlier commit");
    let earlier = {
        let (tx, _) = committed(&c, commit(
            r#"MATCH (i: fixture-ws-a/issue { id: $i }) SET i.title = $t"#,
            &[("i", serde_json::json!(i.0)), ("t", serde_json::json!("the earlier commit"))],
        ));
        tx
    };
    let ghost = project::ghost_id(&earlier);

    // MINT: toward a ghost that names a row.
    let (tx, returned) = committed(&c, commit(
        r#"MATCH (i: fixture-ws-a/issue { id: $i })
           MATCH (g: spine/txn { id: $h })
           CREATE (i) -[landed_by]-> (g)
           RETURN g.state, g.id"#,
        &[("i", serde_json::json!(i.0)), ("h", serde_json::json!(ghost.0))],
    ));
    assert_eq!(returned.len(), 1, "the ghost bound exactly one row");
    assert_eq!(returned[0]["g.state"], serde_json::json!("cleared"));
    assert_eq!(returned[0]["g.id"], serde_json::json!(ghost.0));
    let parts = parts_of(&dir, &tx);
    assert_eq!(parts.len(), 1, "no part ever lands in the projection");
    assert_eq!(parts[0].pool, PoolName::new(WS_A));
    assert_eq!(
        parts[0].far_versions,
        BTreeMap::from([(PoolName::new(project::PROJECTED_POOL), ENVELOPE_FORMAT_VERSION)]),
        "the projection's version is the envelope format version"
    );
    let edges = edges_from(&dir, WS_A, &i);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].kind, "landed_by");
    assert_eq!(edges[0].dst.pool, PoolName::new(project::PROJECTED_POOL));
    assert_eq!(edges[0].dst.id, ghost);

    // The read door resolves the ghost by identifier and returns the
    // projected record — computed on read.
    let rows = read(&c,
        r#"MATCH (g: spine/txn { id: $h }) RETURN g.state, g.statement"#,
        &[("h", serde_json::json!(ghost.0))],
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["g.state"], serde_json::json!("cleared"));
    assert!(rows[0]["g.statement"].as_str().unwrap().contains("SET i.title"));
    // ...and a ghost naming no row reads as the EMPTY match, which succeeds
    // — it is aiming an EDGE at it that refuses.
    let none = read(&c,
        r#"MATCH (g: spine/txn { id: $h }) RETURN g.state"#,
        &[("h", serde_json::json!("ab".repeat(32)))],
    );
    assert!(none.is_empty());

    // REFUSE: the same mint toward a hash naming no row.
    let before = Fingerprint::of(&c, &dir);
    let err = c
        .submit(commit(
            r#"MATCH (i: fixture-ws-a/issue { id: $i })
               MATCH (g: spine/txn { id: $h })
               CREATE (i) -[landed_by]-> (g)"#,
            &[("i", serde_json::json!(i.0)), ("h", serde_json::json!("ab".repeat(32)))],
        ))
        .expect_err("an edge minted toward a ghost identifier that names no row is refused");
    match only_witness(&err) {
        Witness::GhostTargetUnknown { target } => {
            assert!(target.contains(&"ab".repeat(32)), "names the address: {target}")
        }
        other => panic!("expected GhostTargetUnknown, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "a ghost edge toward no row");
}

/// Every match form into `spine` OTHER than by identifier stays staged OUT
/// under ONE frozen name, at both doors: by type/state (a population scan),
/// by field, and as either end of a hop.
// RED: (a) the coverage row's detector made to ignore hops — the hop form
// matched (and failed Hard on a missing snapshot) instead of refusing by
// name; (b) `match_ghost` accepting a head with only `state` — the
// by-state form bound rows.
#[test]
fn every_other_match_form_into_the_projection_refuses_by_its_staged_out_name() {
    let dir = root("ghost-forms");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    let i = create_issue(&c, WS_A, "present");
    let before = Fingerprint::of(&c, &dir);
    assert!(STAGED_OUT_FORMS.contains(&PROJECTED_PATTERN_FORM));

    // (label, the read-door statement, the write-door statement — the same
    // pattern with a mutation on a real pool riding along)
    let forms: [(&str, &str, &str); 4] = [
        (
            "by type",
            r#"MATCH (g: spine/txn) RETURN g.id"#,
            r#"MATCH (g: spine/txn) MATCH (i: fixture-ws-a/issue { id: $i }) SET i.state = "closed""#,
        ),
        (
            "by state",
            r#"MATCH (g: spine/txn { state: "cleared" }) RETURN g.id"#,
            r#"MATCH (g: spine/txn { state: "cleared" }) MATCH (i: fixture-ws-a/issue { id: $i }) SET i.state = "closed""#,
        ),
        (
            "by field",
            r#"MATCH (g: spine/txn { statement: "x" }) RETURN g.id"#,
            r#"MATCH (g: spine/txn { statement: "x" }) MATCH (i: fixture-ws-a/issue { id: $i }) SET i.state = "closed""#,
        ),
        (
            "by hop",
            r#"MATCH (i: fixture-ws-a/issue) -[landed_by]-> (g: spine/txn { id: $h }) RETURN g.id"#,
            r#"MATCH (i: fixture-ws-a/issue { id: $i }) -[landed_by]-> (g: spine/txn { id: $h }) SET i.state = "closed""#,
        ),
    ];
    for (label, text, write) in forms {
        let params: Vec<(&str, serde_json::Value)> = if text.contains("$h") {
            vec![("h", serde_json::json!("ab".repeat(32)))]
        } else {
            vec![]
        };
        // the READ door
        let err = match c.read(&request(text, &params)) {
            Err(e) => e,
            Ok(_) => panic!("{label}: the read door must refuse"),
        };
        match only_witness_of(&err) {
            Witness::UnimplementedForm { form } => {
                assert_eq!(form, PROJECTED_PATTERN_FORM, "{label} at the read door")
            }
            other => panic!("{label}: expected UnimplementedForm, got {other:?}"),
        }
        // the WRITE door
        let mut write_params = params.clone();
        write_params.push(("i", serde_json::json!(i.0)));
        let err = c
            .submit(commit(write, &write_params))
            .expect_err("the write door must refuse");
        match only_witness(&err) {
            Witness::UnimplementedForm { form } => {
                assert_eq!(form, PROJECTED_PATTERN_FORM, "{label} at the write door")
            }
            other => panic!("{label}: expected UnimplementedForm, got {other:?}"),
        }
    }
    before.assert_nothing_landed(&c, &dir, "the staged-out projection forms");
}

// ---------------------------------------------------------------------------
// the ghost-WRITE surface
// ---------------------------------------------------------------------------
//
// A MATCH-by-identifier ghost is BINDABLE at the write door, which
// which opens two write surfaces onto the projection: a SET on the matched ghost,
// and an edge with the ghost as its SOURCE. Both refuse
// `ProjectedNamespaceWrite` from the pen's ONE net — `compile`'s three
// write-target sites, all through `project::guard_write_target` — in phase 7,
// BEFORE the row append at phase 8. Each arm below proves all three halves:
// the witness (variant, and `pool` = the projected pool, and `target` = the
// ghost's address), the whole-cauldron fingerprint unchanged, and NO spine
// row for the refused envelope's identity at all (`attempts_of` its hash is
// empty — not even an aborted attempt, which is what a refusal AFTER the
// append would leave behind).
//
// Placed here rather than in `refusal_witnesses.rs`'s two-names test: every
// arm needs a REAL ghost (an earlier commit's hash) and the workspace
// fixture's `landed_by` reach toward kind `spine`, both of which live in this
// suite's harness; the refusal_witnesses test runs on alpha/beta, which
// declare no reach toward the projection.

/// `SET g.<field>` on a matched ghost refuses, pre-append — alone, and riding
/// in a statement that ALSO carries a legal mutation on a real pool and a
/// RETURN (the legal SET is emitted FIRST, so the refusal is proven WHOLE:
/// the issue's state does not move).
// RED: (a) `guard_write_target` returning `Ok(())` — both arms stopped
// refusing by name: the ghost's `txn` states accept "pending", the row was
// APPENDED, and the door failed HARD at phase 10b ("no pinned grammar for
// pool `spine` while checking at-most-one occupancy"), so
// `expect_projected_write_refused_pre_append` panicked on
// `expected a structured refusal, got Hard`; (b) ONLY the SET site
// (`emit_set_field`) silenced — the same HARD failure at 10b: the SET site is
// this surface's only by-name net.
#[test]
fn a_set_on_a_matched_ghost_refuses_before_the_row_is_appended() {
    let dir = root("ghost-set");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    let i = create_issue(&c, WS_A, "issue");
    let ghost = earlier_ghost(&c, &i);
    let before = Fingerprint::of(&c, &dir);

    // ARM 1 — the SET alone.
    let target = expect_projected_write_refused_pre_append(
        &c,
        &dir,
        commit(
            r#"MATCH (g: spine/txn { id: $h }) SET g.state = "pending""#,
            &[("h", serde_json::json!(ghost.0))],
        ),
        "SET on a matched ghost",
    );
    assert_eq!(target, ghost.0, "the witness names the ghost's address");
    before.assert_nothing_landed(&c, &dir, "SET on a matched ghost");

    // ARM 2 — the same SET inside a statement that also carries a LEGAL
    // mutation on a real pool (emitted first) and a RETURN: the WHOLE
    // statement refuses and the legal half lands nowhere.
    let target = expect_projected_write_refused_pre_append(
        &c,
        &dir,
        commit(
            r#"MATCH (i: fixture-ws-a/issue { id: $i })
               MATCH (g: spine/txn { id: $h })
               SET i.state = "closed"
               SET g.state = "pending"
               RETURN i.state, g.state"#,
            &[("i", serde_json::json!(i.0)), ("h", serde_json::json!(ghost.0))],
        ),
        "SET on a ghost beside a legal SET and a RETURN",
    );
    assert_eq!(target, ghost.0);
    before.assert_nothing_landed(&c, &dir, "SET on a ghost beside a legal SET");
    let rows = read(&c,
        r#"MATCH (i: fixture-ws-a/issue { id: $i }) RETURN i.state"#,
        &[("i", serde_json::json!(i.0))],
    );
    assert_eq!(rows[0]["i.state"], serde_json::json!("open"), "the legal half did not land");
}

/// An edge with a matched ghost as its SOURCE refuses, pre-append, in both
/// arrow spellings — `CREATE (g) -[k]-> (i)` and `CREATE (i) <-[k]- (g)`
/// (the right-to-left form puts the ghost at the SOURCE end too: the source
/// end is where the arrow's tail is, whatever side of the text it sits on).
/// The kind is `landed_by`, which the workspace declares toward kind `spine`
/// — i.e. the edge a caller might plausibly write backwards.
// RED: (a) `guard_write_target` returning `Ok(())` — both arms came back as
// a `GrammarViolation` in pool `spine` ("edge kind `landed_by` is not
// declared by this pool's grammar": the projected grammar has no edge kinds)
// instead of the pen's refusal; (b) ONLY the edge-source site (`edge_deltas`)
// silenced — the same `GrammarViolation`, so this test hits its own net.
#[test]
fn an_edge_sourced_at_a_matched_ghost_refuses_before_the_row_is_appended() {
    let dir = root("ghost-source");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    let i = create_issue(&c, WS_A, "issue");
    let ghost = earlier_ghost(&c, &i);
    let before = Fingerprint::of(&c, &dir);
    for (label, text) in [
        (
            "left-to-right, ghost first",
            r#"MATCH (g: spine/txn { id: $h })
               MATCH (i: fixture-ws-a/issue { id: $i })
               CREATE (g) -[landed_by]-> (i)"#,
        ),
        (
            "right-to-left, ghost still the source",
            r#"MATCH (g: spine/txn { id: $h })
               MATCH (i: fixture-ws-a/issue { id: $i })
               CREATE (i) <-[landed_by]- (g)"#,
        ),
    ] {
        let target = expect_projected_write_refused_pre_append(
            &c,
            &dir,
            commit(text, &[("i", serde_json::json!(i.0)), ("h", serde_json::json!(ghost.0))]),
            label,
        );
        assert_eq!(target, ghost.0, "{label}: the witness names the ghost's address");
        before.assert_nothing_landed(&c, &dir, label);
    }
}

/// ADVERSARIAL PROBE: `REMOVE (i) -[landed_by]-> (g)`
/// toward a ghost that names NO row. OBSERVED, and pinned: the ghost binds
/// NOTHING (the empty match), the REMOVE expands to zero deltas, and the
/// statement COMMITS with a row whose payload carries NO part — landing
/// nothing in any pool (the empty-match rule). The real
/// `landed_by` edge the issue carries toward a REAL ghost is untouched, and
/// no edge toward the absent address exists before or after.
///
/// WHY REMOVE IS UNGUARDED (deliberate, exec phase 5's
/// `guard_ghost_edge_targets` is CREATE-only): the ghost-target guard exists
/// so that MINTING an edge toward an address no row has cannot be swallowed
/// by the empty-match rule (an edge minted toward a ghost
/// identifier that names no row is refused). A REMOVE mints nothing; and an
/// edge toward an address no row has cannot EXIST to be removed, because the
/// only way to create one is the guarded mint. So "remove nothing" is the
/// true answer, and refusing it would be refusing a no-op.
// RED (scratch copy of the crate, never the real `exec.rs`): exec's
// `guard_ghost_edge_targets` widened to guard REMOVE as well as CREATE — the
// REMOVE refused `GhostTargetUnknown` and the `expect` on the empty-match
// success fired. That is the poison that makes the pinned behaviour false.
#[test]
fn a_remove_toward_a_ghost_naming_no_row_commits_landing_nothing() {
    let dir = root("ghost-remove");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    let i = create_issue(&c, WS_A, "issue");
    let ghost = earlier_ghost(&c, &i);
    committed(&c, commit(
        r#"MATCH (i: fixture-ws-a/issue { id: $i })
           MATCH (g: spine/txn { id: $h })
           CREATE (i) -[landed_by]-> (g)"#,
        &[("i", serde_json::json!(i.0)), ("h", serde_json::json!(ghost.0))],
    ));
    let absent = "cd".repeat(32);
    let before = Fingerprint::of(&c, &dir);

    let outcome = c
        .submit(commit(
            r#"MATCH (i: fixture-ws-a/issue { id: $i })
               MATCH (g: spine/txn { id: $h })
               REMOVE (i) -[landed_by]-> (g)
               RETURN g.id"#,
            &[("i", serde_json::json!(i.0)), ("h", serde_json::json!(absent))],
        ))
        .expect("a REMOVE toward an absent ghost is the empty match: it succeeds");
    let Outcome::Committed { tx, returned, deduplicated, .. } = outcome else {
        panic!("expected Committed, got {outcome:?}");
    };
    assert!(!deduplicated);
    assert!(returned.0.is_empty(), "the ghost bound nothing, so RETURN projects nothing");
    assert!(parts_of(&dir, &tx).is_empty(), "the row's payload carries no part at all");
    // No pool moved: the real edge toward the real ghost is still there, and
    // nothing ever pointed at the absent address.
    assert_eq!(
        Fingerprint::of(&c, &dir).pools,
        before.pools,
        "a REMOVE toward an absent ghost lands nothing in any pool"
    );
    let edges = edges_from(&dir, WS_A, &i);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].dst.id, ghost);
    assert!(edges.iter().all(|e| e.dst.id.0 != absent));
}

/// ADVERSARIAL PROBE: a ghost head whose `$h` is bound to a
/// NON-STRING. OBSERVED, and pinned: exec's phase-5 ghost-target guard
/// `continue`s past a value that is not text (it cannot be a hash), and the
/// MATCHER refuses it — `Witness::MalformedStatement` from
/// `read::match_ghost` ("`id` on a projected head matches text") — at phase
/// 6, before compile and before any row. Nothing landed; no attempt exists.
// RED (scratch copy of the crate, never the real `exec.rs`): exec's guard
// made to STRINGIFY a non-text id instead of `continue`-ing — it resolved
// "42" against the spine, found no row, and refused `GhostTargetUnknown`; the
// variant assertion fired. (Both nets this probe pins live in other
// modules — exec's guard, read's matcher — so the test is the pin.)
#[test]
fn a_ghost_head_bound_to_a_non_string_refuses_as_malformed_before_the_row() {
    let dir = root("ghost-nonstring");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    let i = create_issue(&c, WS_A, "issue");
    let before = Fingerprint::of(&c, &dir);
    let env = commit(
        r#"MATCH (i: fixture-ws-a/issue { id: $i })
           MATCH (g: spine/txn { id: $h })
           CREATE (i) -[landed_by]-> (g)"#,
        &[("i", serde_json::json!(i.0)), ("h", serde_json::json!(42))],
    );
    let hash = env.hash_identity().expect("hash identity");
    let err = c.submit(env).expect_err("a number is not a transaction hash");
    match only_witness(&err) {
        Witness::MalformedStatement { .. } => {}
        other => panic!("expected MalformedStatement from the matcher, got {other:?}"),
    }
    before.assert_nothing_landed(&c, &dir, "a ghost head bound to a number");
    assert_no_attempt(&dir, &hash, "a ghost head bound to a number");
}

// ---------------------------------------------------------------------------
// the negatives — every one by name, nothing landed after any of them
// ---------------------------------------------------------------------------

/// A statement naming a pool this cauldron does not hold refuses with the
/// FROZEN tag `foreign_pool_unknown`, stamped with the pool it NAMED, at BOTH
/// doors (it used to be untagged prose, and
/// `grammar_fault_tag` answered `None`). ONE fact, ONE tag: the door's head
/// resolution produces the same tag the validator produces for a direct
/// caller's cross-pool edge (storage.rs), never a second one.
///
/// `fixture-ws-c` is a well-formed name of the fixture's own shape — a third
/// workspace that might exist in some other cauldron, just not in this one.
/// Three arms: the far head of a cross-pool edge ("a far pool the cauldron does not hold refuses by name"), a CREATE
/// into it, and a read of it. None leaves a spine row.
// RED: `resolve_grammars`' refusal reverted to the untagged
// `grammar_violation(pool, "no pool named ...")` — `expect_fault`'s tag
// assertion fired on the first arm (`None` != `Some("foreign_pool_unknown")`;
// the witness was otherwise identical, pool `fixture-ws-c`), and ladle_unit's
// `claim_footprint` arm fired the same way. All three arms here reach the
// SAME call (`claim_footprint` -> `resolve_grammars`, phase 5 at submit, the
// first act of `bulk_read` at read), so one arm going red is the net going
// red.
#[test]
fn a_statement_naming_a_pool_the_cauldron_lacks_refuses_foreign_pool_unknown_at_both_doors() {
    const MISSING: &str = "fixture-ws-c";
    let dir = root("neg-nopool");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    assert_eq!(c.grammar(&PoolName::new(MISSING)).map(|_| ()), None);
    let a = create_issue(&c, WS_A, "issue");
    let before = Fingerprint::of(&c, &dir);

    for (label, text, params) in [
        (
            "the far head of a cross-pool edge",
            r#"MATCH (a: fixture-ws-a/issue { id: $a })
               MATCH (x: fixture-ws-c/issue { id: $x })
               CREATE (a) -[blocked_by]-> (x)"#,
            vec![("a", serde_json::json!(a.0)), ("x", serde_json::json!("n0000000000000000"))],
        ),
        (
            "a CREATE into it",
            r#"CREATE (x: fixture-ws-c/issue { title: $t, state: "open" })"#,
            vec![("t", serde_json::json!("orphan"))],
        ),
    ] {
        let env = commit(text, &params);
        let hash = env.hash_identity().expect("hash identity");
        let err = c.submit(env).expect_err("this cauldron holds no `fixture-ws-c`");
        expect_fault(&err, MISSING, "foreign_pool_unknown");
        before.assert_nothing_landed(&c, &dir, label);
        assert_no_attempt(&dir, &hash, label);
    }

    // The READ door: the same head resolution, the same tag.
    let err = match c.read(&request(
        r#"MATCH (x: fixture-ws-c/issue { id: $x }) RETURN x.id"#,
        &[("x", serde_json::json!("n0000000000000000"))],
    )) {
        Err(e) => e,
        Ok(r) => panic!("the read door must refuse an unknown pool; got {:?}", r.rows),
    };
    expect_fault(&err, MISSING, "foreign_pool_unknown");
    before.assert_nothing_landed(&c, &dir, "a read of an unknown pool");
}

/// `depends_on` toward a blueprint `module`: the far kind IS declared, the
/// far TYPE is not — `reach_undeclared`, stamped with the SOURCE pool (its
/// declaration is what the edge failed against).
// RED: `validate_reach` comparing the far type against the reach set of
// EVERY kind rather than the far pool's kind — `module` still refused, but
// (with `blocked_by`'s `issue` reach in scope) the kind negative below
// stopped refusing; and dropping the actual-type check made this one land.
#[test]
fn depends_on_toward_a_blueprint_module_refuses_reach_undeclared_by_type() {
    let dir = root("neg-module");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    let m = create_milestone(&c, WS_A, "v1");
    let module = create_bp(&c, "module", "spine", "");
    let before = Fingerprint::of(&c, &dir);
    let err = c
        .submit(commit(
            r#"MATCH (m: fixture-ws-a/milestone { id: $m })
               MATCH (x: fixture-bp/module { id: $x })
               CREATE (m) -[depends_on]-> (x)"#,
            &[("m", serde_json::json!(m.0)), ("x", serde_json::json!(module.0))],
        ))
        .expect_err("`module` is deliberately unreachable from the workspace fixture");
    expect_fault(&err, WS_A, "reach_undeclared");
    before.assert_nothing_landed(&c, &dir, "depends_on toward a module");
}

/// `blocked_by` toward a blueprint migration: no reach toward kind
/// `blueprint` at all — `reach_undeclared`, by kind.
///
/// The discriminating half uses a blueprint VARIANT that declares a node
/// type named `issue`: the far TYPE then matches the reach's name exactly,
/// and only the far pool's KIND refuses. A kind-blind validator (one merged
/// vocabulary of types, which the design forbids) would land that edge.
// RED: `validate_reach`'s kind filter replaced with `|_| true` — the
// migration still refused (on type), but the blueprint `issue` LANDED and
// the second `expect_err` fired. The first arm alone did not discriminate,
// which is why the variant exists.
#[test]
fn blocked_by_toward_a_blueprint_migration_refuses_reach_undeclared_by_kind() {
    let dir = root("neg-kind");
    let mut bp = fixture_blueprint();
    bp.node_types.insert(
        "issue".to_string(),
        bp.node_types["module"].clone(), // field `name`, no states
    );
    let c = fresh_cauldron(
        &dir,
        vec![fixture_workspace(WS_A), fixture_workspace_at(WS_B, WS_B_PIN), bp],
    );
    let a = create_issue(&c, WS_A, "issue");
    let mig = create_bp(&c, "migration", "0001", "planned");
    let bp_issue = create_bp(&c, "issue", "a blueprint's own issue type", "");
    let before = Fingerprint::of(&c, &dir);

    let err = c
        .submit(commit(
            r#"MATCH (a: fixture-ws-a/issue { id: $a })
               MATCH (g: fixture-bp/migration { id: $g })
               CREATE (a) -[blocked_by]-> (g)"#,
            &[("a", serde_json::json!(a.0)), ("g", serde_json::json!(mig.0))],
        ))
        .expect_err("`blocked_by` reaches toward workspaces only");
    expect_fault(&err, WS_A, "reach_undeclared");
    before.assert_nothing_landed(&c, &dir, "blocked_by toward a migration");

    // Same type NAME, wrong KIND: this is the arm only a kind check refuses.
    let err = c
        .submit(commit(
            r#"MATCH (a: fixture-ws-a/issue { id: $a })
               MATCH (x: fixture-bp/issue { id: $x })
               CREATE (a) -[blocked_by]-> (x)"#,
            &[("a", serde_json::json!(a.0)), ("x", serde_json::json!(bp_issue.0))],
        ))
        .expect_err("a blueprint's `issue` is not a workspace's `issue`: the reach is by KIND");
    expect_fault(&err, WS_A, "reach_undeclared");
    before.assert_nothing_landed(&c, &dir, "blocked_by toward a blueprint `issue`");
}

/// The matcher's OWN net for the staged-out projection forms — the one a
/// direct caller of `read::run_match` (not the door) hits, since the coverage
/// table never runs for it. Proven separately because the door's table
/// shadows it: a poison of the matcher arm alone is invisible through the
/// door.
// RED: `run_match`'s projected-hop arm silenced (`if false && ...`) — the
// hop then ran `match_hop`, which reached for a `spine` snapshot no
// `Snapshots` holds and failed HARD instead of by name; `match_ghost`'s
// no-`id` arm silenced — the by-type head fell into `resolve_ghost` with an
// absent identifier and returned nothing instead of refusing by name.
#[test]
fn the_matcher_itself_refuses_the_projected_pattern_forms_for_a_direct_caller() {
    use spine::ladle::compile::Fuel;
    use spine::ladle::parse::parse;
    use spine::read;
    let dir = root("matcher-net");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    create_issue(&c, WS_A, "present");
    let open = read::open_pools(&c, &[PoolName::new(WS_A)]).expect("open pools");
    let snaps = read::open_snapshots(&open).expect("snapshots");
    for text in [
        r#"MATCH (i: fixture-ws-a/issue) -[landed_by]-> (g: spine/txn { id: $h }) RETURN g.id"#,
        r#"MATCH (g: spine/txn) RETURN g.id"#,
        r#"MATCH (g: spine/txn { state: "cleared" }) RETURN g.id"#,
    ] {
        let stmt = parse(text).expect("parses");
        let mut bindings = Bindings::new();
        if text.contains("$h") {
            bindings.insert("h".into(), serde_json::json!("ab".repeat(32)));
        }
        let mut fuel = Fuel::new(1_000);
        let err = read::run_match(&snaps, &stmt, &bindings, &mut fuel)
            .expect_err("the matcher refuses by name without the coverage table in front of it");
        match only_witness(&err) {
            Witness::UnimplementedForm { form } => assert_eq!(form, PROJECTED_PATTERN_FORM, "{text}"),
            other => panic!("{text}: expected UnimplementedForm, got {other:?}"),
        }
    }
    // ...and the by-identifier form resolves through the same matcher.
    let stmt = parse(r#"MATCH (g: spine/txn { id: $h }) RETURN g.id"#).expect("parses");
    let mut bindings = Bindings::new();
    bindings.insert("h".into(), serde_json::json!("ab".repeat(32)));
    let set = read::run_match(&snaps, &stmt, &bindings, &mut Fuel::new(1_000)).expect("matches");
    assert!(set.bindings["g"].is_empty(), "no row at that address: the empty match");
}

/// `blocked_by` toward a ws-b MILESTONE: the far kind is declared, the far
/// node's ACTUAL type (as ws-b resolves it) is not in the reach.
// RED: the far-type lookup returning the reach's hoped-for type instead of
// the match set's actual type — the milestone validated as an issue.
#[test]
fn blocked_by_toward_a_milestone_in_the_other_workspace_refuses_on_the_actual_type() {
    let dir = root("neg-type");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    let a = create_issue(&c, WS_A, "issue");
    let m = create_milestone(&c, WS_B, "ws-b milestone");
    let before = Fingerprint::of(&c, &dir);
    let err = c
        .submit(commit(
            r#"MATCH (a: fixture-ws-a/issue { id: $a })
               MATCH (m: fixture-ws-b/milestone { id: $m })
               CREATE (a) -[blocked_by]-> (m)"#,
            &[("a", serde_json::json!(a.0)), ("m", serde_json::json!(m.0))],
        ))
        .expect_err("a milestone is not an issue, whatever workspace it is in");
    expect_fault(&err, WS_A, "reach_undeclared");
    before.assert_nothing_landed(&c, &dir, "blocked_by toward a milestone");
}

/// A workspace-grammar VARIANT declaring a reach to `{blueprint, epic}`
/// against a blueprint that declares no `epic`: the source's declaration is
/// checked against the TARGET's own pin at the door — `foreign_type_unknown`
/// — even though the edge itself aims at a perfectly good migration. That is
/// "check the schema version on it": a declaration stale against the far pin
/// refuses loudly the first time it is used, not only when the dead type is
/// aimed at (an interior choice).
// RED: the per-reach `far_grammar.node_types.contains_key` loop removed —
// the migration edge landed and the stale `epic` reach was never noticed.
#[test]
fn a_reach_toward_a_type_the_blueprint_does_not_declare_refuses_foreign_type_unknown() {
    let dir = root("neg-epic");
    let mut ws = fixture_workspace(WS_A);
    ws.edge_kinds
        .get_mut("depends_on")
        .unwrap()
        .to
        .insert(TypeRef::reach("blueprint", "epic"));
    ws.check_wellformed()
        .expect("a reach toward an undeclared far type is not malformed on its own");
    let c = fresh_cauldron(&dir, vec![ws, fixture_blueprint()]);
    let m = create_milestone(&c, WS_A, "v1");
    let mig = create_bp(&c, "migration", "0001", "planned");
    let before = Fingerprint::of(&c, &dir);
    let err = c
        .submit(commit(
            r#"MATCH (m: fixture-ws-a/milestone { id: $m })
               MATCH (g: fixture-bp/migration { id: $g })
               CREATE (m) -[depends_on]-> (g)"#,
            &[("m", serde_json::json!(m.0)), ("g", serde_json::json!(mig.0))],
        ))
        .expect_err("the blueprint's pin declares no `epic`");
    expect_fault(&err, WS_A, "foreign_type_unknown");
    before.assert_nothing_landed(&c, &dir, "a reach toward an undeclared far type");
}

/// A reach toward a kind NO pool in the cauldron has PINS: `Cauldron::create`
/// accepts the grammar (the cauldron does not know the future, and
/// `check_wellformed` cannot judge a reach's far kind at pin time), and the
/// cauldron answers "no pools of kind `engine`". THOSE are this test's
/// discriminating assertions — the `create` succeeding and `pools_of_kind`
/// answering empty.
///
/// The refusal at the end is NOT a second mechanism, and this test does not
/// pretend it is (that would be a wrong-reason green). The edge is
/// aimed at a ws-b ISSUE — a real node in a real pool of kind `workspace` —
/// and it refuses `reach_undeclared` from the SAME arm the by-kind test
/// (`blocked_by_toward_a_blueprint_migration_refuses_reach_undeclared_by_kind`)
/// already proves: "`runs_on` declares no reach toward the far pool's kind".
/// The validator never consults `pools_of_kind`; it judges the far node's
/// ACTUAL pool's kind against the source's reaches, one edge at a time. So
/// the refusal would be identical if pools of kind `engine` existed, and
/// there is no statement that could "use" the reach toward `engine` any
/// other way: a far node can only be bound by a head in a pool this
/// cauldron holds, and none of them is of kind `engine`. There is no
/// "reach toward a kind nobody has" refusal to discriminate, because there
/// is nothing distinct for it to refuse.
// RED: the discriminating assertion is `pools_of_kind` — the poison
// "`pools_of_kind` ignores its filter (every pool answers)" fired
// `c.pools_of_kind(engine).is_empty()`. The refusal
// half rides the by-kind arm: `validate_reach`'s kind filter replaced with
// `|_| true` moved its tag to `foreign_type_unknown` — the
// same poison that reddens the by-kind test, which is exactly why it proves
// nothing new here.
#[test]
fn a_reach_toward_a_kind_no_pool_has_pins_and_then_refuses_every_use() {
    let dir = root("neg-nokind");
    let mut ws = fixture_workspace(WS_A);
    ws.edge_kinds.insert(
        "runs_on".to_string(),
        spine::schema_def::EdgeKindDef::reaching("issue", [TypeRef::reach("engine", "job")]),
    );
    let c = fresh_cauldron(
        &dir,
        vec![ws, fixture_workspace_at(WS_B, WS_B_PIN), fixture_blueprint()],
    );
    assert!(c.pools_of_kind(&PoolKind::new("engine")).is_empty());
    let a = create_issue(&c, WS_A, "issue");
    let b = create_issue(&c, WS_B, "not an engine job");
    let before = Fingerprint::of(&c, &dir);
    let err = c
        .submit(commit(
            r#"MATCH (a: fixture-ws-a/issue { id: $a })
               MATCH (b: fixture-ws-b/issue { id: $b })
               CREATE (a) -[runs_on]-> (b)"#,
            &[("a", serde_json::json!(a.0)), ("b", serde_json::json!(b.0))],
        ))
        .expect_err("no pool of kind `engine` exists to reach");
    expect_fault(&err, WS_A, "reach_undeclared");
    before.assert_nothing_landed(&c, &dir, "a reach toward a kind no pool has");
}

// ---------------------------------------------------------------------------
// the closure checker, and the far record's durability
// ---------------------------------------------------------------------------

/// `checkers::referential_closure` counts an edge into `spine` as CLOSED
/// when the ghost resolves and as DANGLING when it does not.
// RED: (a) `census()` no longer collecting `resolved_ghosts` — the landed
// ghost edge read as dangling and `green` fired; (b) the checker's projected
// arm removed — the poisoned census (ghosts cleared) passed, because the
// old code reported `EndpointPoolNotCensused` for `spine`, which is a
// DIFFERENT finding than the dangling one asserted here.
#[test]
fn referential_closure_counts_a_ghost_edge_closed_when_it_resolves_and_dangling_when_not() {
    let dir = root("closure");
    let c = fresh_cauldron(&dir, cross_pool_grammars());
    let i = create_issue(&c, WS_A, "issue");
    let (earlier, _) = committed(&c, commit(
        r#"MATCH (i: fixture-ws-a/issue { id: $i }) SET i.title = "moved""#,
        &[("i", serde_json::json!(i.0))],
    ));
    let ghost = project::ghost_id(&earlier);
    committed(&c, commit(
        r#"MATCH (i: fixture-ws-a/issue { id: $i })
           MATCH (g: spine/txn { id: $h })
           CREATE (i) -[landed_by]-> (g)"#,
        &[("i", serde_json::json!(i.0)), ("h", serde_json::json!(ghost.0))],
    ));

    let census = checkers::census(&c).expect("census");
    assert!(census.resolved_ghosts.contains(&ghost));
    let v = checkers::referential_closure(&census);
    assert!(v.passed(), "a ghost edge toward a row is closed: {:?}", v.findings);

    // POISON: the ghost no longer resolves.
    let mut poisoned = census.clone();
    poisoned.resolved_ghosts.clear();
    let v = checkers::referential_closure(&poisoned);
    assert!(!v.passed());
    assert!(
        v.findings.iter().any(|f| matches!(
            f,
            Finding::DanglingDestinationEdge { pool, kind, dst, .. }
                if pool == &PoolName::new(WS_A)
                    && kind == "landed_by"
                    && dst.pool == PoolName::new(project::PROJECTED_POOL)
                    && dst.id == ghost
        )),
        "{:?}",
        v.findings
    );
}

/// `CompiledPart::far_versions` parses with a serde default, so a payload
/// written before the field (or by a part with no cross-pool edge) still
/// reads — and round-trips when present.
// RED: removing `#[serde(default)]` — the field-less JSON failed to parse.
#[test]
fn the_far_version_record_has_a_serde_default_and_round_trips() {
    let old = serde_json::json!({
        "pool": "fixture-ws-a",
        "schema_version": 3,
        "deltas": [],
        "premises": []
    });
    let part: CompiledPart = serde_json::from_value(old).expect("a field-less payload parses");
    assert!(part.far_versions.is_empty());

    let mut with = part.clone();
    with.far_versions.insert(PoolName::new(BP), 4);
    let text = serde_json::to_string(&with).expect("serializes");
    let back: CompiledPart = serde_json::from_str(&text).expect("parses");
    assert_eq!(back, with);
}

/// The projected namespace's grammar is CODE: kind `spine`, version = the
/// envelope format version, one node type `txn` with no settable fields and
/// the lifecycle states, no edge kinds — and no real pool may claim its name
/// or its kind.
// RED: `projected_grammar` given an edge kind — the assertion fired; a real
// pool given kind `spine` — `check_wellformed` refused, as asserted.
#[test]
fn the_projected_namespace_is_a_pool_kind_with_a_code_defined_grammar() {
    let g = project::projected_grammar();
    assert_eq!(g.pool, PoolName::new(project::PROJECTED_POOL));
    assert_eq!(g.kind, PoolKind::new(project::PROJECTED_KIND));
    assert_eq!(g.version, ENVELOPE_FORMAT_VERSION);
    assert_eq!(g.node_types.keys().collect::<Vec<_>>(), vec![project::TXN_NODE_TYPE]);
    let txn = &g.node_types[project::TXN_NODE_TYPE];
    assert!(txn.fields.is_empty(), "no settable fields: the origin is code");
    assert_eq!(txn.states, project::TXN_STATES);
    assert!(g.edge_kinds.is_empty(), "a ghost is a target, never a source");
    assert!(g.pool.is_reserved() && g.kind.is_reserved());
    // A real pool may not take the kind.
    let mut impostor = fixture_workspace("fixture-ws-x");
    impostor.kind = PoolKind::new(project::PROJECTED_KIND);
    let err = impostor.check_wellformed().unwrap_err();
    expect_fault(&err, "fixture-ws-x", "malformed_grammar");
}

// ---------------------------------------------------------------------------
// the cauldron grammar: a cluster in pool `cauldron` claims a region of `ws`
// ---------------------------------------------------------------------------

const BOUNDARY: &str = "Inside: everything about releases and how they ship. Outside: what they contain.";

fn soup(name: &str, with_cauldron_pool: bool) -> (PathBuf, Cauldron) {
    let dir = root(name);
    let mut g = vec![spine::grammar_v6::workspace_v6("ws")];
    if with_cauldron_pool {
        g.push(spine::grammar_cauldron::cauldron_v1("cauldron"));
    }
    let c = fresh_cauldron(&dir, g);
    (dir, c)
}

fn region(c: &Cauldron) -> NodeId {
    let (_, rows) = committed(c, commit(r#"CREATE (r: ws/region { handle: "releases", boundary: $b }) RETURN r.id"#, &[("b", serde_json::json!(BOUNDARY))]));
    returned_id(&rows, "r.id")
}

fn cluster(c: &Cauldron) -> NodeId {
    let (_, rows) = committed(c, commit(r#"CREATE (k: cauldron/cluster { handle: "shipping", boundary: $b }) RETURN k.id"#, &[("b", serde_json::json!(BOUNDARY))]));
    returned_id(&rows, "k.id")
}

fn claim(c: &Cauldron, verb: &str, k: &NodeId, ty: &str, r: &NodeId) -> Result<Outcome, SpineError> {
    c.submit(commit(
        &format!("MATCH (k: cauldron/cluster {{ id: $k }}) MATCH (r: ws/{ty} {{ id: $r }}) {verb} (k) -[claims]-> (r)"),
        &[("k", serde_json::json!(k.0)), ("r", serde_json::json!(r.0))],
    ))
}

/// A cluster claims a region of the workspace pool across pools; the edge is
/// stored once, in the cauldron pool, and a region's claimants are read from
/// the reverse index. Removing the claim lands too.
#[test]
fn a_cluster_claims_a_workspace_region_across_pools_and_the_claim_is_removed() {
    let (dir, c) = soup("claims", true);
    let (r, k) = (region(&c), cluster(&c));
    assert!(k.0.starts_with("CL-"), "{k}");
    claim(&c, "CREATE", &k, "region", &r).expect("the cross-pool claim lands");
    let edges = edges_from(&dir, "cauldron", &k);
    assert_eq!(edges.len(), 1);
    assert_eq!((edges[0].kind.as_str(), edges[0].dst.pool.as_str(), edges[0].dst.id.0.as_str()), ("claims", "ws", r.0.as_str()));
    let p = Pool::open(&Pool::path_in(&dir, &PoolName::new("cauldron"))).unwrap();
    let back = p.snapshot().unwrap().edges_to(&spine::pool::QualifiedNodeId { pool: PoolName::new("ws"), id: r.clone() }, Some("claims")).unwrap();
    assert_eq!(back.iter().map(|e| e.src.0.clone()).collect::<Vec<_>>(), [k.0.clone()]);
    claim(&c, "REMOVE", &k, "region", &r).expect("removing the claim lands");
    assert!(edges_from(&dir, "cauldron", &k).is_empty());
}

/// A statement making a region and a cluster's claim of it lands the
/// workspace part FIRST and the claim-bearing part second, though `cauldron`
/// sorts before `ws`: the far end of a cross-pool edge is readable before
/// the edge is. The order is frozen on the row, so recovery lands it the
/// same way.
// RED: the compiler's parts back in ascending pool name (`cauldron` first).
#[test]
fn a_part_lands_after_the_pool_its_cross_pool_edges_point_into() {
    let (dir, c) = soup("claims-order", true);
    committed(
        &c,
        commit(
            r#"CREATE (r: ws/region { handle: "releases", boundary: $b })
               CREATE (k: cauldron/cluster { handle: "shipping", boundary: $b })
               CREATE (k) -[claims]-> (r)"#,
            &[("b", serde_json::json!(BOUNDARY))],
        ),
    );
    let conn = spine::offline::read_only_connection(&dir.join(spine::spine_db::SPINE_FILE)).unwrap();
    let payload: String = conn
        .query_row("SELECT payload FROM transactions WHERE decided = 1 ORDER BY seq DESC LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let p: spine::delta::CompiledPayload = serde_json::from_str(&payload).unwrap();
    let order: Vec<&str> = p.parts.iter().map(|x| x.pool.as_str()).collect();
    assert_eq!(order, ["ws", "cauldron"], "the region's pool lands before the claim's");
    // A cluster alone (no edge into `ws`) is one part, and a claim of a
    // region already held is one part too: nothing to order.
    committed(&c, commit(r#"CREATE (k: cauldron/cluster { handle: "docs", boundary: $b })"#, &[("b", serde_json::json!(BOUNDARY))]));
}

/// A claim of a workspace node that is not a region is refused by the
/// grammar, and nothing lands.
#[test]
fn a_claim_of_a_non_region_is_refused() {
    let (dir, c) = soup("claims-non-region", true);
    let k = cluster(&c);
    let (_, rows) = committed(&c, commit(
        r#"MATCH (r: ws/region { id: $r }) CREATE (q: ws/question { handle: "which?", state: "open" }) CREATE (q) -[in_region]-> (r) RETURN q.id"#,
        &[("r", serde_json::json!(region(&c).0))],
    ));
    let q = returned_id(&rows, "q.id");
    let err = claim(&c, "CREATE", &k, "question", &q).expect_err("a question cannot be claimed");
    assert!(matches!(err, SpineError::Refused { .. }), "{err:?}");
    assert!(edges_from(&dir, "cauldron", &k).is_empty());
}

/// A cluster carries no state, and its boundary keeps the region's rule.
#[test]
fn a_cluster_takes_no_state_and_a_short_boundary_is_refused() {
    let (_dir, c) = soup("cluster-shape", true);
    for stmt in [
        r#"CREATE (k: cauldron/cluster { handle: "h", boundary: $b, state: "open" })"#,
        r#"CREATE (k: cauldron/cluster { handle: "h", boundary: "too short" })"#,
        r#"CREATE (k: cauldron/cluster { boundary: $b })"#,
    ] {
        let err = c.submit(commit(stmt, &[("b", serde_json::json!(BOUNDARY))])).expect_err(stmt);
        assert!(matches!(err, SpineError::Refused { .. }), "{stmt}: {err:?}");
    }
}

/// An older cauldron (workspace pool only) gets the cauldron pool by one
/// additive act: nothing else in the store moves, the handle can then write
/// a cluster and its claim, and a fresh open discovers the pool. Adding it
/// again is a no-op; a different grammar under the same name is refused.
#[test]
fn add_pool_adds_the_cauldron_pool_to_an_older_store_and_changes_nothing_else() {
    let (dir, mut c) = soup("add-pool", false);
    let r = region(&c);
    let counter = c.committed_commit_counter().unwrap();
    let ws_bytes = std::fs::read(dir.join("ws.db")).unwrap();
    let g = spine::grammar_cauldron::cauldron_v1("cauldron");
    assert!(c.add_pool(&g).expect("the pool is added"));
    assert_eq!(c.committed_commit_counter().unwrap(), counter, "adding a pool writes no spine row");
    assert_eq!(std::fs::read(dir.join("ws.db")).unwrap(), ws_bytes, "the workspace pool is not touched");
    assert!(!dir.join(".adding-cauldron").exists(), "the staging is gone");
    assert!(!dir.join("cauldron.db-wal").exists(), "the pool was moved in whole, with no log beside it");
    assert!(!c.add_pool(&g).unwrap(), "adding it again is a no-op");
    let mut other = g.clone();
    other.version = 2;
    assert!(c.add_pool(&other).is_err(), "a different pin under the same name is refused");
    let k = cluster(&c);
    claim(&c, "CREATE", &k, "region", &r).expect("the claim lands through the same handle");
    drop(c);
    let reopened = Cauldron::open(&dir, SpineConfig::default()).expect("the store opens");
    assert_eq!(reopened.pools().map(|p| p.as_str().to_string()).collect::<Vec<_>>(), ["cauldron", "ws"]);
    assert_eq!(reopened.committed_commit_counter().unwrap(), counter + 2, "one counter across both pools");
}

/// `add_pool` adds the cluster pool and nothing else: another grammar, or
/// `cauldron@1` under another name, is refused, whatever the cauldron holds.
// RED: the grammar check removed from `add_pool`.
#[test]
fn add_pool_adds_only_the_cluster_pool() {
    let (_dir, mut c) = soup("add-pool-only", false);
    let err = c.add_pool(&spine::grammar_v6::workspace_v6("ws2")).expect_err("a second workspace pool is not added");
    assert!(format!("{err}").contains("only as `cauldron`"), "{err}");
    assert!(c.add_pool(&spine::grammar_cauldron::cauldron_v1("other")).is_err(), "nor the cluster grammar under another name");
    assert!(c.add_pool(&spine::grammar_cauldron::cauldron_v1("cauldron")).unwrap());
}

/// An add that died after its rename and before removing its staging
/// leaves a second name for the live pool file; the next add (which finds
/// the pool in place) removes it.
// RED: `clear_staging` removed from the already-pinned path.
#[test]
fn a_staging_left_after_the_rename_is_removed_by_the_next_add() {
    let (dir, mut c) = soup("add-pool-stale-link", false);
    let g = spine::grammar_cauldron::cauldron_v1("cauldron");
    assert!(c.add_pool(&g).unwrap());
    let staging = dir.join(".adding-cauldron");
    std::fs::create_dir(&staging).unwrap();
    std::fs::hard_link(dir.join("cauldron.db"), staging.join("cauldron.db")).unwrap();
    assert!(!c.add_pool(&g).unwrap(), "already there");
    assert!(!staging.exists(), "the stale staging is gone");
    drop(c);
    let mut c = Cauldron::open(&dir, SpineConfig::default()).unwrap();
    std::fs::create_dir(&staging).unwrap();
    std::fs::hard_link(dir.join("cauldron.db"), staging.join("cauldron.db")).unwrap();
    assert!(!c.add_pool(&g).unwrap());
    assert!(!staging.exists(), "removed by an add through a fresh handle too");
}

/// An add that died after staging the pool and before moving it in leaves
/// only a directory no open reads: the store opens with its old pools, and
/// the next add removes the staging and adds the pool.
#[test]
fn an_add_cut_short_before_the_move_leaves_the_store_as_it_was_and_the_next_add_finishes() {
    let (dir, c) = soup("add-pool-crash", false);
    let g = spine::grammar_cauldron::cauldron_v1("cauldron");
    let staging = c.stage_pool(&g).expect("staged");
    assert!(staging.join("cauldron.db").is_file());
    drop(c);
    let mut c = Cauldron::open(&dir, SpineConfig::default()).expect("a store with a staging directory opens");
    assert_eq!(c.pools().map(|p| p.as_str().to_string()).collect::<Vec<_>>(), ["ws"]);
    assert!(spine::offline::rest_check(&dir).unwrap().is_empty(), "and is at rest");
    let mut copy = root("add-pool-crash-copy");
    let _ = std::fs::remove_dir_all(&copy);
    std::fs::create_dir_all(&copy).unwrap();
    spine::offline::snapshot_copy(&dir, &copy).expect("a snapshot copy skips the staging");
    assert!(!copy.join(".adding-cauldron").exists());
    copy.push("cauldron.db");
    assert!(!copy.exists());
    assert!(c.add_pool(&g).expect("the next add finishes"));
    assert!(!staging.exists());
    region(&c);
    cluster(&c);
}

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

fn root(name: &str) -> PathBuf {
    test_scratch::tmp_path(format!("spine-cross-pool-{name}-{}", std::process::id()))
}

fn fresh_cauldron(dir: &Path, grammars: Vec<PoolGrammar>) -> Cauldron {
    let _ = std::fs::remove_dir_all(dir);
    Cauldron::create(dir, &grammars, SpineConfig::default()).expect("create the cauldron")
}

fn commit(statement: &str, params: &[(&str, serde_json::Value)]) -> Envelope {
    let mut bindings = Bindings::new();
    for (k, v) in params {
        bindings.insert((*k).to_string(), v.clone());
    }
    Envelope {
        statement: statement.to_string(),
        bindings,
        format_version: ENVELOPE_FORMAT_VERSION,
        provenance: Provenance {
            initiator: Initiator::Agent {
                thread: "cross-pool".into(),
                dispatch: "cross-pool".into(),
            },
            instrument: Instrument::door(),
        },
        mode: Mode::Commit,
        idempotency_nonce: None,
    }
}

fn request(statement: &str, params: &[(&str, serde_json::Value)]) -> ReadRequest {
    let mut bindings = Bindings::new();
    for (k, v) in params {
        bindings.insert((*k).to_string(), v.clone());
    }
    ReadRequest {
        statement: statement.to_string(),
        bindings,
        format_version: ENVELOPE_FORMAT_VERSION,
    }
}

type Row = BTreeMap<String, serde_json::Value>;

fn committed(c: &Cauldron, env: Envelope) -> (spine::envelope::TxHash, Vec<Row>) {
    match c.submit(env).expect("the statement commits") {
        Outcome::Committed { tx, returned, .. } => (tx, returned.0),
        other => panic!("expected Committed, got {other:?}"),
    }
}

fn read(c: &Cauldron, statement: &str, params: &[(&str, serde_json::Value)]) -> Vec<Row> {
    c.read(&request(statement, params)).expect("the read completes").rows.0
}

fn returned_id(rows: &[Row], column: &str) -> NodeId {
    NodeId(rows[0][column].as_str().expect("an identifier").to_string())
}

fn create_issue(c: &Cauldron, pool: &str, title: &str) -> NodeId {
    let (_, rows) = committed(c, commit(
        &format!(r#"CREATE (i: {pool}/issue {{ title: $t, state: "open" }}) RETURN i.id"#),
        &[("t", serde_json::json!(title))],
    ));
    returned_id(&rows, "i.id")
}

fn create_milestone(c: &Cauldron, pool: &str, title: &str) -> NodeId {
    let (_, rows) = committed(c, commit(
        &format!(r#"CREATE (m: {pool}/milestone {{ title: $t, state: "active" }}) RETURN m.id"#),
        &[("t", serde_json::json!(title))],
    ));
    returned_id(&rows, "m.id")
}

fn create_bp(c: &Cauldron, node_type: &str, name: &str, state: &str) -> NodeId {
    let text = if state.is_empty() {
        format!(r#"CREATE (n: {BP}/{node_type} {{ name: $n }}) RETURN n.id"#)
    } else {
        format!(r#"CREATE (n: {BP}/{node_type} {{ name: $n, state: "{state}" }}) RETURN n.id"#)
    };
    let (_, rows) = committed(c, commit(&text, &[("n", serde_json::json!(name))]));
    returned_id(&rows, "n.id")
}

fn parts_of(dir: &Path, tx: &spine::envelope::TxHash) -> Vec<CompiledPart> {
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    spine
        .find_by_hash(tx)
        .expect("read the row")
        .expect("the row exists")
        .payload
        .parts
}

fn edges_from(dir: &Path, pool: &str, src: &NodeId) -> Vec<spine::pool::EdgeRecord> {
    let p = Pool::open(&Pool::path_in(dir, &PoolName::new(pool))).expect("pool");
    let s = p.snapshot().expect("snapshot");
    s.edges_from(src, None).expect("edges")
}

fn only_witness(err: &SpineError) -> Witness {
    only_witness_of(err)
}

/// A real ghost: the `spine/<hash>` of an earlier commit (a title SET on
/// `issue`), cleared by the time this returns.
fn earlier_ghost(c: &Cauldron, issue: &NodeId) -> NodeId {
    let (tx, _) = committed(c, commit(
        r#"MATCH (i: fixture-ws-a/issue { id: $i }) SET i.title = $t"#,
        &[("i", serde_json::json!(issue.0)), ("t", serde_json::json!("the earlier commit"))],
    ));
    project::ghost_id(&tx)
}

/// The refusal came BEFORE the row append (phase 8): the spine holds NO
/// attempt at all for this identity — not a decided row, not an aborted one
/// (an abort is what a refusal after the append leaves behind).
/// Discriminating, forced: with the SET site silenced and this
/// check run ahead of the witness check, it fired "the spine holds 1
/// attempt(s)" — the aborted attempt the phase-10b failure leaves.
#[track_caller]
fn assert_no_attempt(dir: &Path, hash: &spine::envelope::TxHash, what: &str) {
    let spine = SpineDb::open(&dir.join(SPINE_FILE)).expect("spine");
    let attempts = spine.attempts_of(hash).expect("attempts");
    assert!(
        attempts.is_empty(),
        "{what}: the refusal must precede the row append, but the spine holds {} attempt(s) for this identity",
        attempts.len()
    );
}

/// Submit `env`, expect the pen's refusal — `ProjectedNamespaceWrite` stamped
/// with the projected pool — and prove it came before the row append.
/// Returns the witness's `target` for the caller to assert on.
#[track_caller]
fn expect_projected_write_refused_pre_append(
    c: &Cauldron,
    dir: &Path,
    env: Envelope,
    what: &str,
) -> String {
    let hash = env.hash_identity().expect("hash identity");
    let err = c.submit(env).expect_err(what);
    let target = match only_witness(&err) {
        Witness::ProjectedNamespaceWrite { pool, target } => {
            assert_eq!(pool, PoolName::new(project::PROJECTED_POOL), "{what}");
            target
        }
        other => panic!("{what}: expected ProjectedNamespaceWrite, got {other:?}"),
    };
    assert_no_attempt(dir, &hash, what);
    target
}

fn only_witness_of(err: &SpineError) -> Witness {
    match err {
        SpineError::Refused { witnesses } => {
            assert_eq!(witnesses.len(), 1, "one refusal, one witness: {witnesses:?}");
            witnesses[0].clone()
        }
        other => panic!("expected a structured refusal, got {other:?}"),
    }
}

/// Assert on the STRUCTURE of a grammar refusal — the pool it names and the
/// frozen fault tag — never on the prose.
#[track_caller]
fn expect_fault(err: &SpineError, pool: &str, tag: &str) {
    match only_witness(err) {
        w @ Witness::GrammarViolation { .. } => {
            let Witness::GrammarViolation { pool: p, .. } = &w else {
                unreachable!()
            };
            assert_eq!(p, &PoolName::new(pool), "the pool whose grammar refused: {w:?}");
            assert_eq!(grammar_fault_tag(&w), Some(tag), "the frozen tag: {w:?}");
        }
        other => panic!("expected a GrammarViolation, got {other:?}"),
    }
}

/// Everything a landing could move, in every pool, as one comparable value
/// (the same shape `refusal_witnesses.rs` uses): every node of every
/// declared type with its state, revision and fields; every edge; the
/// committed-commit counter; the in-landing set.
#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    counter: u64,
    landing: usize,
    pools: Vec<String>,
}

impl Fingerprint {
    fn of(c: &Cauldron, dir: &Path) -> Fingerprint {
        let mut pools = Vec::new();
        for name in c.pools() {
            let grammar = c.grammar(name).expect("grammar");
            let p = Pool::open(&Pool::path_in(dir, name)).expect("pool");
            let s = p.snapshot().expect("snapshot");
            let mut lines = Vec::new();
            for t in grammar.node_types.keys() {
                for n in s.nodes_of(t, None).expect("nodes") {
                    lines.push(format!("{name} node {} {t} {:?} r{} {:?}", n.id, n.state, n.rev, n.fields));
                    for e in s.edges_from(&n.id, None).expect("edges") {
                        lines.push(format!("{name} edge {} -[{}]-> {}", e.src, e.kind, e.dst));
                    }
                }
            }
            pools.push(lines.join("\n"));
        }
        Fingerprint {
            counter: c.committed_commit_counter().expect("counter"),
            landing: c.in_landing(None).expect("in landing").len(),
            pools,
        }
    }

    #[track_caller]
    fn assert_nothing_landed(&self, c: &Cauldron, dir: &Path, what: &str) {
        let now = Fingerprint::of(c, dir);
        assert_eq!(self, &now, "{what}: something landed in some pool");
    }
}

/// **A user grammar joins a cauldron by the same additive act**: the pool
/// file is built whole and linked in, no spine row and no other pool moves,
/// a fresh open discovers it, and its records cross to the workspace by
/// reach. A grammar the user check refuses (no hyphen, or a type name the
/// workspace uses) is refused by `add_pool` itself, not only by the command
/// that judges the file first.
// RED: the user check removed from `add_pool` — the bad grammars below land.
#[test]
fn add_pool_takes_a_user_grammar_the_check_passes_and_no_other() {
    let (dir, mut c) = soup("add-user-pool", true);
    let r = region(&c);
    let counter = c.committed_commit_counter().unwrap();
    let ws_bytes = std::fs::read(dir.join("ws.db")).unwrap();
    let file = include_str!("../../../schema/soup/examples/reading-list.grammar.json");
    let g = spine::user_grammar::parse_file(file.as_bytes(), None).unwrap().grammar;
    assert!(c.add_pool(&g).expect("the user pool is added"));
    assert_eq!(c.committed_commit_counter().unwrap(), counter, "adding a pool writes no spine row");
    assert_eq!(std::fs::read(dir.join("ws.db")).unwrap(), ws_bytes, "the workspace pool is not touched");
    assert!(!c.add_pool(&g).unwrap(), "adding it again is a no-op");
    let mut bad = g.clone();
    bad.kind = PoolKind::new("readinglist");
    bad.pool = PoolName::new("readinglist");
    let err = c.add_pool(&bad).expect_err("a kind with no hyphen is not a user kind, and not cauldron@1");
    assert!(format!("{err}").contains("only as `cauldron`"), "{err}");
    let mut clash = g.clone();
    clash.kind = PoolKind::new("other-list");
    clash.pool = PoolName::new("other-list");
    let err = c.add_pool(&clash).expect_err("types and prefixes already used");
    assert!(format!("{err}").contains("type_name_taken") || format!("{err}").contains("kind_registered"), "{err}");
    // A book reaching the workspace region lands in the user pool's part.
    let (_, rows) = committed(
        &c,
        commit(
            r#"MATCH (r: ws/region { id: $r }) CREATE (b: reading-list/book { handle: "A book", state: "reading" }) CREATE (b) -[about]-> (r) RETURN b.id"#,
            &[("r", serde_json::json!(r.0))],
        ),
    );
    let b = returned_id(&rows, "b.id");
    assert!(b.0.starts_with("BK-"), "{b:?}");
    drop(c);
    let reopened = Cauldron::open(&dir, SpineConfig::default()).expect("the store opens");
    assert_eq!(reopened.pools().map(|p| p.as_str().to_string()).collect::<Vec<_>>(), ["cauldron", "reading-list", "ws"]);
    assert_eq!(reopened.grammar(&PoolName::new("reading-list")), Some(&g), "pinned exactly as given");
}

/// A pool file's bytes with its log folded in, and no side files left.
fn settle(file: &Path) -> Vec<u8> {
    let conn = rusqlite::Connection::open(file).unwrap();
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())).unwrap();
    drop(conn);
    for side in ["-wal", "-shm"] {
        let mut p = file.as_os_str().to_owned();
        p.push(side);
        let _ = std::fs::remove_file(PathBuf::from(p));
    }
    std::fs::read(file).unwrap()
}

/// `file`'s pinned grammar, changed by `edit` (a hand edit of the pin).
fn edit_pin(file: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let conn = rusqlite::Connection::open(file).unwrap();
    let text: String = conn.query_row("SELECT grammar FROM pool_meta", [], |r| r.get(0)).unwrap();
    let mut g: serde_json::Value = serde_json::from_str(&text).unwrap();
    edit(&mut g);
    conn.execute("UPDATE pool_meta SET grammar = ?1", [g.to_string()]).unwrap();
    drop(conn);
    settle(file);
}

/// **A user pool file is judged at every open against the spine's registry,
/// and bound to its spine** (issue #28, inspections F4, F7, F8, N4): the
/// pin is the registered grammar or the file is refused, whatever was
/// edited (a built-in type, a built-in prefix, a built-in kind, a kind
/// with no hyphen such as `dumps`, a dropped state, an added field); a pool
/// file from another cauldron, and an older copy of this one's, are refused
/// as not this soup's own, each with its way out. A pool whose file is out
/// does not block the soup, and its name, kind and prefix stay taken. The
/// file put back, the soup opens as before.
// RED: (a) `user_grammar::refusal` judging only hyphenated kinds (the
// U2 build) — `kind=dumps` and `drop-states` open; (b) `pool_has_history`
// not asked by `add_pool` — the removed pool is made again.
#[test]
fn a_user_pool_file_edited_or_swapped_in_is_refused_at_open_and_a_removed_one_is_not_made_again() {
    let file_text = include_str!("../../../schema/soup/examples/reading-list.grammar.json");
    let g = spine::user_grammar::parse_file(file_text.as_bytes(), None).unwrap().grammar;
    let book = r#"CREATE (b: reading-list/book { handle: "A book", state: "reading" }) RETURN b.id"#;
    let (dir, mut c) = soup("user-pool-bound", true);
    assert!(c.add_pool(&g).unwrap());
    drop(c);
    let pool = dir.join("reading-list.db");
    let empty = settle(&pool);
    let c = Cauldron::open(&dir, SpineConfig::default()).unwrap();
    committed(&c, commit(book, &[]));
    drop(c);
    let good = settle(&pool);
    let refused = |why: &str| {
        let err = Cauldron::open(&dir, SpineConfig::default()).err().unwrap_or_else(|| panic!("{why}: the soup opened"));
        format!("{err}")
    };
    let edits: [(&str, fn(&mut serde_json::Value)); 6] = [
        ("hijack decision", |g| {
            g["node_types"]["decision"] = g["node_types"]["author"].clone();
            g["node_types"]["decision"]["id_prefix"] = "DEC".into();
        }),
        ("kind workspace", |g| g["kind"] = "workspace".into()),
        ("kind dumps", |g| g["kind"] = "dumps".into()),
        ("prefix R", |g| g["node_types"]["author"]["id_prefix"] = "R".into()),
        ("drop states", |g| g["node_types"]["book"]["states"] = serde_json::json!([])),
        ("add a field", |g| g["node_types"]["book"]["fields"]["approved"] = serde_json::json!({"required": false, "kind": "any"})),
    ];
    for (what, edit) in edits {
        edit_pin(&pool, edit);
        let text = refused(what);
        assert!(text.contains("is not the one this soup recorded when pool `reading-list` was added") && text.contains("Put back the file this cauldron wrote"), "{what}: {text}");
        std::fs::write(&pool, &good).unwrap();
    }
    Cauldron::open(&dir, SpineConfig::default()).expect("put back, the soup opens");
    // Another cauldron's pool file, and an older copy of this one's.
    let (other, mut o) = soup("user-pool-bound-other", true);
    assert!(o.add_pool(&g).unwrap());
    // (Its own write: two cauldrons' transactions differ, by what they say
    // and by the nonce every door gives a write.)
    committed(&o, commit(r#"CREATE (b: reading-list/book { handle: "Another cauldron's book", state: "read" }) RETURN b.id"#, &[]));
    drop(o);
    std::fs::write(&pool, settle(&other.join("reading-list.db"))).unwrap();
    let text = refused("another cauldron's");
    assert!(text.contains("is not this soup's own: it holds 1 write(s) this soup's spine never made") && text.contains("restore the whole soup directory"), "{text}");
    std::fs::write(&pool, &empty).unwrap();
    let text = refused("an older copy");
    assert!(text.contains("records 1 write(s) to pool `reading-list` that the file does not hold") && text.contains("grammar rebind reading-list"), "{text}");
    std::fs::write(&pool, &good).unwrap();
    // Moved out: the rest of the soup opens; the name, the kind and the
    // prefix stay taken, so no id is minted twice.
    std::fs::create_dir_all(dir.join("aside")).unwrap();
    let aside = dir.join("aside").join("reading-list.db");
    std::fs::rename(&pool, &aside).unwrap();
    let mut c = Cauldron::open(&dir, SpineConfig::default()).expect("without the pool file, the rest of the soup opens");
    assert_eq!(c.missing_user_pools().unwrap(), vec![g.clone()]);
    let err = c.add_pool(&g).expect_err("a pool name with a history is not made again empty");
    assert!(format!("{err}").contains(spine::user_grammar::POOL_HAS_HISTORY), "{err}");
    let mut renamed = g.clone();
    renamed.pool = PoolName::new("reading-list-2");
    let err = format!("{}", c.add_pool(&renamed).expect_err("the kind stays registered"));
    assert!(err.contains("kind_registered") || err.contains("prefix"), "{err}");
    let mut other_kind = g.clone();
    other_kind.kind = PoolKind::new("book-list");
    other_kind.pool = PoolName::new("book-list");
    // Its own type names and edges: only the prefixes `BK` and `AU` clash.
    other_kind.node_types = g.node_types.iter().map(|(t, d)| (format!("{t}2"), d.clone())).collect();
    other_kind.edge_kinds = BTreeMap::new();
    let err = format!("{}", c.add_pool(&other_kind).expect_err("the prefix BK stays taken"));
    assert!(err.contains("prefix_taken") || err.contains(spine::user_grammar::PREFIX_HAS_HISTORY), "{err}");
    assert!(!pool.exists() && !dir.join("reading-list-2.db").exists() && !dir.join("book-list.db").exists(), "nothing was made");
    drop(c);
    std::fs::rename(&aside, &pool).unwrap();
    let c = Cauldron::open(&dir, SpineConfig::default()).expect("put back, the soup opens");
    assert_eq!(c.grammar(&PoolName::new("reading-list")), Some(&g));
}

/// **An older copy of a user pool file is taken by `rebind`** (inspection
/// N1): judged first (the writes it lacks, the ids they made), then, with
/// `apply`, recorded as lost; the soup opens again, the lost id is never
/// minted again, and the ids already minted go on from where they were. A
/// copy with another cauldron's write, or an edited pin, is never taken.
/// **A pool with no record is removed** (F7), its file moved aside, its
/// name free again; one with records, or one another pool reaches, is not.
// RED: `rebind` not recording the forgiven parts — the soup stays refused.
#[test]
fn an_older_copy_is_rebound_and_an_empty_pool_is_removed() {
    let file_text = include_str!("../../../schema/soup/examples/reading-list.grammar.json");
    let g = spine::user_grammar::parse_file(file_text.as_bytes(), None).unwrap().grammar;
    let book = |h: &str| format!(r#"CREATE (b: reading-list/book {{ handle: "{h}", state: "reading" }}) RETURN b.id"#);
    let (dir, mut c) = soup("user-pool-rebind", true);
    assert!(c.add_pool(&g).unwrap());
    committed(&c, commit(&book("one"), &[]));
    drop(c);
    let pool = dir.join("reading-list.db");
    let backup = settle(&pool);
    let c = Cauldron::open(&dir, SpineConfig::default()).unwrap();
    let (_, rows) = committed(&c, commit(&book("two"), &[]));
    assert_eq!(returned_id(&rows, "b.id").0, "BK-2");
    drop(c);
    std::fs::write(&pool, &backup).unwrap();
    assert!(Cauldron::open(&dir, SpineConfig::default()).is_err(), "the older copy is refused at open");
    let judged = spine::user_registry::rebind(&dir, "reading-list", false, "test").unwrap();
    assert_eq!((judged["outcome"].clone(), judged["lost_ids"].clone()), (serde_json::json!("would-rebind"), serde_json::json!(["BK-2"])), "{judged}");
    assert!(Cauldron::open(&dir, SpineConfig::default()).is_err(), "judging wrote nothing");
    let done = spine::user_registry::rebind(&dir, "reading-list", true, "test").unwrap();
    assert_eq!(done["outcome"], "rebound", "{done}");
    let c = Cauldron::open(&dir, SpineConfig::default()).expect("rebound, the soup opens");
    let (_, rows) = committed(&c, commit(&book("three"), &[]));
    assert_eq!(returned_id(&rows, "b.id").0, "BK-3", "BK-2 is never minted again");
    drop(c);
    // An edited pin is never taken.
    edit_pin(&pool, |g| g["node_types"]["book"]["states"] = serde_json::json!([]));
    let err = format!("{}", spine::user_registry::rebind(&dir, "reading-list", true, "test").unwrap_err());
    assert!(err.contains("a pin that is not the grammar this soup registered"), "{err}");
    edit_pin(&pool, |g| g["node_types"]["book"]["states"] = serde_json::json!(["to-read", "reading", "read"]));
    let mut c = Cauldron::open(&dir, SpineConfig::default()).expect("the pin as it was");
    // Remove: refused with records; an empty pool goes, and comes back empty.
    let err = format!("{}", c.remove_user_pool(&PoolName::new("reading-list"), true).unwrap_err());
    assert!(err.contains("holds records"), "{err}");
    let mut films: PoolGrammar = g.clone();
    films.kind = PoolKind::new("film-list");
    films.pool = PoolName::new("film-list");
    films.node_types = BTreeMap::from([("film".to_string(), {
        let mut d = g.node_types["author"].clone();
        d.id_prefix = Some("FM".into());
        d
    })]);
    films.edge_kinds = BTreeMap::new();
    assert!(c.add_pool(&films).unwrap());
    let judged = c.remove_user_pool(&PoolName::new("film-list"), false).unwrap();
    assert_eq!(judged["outcome"], "would-remove");
    assert!(dir.join("film-list.db").exists(), "judging moved nothing");
    let done = c.remove_user_pool(&PoolName::new("film-list"), true).unwrap();
    assert_eq!(done["outcome"], "removed", "{done}");
    assert!(!dir.join("film-list.db").exists() && dir.join(".removed/film-list.1/film-list.db").exists(), "moved, never deleted");
    drop(c);
    let mut c = Cauldron::open(&dir, SpineConfig::default()).expect("the soup opens without it");
    assert!(c.add_pool(&films).unwrap(), "the name is free again: it minted nothing");
}

/// **`rebind` never takes another cauldron's records** (U3i L1), even from
/// a file whose part log was emptied by hand; and **a pool `rebind` left
/// with no record can be removed and added again** (U3i L2), with no id
/// minted twice.
// RED: (a) `rebind` judging the part log only — the emptied foreign file is
// rebound and the next mint is `duplicate_node_id`; (b) `pool_has_history`
// counting forgiven parts — the re-add is refused.
#[test]
fn rebind_refuses_records_the_spine_never_created_and_a_fully_lost_pool_comes_back_empty() {
    let file_text = include_str!("../../../schema/soup/examples/reading-list.grammar.json");
    let g = spine::user_grammar::parse_file(file_text.as_bytes(), None).unwrap().grammar;
    let book = |h: &str| format!(r#"CREATE (b: reading-list/book {{ handle: "{h}", state: "reading" }}) RETURN b.id"#);
    let (dir, mut c) = soup("rebind-strangers", true);
    assert!(c.add_pool(&g).unwrap());
    committed(&c, commit(&book("ours"), &[]));
    drop(c);
    let pool = dir.join("reading-list.db");
    let ours = settle(&pool);
    let (other, mut o) = soup("rebind-strangers-other", true);
    assert!(o.add_pool(&g).unwrap());
    committed(&o, commit(&book("theirs one"), &[]));
    committed(&o, commit(&book("theirs two"), &[]));
    drop(o);
    let theirs = other.join("reading-list.db");
    settle(&theirs);
    let conn = rusqlite::Connection::open(&theirs).unwrap();
    conn.execute("DELETE FROM applied_parts", []).unwrap();
    drop(conn);
    std::fs::write(&pool, settle(&theirs)).unwrap();
    let err = format!("{}", spine::user_registry::rebind(&dir, "reading-list", true, "test").unwrap_err());
    // (Its `BK-1` is caught first, by its creating write; its `BK-2` is one
    // this spine never created at all.)
    assert!(err.contains("another cauldron's file") && (err.contains("`BK-1`") || err.contains("`BK-2`")), "{err}");
    std::fs::write(&pool, &ours).unwrap();
    // A pool whose every record was lost by rebind: removed, then added again.
    let mut c = Cauldron::open(&dir, SpineConfig::default()).unwrap();
    let mut games = g.clone();
    games.kind = PoolKind::new("game-list");
    games.pool = PoolName::new("game-list");
    games.node_types = BTreeMap::from([("game".to_string(), {
        let mut d = g.node_types["author"].clone();
        d.id_prefix = Some("GM".into());
        d
    })]);
    games.edge_kinds = BTreeMap::new();
    assert!(c.add_pool(&games).unwrap());
    drop(c);
    let gfile = dir.join("game-list.db");
    let empty = settle(&gfile);
    let c = Cauldron::open(&dir, SpineConfig::default()).unwrap();
    committed(&c, commit(r#"CREATE (x: game-list/game { handle: "GM one" }) RETURN x.id"#, &[]));
    drop(c);
    settle(&gfile);
    std::fs::write(&gfile, &empty).unwrap();
    assert_eq!(spine::user_registry::rebind(&dir, "game-list", true, "test").unwrap()["lost_ids"], serde_json::json!(["GM-1"]));
    let mut c = Cauldron::open(&dir, SpineConfig::default()).unwrap();
    assert_eq!(c.remove_user_pool(&PoolName::new("game-list"), true).unwrap()["outcome"], "removed");
    assert!(c.add_pool(&games).expect("its writes were all accepted as lost: the name comes back, empty"));
    let (_, rows) = committed(&c, commit(r#"CREATE (x: game-list/game { handle: "GM again" }) RETURN x.id"#, &[]));
    assert_eq!(returned_id(&rows, "x.id").0, "GM-2", "GM-1 is never minted again");
}

/// **A registry row in an earlier digest form is migrated, never refused**
/// (LG1 D1, U3i D2): a soup whose user pool was registered by a development
/// build that hashed the grammar structs directly (form 0), with no
/// `digest_input` column, opens; the open for writing rewrites the row to
/// this release's form. A row from a NEWER release's form is refused saying
/// so, and a pin that really differs is refused naming what differs (not
/// "edited by hand" for a file nobody touched).
// RED: `user_grammar::refusal` comparing `digest()` alone — the form-0 row
// is refused "edited by hand", as one development build refused an earlier one's soups.
#[test]
fn a_registry_row_in_an_earlier_digest_form_is_migrated_and_a_newer_one_is_named() {
    let file_text = include_str!("../../../schema/soup/examples/reading-list.grammar.json");
    let g = spine::user_grammar::parse_file(file_text.as_bytes(), None).unwrap().grammar;
    let (dir, mut c) = soup("digest-forms", true);
    assert!(c.add_pool(&g).unwrap());
    committed(&c, commit(r#"CREATE (b: reading-list/book { handle: "A book", state: "reading" }) RETURN b.id"#, &[]));
    drop(c);
    let spine_file = dir.join(SPINE_FILE);
    // The registry as a development build left it: form-0 digest, no column.
    let conn = rusqlite::Connection::open(&spine_file).unwrap();
    conn.execute_batch("ALTER TABLE user_pools DROP COLUMN digest_input").unwrap();
    conn.execute("UPDATE user_pools SET digest = ?1", [spine::user_grammar::digest_v0(&g)]).unwrap();
    drop(conn);
    assert_ne!(spine::user_grammar::digest_v0(&g), spine::user_grammar::digest(&g), "the two forms differ, so the test can fail");
    let c = Cauldron::open(&dir, SpineConfig::default()).expect("an earlier digest form opens");
    drop(c);
    let conn = rusqlite::Connection::open(&spine_file).unwrap();
    let (digest, form): (String, i64) = conn.query_row("SELECT digest, digest_input FROM user_pools", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!((digest.as_str(), form), (spine::user_grammar::digest(&g).as_str(), 1), "migrated to this release's form");
    // A newer release's form: named, not called a hand edit.
    conn.execute("UPDATE user_pools SET digest_input = 99, digest = 'sha256:future'", []).unwrap();
    drop(conn);
    let err = format!("{}", Cauldron::open(&dir, SpineConfig::default()).err().expect("a newer form is not judged"));
    assert!(err.contains("written by a newer release (digest form 99") && !err.contains("by hand"), "{err}");
    let conn = rusqlite::Connection::open(&spine_file).unwrap();
    conn.execute("UPDATE user_pools SET digest_input = 1, digest = ?1", [spine::user_grammar::digest(&g)]).unwrap();
    drop(conn);
    // A pin that differs: what differs is named.
    let pool = dir.join("reading-list.db");
    settle(&pool);
    edit_pin(&pool, |g| g["node_types"]["book"]["states"] = serde_json::json!([]));
    let err = format!("{}", Cauldron::open(&dir, SpineConfig::default()).err().unwrap());
    assert!(err.contains("type `book` differs"), "{err}");
}

/// **`rebind` compares each record with the write that created it here**
/// (U4i L1): another cauldron's file whose every id this spine also created
/// (its `BK-1` beside ours), part log emptied by hand, is refused: the
/// record's creating transaction is not the one this spine recorded for
/// that id.
// RED: `rebind` comparing ids only — the other cauldron's `BK-1` is taken.
#[test]
fn rebind_refuses_a_record_another_cauldron_created_under_an_id_ours_also_has() {
    let file_text = include_str!("../../../schema/soup/examples/reading-list.grammar.json");
    let g = spine::user_grammar::parse_file(file_text.as_bytes(), None).unwrap().grammar;
    let (dir, mut c) = soup("rebind-same-ids", true);
    assert!(c.add_pool(&g).unwrap());
    committed(&c, commit(r#"CREATE (b: reading-list/book { handle: "own", state: "reading" }) RETURN b.id"#, &[]));
    drop(c);
    let (other, mut o) = soup("rebind-same-ids-other", true);
    assert!(o.add_pool(&g).unwrap());
    committed(&o, commit(r#"CREATE (b: reading-list/book { handle: "OTHER cauldron BK-1 text", state: "read" }) RETURN b.id"#, &[]));
    drop(o);
    let theirs = other.join("reading-list.db");
    settle(&theirs);
    let conn = rusqlite::Connection::open(&theirs).unwrap();
    conn.execute("DELETE FROM applied_parts", []).unwrap();
    drop(conn);
    let pool = dir.join("reading-list.db");
    settle(&pool);
    std::fs::write(&pool, settle(&theirs)).unwrap();
    let err = format!("{}", spine::user_registry::rebind(&dir, "reading-list", false, "test").unwrap_err());
    assert!(err.contains("`BK-1`") && err.contains("not the write this soup's spine recorded"), "{err}");
}
