//! The door pipeline — how one statement executes as one transaction
//! (two existing designs composed; nothing here changes the commit
//! protocol, the recovery design, or the lock taxonomy).
//!
//! There is exactly ONE write path and this is it.
//! Agent writes, the migration's freight, and the spine's own mechanical
//! operations all enter here; a second import path is a defect by
//! construction.

use crate::delta::{BoundIdentities, CompiledPayload, Delta};
use crate::envelope::{Bindings, Envelope, Initiator, Mode, RippleKind, TxHash};
use crate::error::{Result, SpineError};
use crate::ladle::ast::Statement;
use crate::ladle::compile::{self, CompileCtx, Fuel};
use crate::locks::{self, Claim, ClaimSet, TrancheClaimant};
use crate::pool::{NodeId, QualifiedNodeId};
use crate::read::{self, BoundNode, HopTrace, MatchSet, View};
use crate::recover::{self, Stage};
use crate::schema_def::{Direction, GrammarFault, PoolName};
use crate::spine_db::{AppendOutcome, Seq, SpineDb, TxRow};
use crate::tranche::{ChildOutcome, LandOutcome, LanderStage, LandingStop, Slot, StageCtx};
use crate::witness::Witness;
use crate::{fault, project, Cauldron, Outcome, Returned};
use rusqlite::OptionalExtension;

/// Execute one envelope through the door. THE ORDER OF PHASES (stage
/// names in brackets are recovery's vocabulary, `crate::recover::Stage`,
/// which the fault seam borrows verbatim):
///
///  1. **Halt gate** — a halted spine refuses everything
///     (`SpineError::Halted`; fail-closed). Quiesced: no new
///     commit begins.
///  2. **Fault-seam gate** — a set `SPINE_FAULT` in a build without the
///     feature refuses loudly (`FaultSeamRefused`).
///  3. **Envelope version gate** — mismatch refuses naming both versions.
///  4. **Hash identity + dedup** — a duplicate hash returns the ORIGINAL
///     outcome and lands nothing (exactly-once).
///  5. **Parse** (`ladle::parse`), then **staged-coverage check** — refusal
///     by name.
///  6. **Match phase** as a validated snapshot read (the validated-read
///     machinery, reused; `read::run_match`), spending fuel; count
///     expectations checked against the match.
///  7. **Compile** to interior deltas + premises (`ladle::compile`),
///     grammar-validating every part against its pool's OWN pinned grammar
///     and derive the claim footprint from the pattern text.
///     `Validate` mode STOPS here and returns `Outcome::Validated` — one
///     validator, two callers; no spine row is written (an interior
///     choice).
///  8. **Append the pending row** — statement, bindings, stamps, compiled
///     payload, durably, before any landing (failure here is
///     fail-closed halt). Sequence number issued. [after: `RowAppended`]
///  9. **Acquire locks** in spine-sequence order (`locks::acquire`).
/// 10. **Re-validate premises under the granted locks**; if the data moved,
///     RESTART with a fresh sequence number from phase 6 (never upgrade in
///     place), escalating past `escalation_threshold`,
///     refusing whole with contested claims past `restart_bound`. When the
///     premises hold, the row's BOUND IDENTITIES — which nodes each MATCH
///     binding bound under the grant ([`crate::delta::BoundIdentities`])
///     — are recorded beside the row, durably, still before the
///     decided flip and any landing, so recovery can project RETURN from
///     exactly the set the live path projects from, without re-matching.
/// 10b. **Re-validate OCCUPANCY under the granted locks** — the grammar
///     checks that need to see the pool's current rows rather than only the
///     part's deltas, which today is the at-most-one slice bound. Still
///     BEFORE the decided flip, so a violation refuses WHOLE: no row decided,
///     every lock released, nothing landed. See
///     [`check_occupancy_under_locks`].
/// 11. **Flip the commit-decided marker**. [after: `Decided`]
/// 12. **Land parts**, one atomic SQLite transaction per pool file, in the
///     payload's frozen pool order, flipping each part's landed flag; the
///     committed-commit counter increments with the LAST part.
///     [between each pair: `BetweenLandings`]
/// 13. **Project RETURN from the landed state** ([`project_landed`] — the
///     ONE projection both this door and recovery's roll-forward run), then
///     **flip cleared** (storing the projection) and **release every lock at
///     exactly that flip**. [before: `BeforeCleared`]
/// 14. Return `Outcome::Committed` with the RETURN projection.
///
/// # PHASE 10's RESTART, and the row lifecycle it needs
///
/// Phases 7 through 13 are ONE ATTEMPT, and `execute` loops over attempts.
/// A restart is exactly this: the abandoned attempt
/// releases its locks, its row is **recorded aborted** (decided = 0,
/// cleared = 1 — kept forever), and the next turn appends a
/// **FRESH row with a FRESH sequence number** carrying the payload the new
/// match compiled. Nothing is ever upgraded in place and no claim set ever
/// grows, so the deadlock argument is untouched: every attempt enrols its
/// complete claim set at its own number.
///
/// This needs `hash` to stop being UNIQUE, and it is not: dedup keys on "did
/// this LAND" through two partial indexes —
/// at most one UN-CLEARED attempt per identity, and at most one DECIDED row
/// per identity ever. See `spine_db::SPINE_DDL`'s docs. An aborted attempt
/// blocks nothing, which is also why a caller whose envelope aborted may
/// simply submit that same envelope again.
///
/// `StaleMatch` keeps its one home and only that home: the refusal when the
/// RESTART BOUND is exhausted, carrying the contested claims.
pub fn execute(cauldron: &Cauldron, envelope: Envelope) -> Result<Outcome> {
    let config = cauldron.config().clone();
    // ONE spine connection for the whole door call. `Cauldron` caches none
    // (see its docs), but opening the same file three times inside one
    // statement was redundancy, not a design property: at ~0.57ms per open
    // on this machine it was a fifth of a single-pool commit's latency.
    // Measured before and after.
    //
    // A spine that will not OPEN for writing is the same physics refusal as
    // a spine that will not append, and it used to slip the
    // fail-closed arm entirely: the open's own WAL pragma write failed
    // first, so a read-only spine — the canonical physics fault —
    // returned a bare storage error, never halting, never naming the halt
    // (exec.rs phase-0 versus the
    // phase-13 arm). Routed through the same rule now: announced
    // best-effort, refused NAMING the fail-closed state, no retry. The one
    // honest caveat is in the reason — a durable halt mark cannot be
    // written to storage that refuses writes, so it is stated rather than
    // claimed. `NotFound` stays itself: a missing spine is misconfiguration,
    // not physics refusing a write.
    let mut spine = match cauldron.spine() {
        Ok(s) => s,
        Err(e @ SpineError::NotFound { .. }) => return Err(e),
        Err(e) => {
            eprintln!(
                "spine: HALTED (fail-closed, read-only): the spine cannot be opened for \
                 writing: {e}"
            );
            return Err(SpineError::Halted {
                reason: format!(
                    "fail-closed: the spine cannot be opened for writing ({e}); no commit \
                     begins and no effect exists without its record. The halt \
                     could not be durably recorded on storage that refuses writes — repair \
                     the spine file out-of-band."
                ),
            });
        }
    };

    // ---- PHASE 1: the halt gate, then the quiesce gate ----
    {
        if let Some(reason) = spine.halted()? {
            // Fail-closed: effectively read-only until repaired out-of-band
            // Every subsequent write refuses NAMING the halt.
            return Err(SpineError::Halted { reason });
        }
        // The courtesy refusal (the append gate is the guarantee), naming
        // every standing quiescer by holder and since when.
        let standing = spine.quiescers()?;
        if !standing.is_empty() {
            return Err(recover::quiesced_busy(&standing));
        }
        // CHECKPOINT 1 of the tranche gate: a
        // plain read beside the quiesce read — cheap and racy by design,
        // the courtesy refusal, so most writers are refused `busy
        // tranche_busy / locked` with no row written. The GUARANTEE is
        // checkpoint 2, inside the append's own transaction. A
        // `validate`-mode submit lands nothing and passes the gate.
        if envelope.mode == Mode::Commit {
            if let Some(holder) = crate::tranche::gate_up(spine.conn())? {
                return Err(crate::tranche::locked_busy(&holder));
            }
        }
    }

    // ---- PHASE 2: the fault-seam gate ----
    // In a build without `fault-injection`, a set SPINE_FAULT refuses HERE,
    // at the door, before anything is parsed or appended — so "the seam is
    // absent or refusing" is a property of the production door and not only
    // of a stage this statement might never have reached.
    fault::gate()?;
    // [gate seam: after phase 1] — a writer paused HERE has passed
    // checkpoint 1 with the gate down and reaches the append after it rose
    // (`fault::GateSeam::AfterPhase1`).
    fault::at_gate_seam(fault::GateSeam::AfterPhase1)?;

    // ---- PHASE 3: the envelope format version ----
    envelope.check_format_version()?;

    // ---- PHASE 4: hash identity, and dedup against the record ----
    //
    // NOT IN VALIDATE MODE. `mode` is
    // outside the hash identity by design — a dry run and its commit are the
    // same ask — so a Validate-mode envelope whose identity already landed
    // used to be answered `Committed { deduplicated: true }` here, before the
    // mode was ever read: the caller asked for nothing to land and was told
    // something did. A dry run is judged on the statement alone; whether the
    // identity has landed is the COMMIT's question, and the commit asks it.
    let hash = envelope.hash_identity()?;
    if envelope.mode == Mode::Commit {
        if let Some(row) = spine.find_by_hash(&hash)? {
            // `None` = every attempt at this identity aborted, so it never
            // landed and this submission is a fresh attempt. Fall through.
            if let Some(verdict) = dedup_outcome(&row) {
                return verdict;
            }
        }
        // THE OTHER DIRECTION: a hash that is a STAGED
        // child of a non-terminal tranche is in flight — inside the tranche
        // — and is answered `busy` as a duplicate of an un-cleared row is,
        // naming `tranche <T>` as the holder so the gate's retry rule exempts
        // it. The busy stands until the tranche ends.
        if let Some((t, n)) = spine.staged_child_of_live_tranche(&hash)? {
            return Err(SpineError::Busy {
                message: format!(
                    "hash identity {hash} is in flight as a staged child of tranche {t} (attempt \
                     {n}); the holder is tranche {t}, outside every retry bound \
                     — the step lands or rolls back with the tranche"
                ),
            });
        }
    }

    // ---- PHASE 5: parse, staged coverage, parameters (and provenance) ----
    // Shared with the tranche's child pipeline (`stage`): one
    // statement is judged by one set of gates whichever door it enters.
    let Prepared {
        stmt,
        pools,
        static_claims,
    } = prepare_statement(cauldron, &spine, &envelope)?;
    let lookup = cauldron.grammar_lookup();
    let kinds = cauldron.kind_lookup();

    // ---- PHASE 6: the match phase, as a validated snapshot read ----
    let (mut matched, mut fuel) = match_phase(
        cauldron,
        &stmt,
        &envelope.bindings,
        &config,
        &pools,
        &static_claims,
    )?;

    // THE RESTART LOOP. Each turn is one whole ATTEMPT: compile,
    // append a FRESH row with a FRESH sequence number, acquire, re-validate.
    // When the data moved between snapshot and grant, the abandoned
    // attempt's row is recorded aborted and the next turn appends another —
    // which is what makes the number genuinely fresh and keeps
    // claims-complete-at-open intact, because every attempt enrols its own
    // claim set at its own number and never grows one in place.
    let mut restarts: u32 = 0;
    // Attempts a `recover()` beside this door aborted under it.
    // Counted apart from `restarts`: those are the DATA moving, and they
    // escalate the re-match to real read locks; this is another process
    // sweeping, and locks would not help. Bounded by the same number, so a
    // recovery running in a loop beside the door ends as a loud `Busy`
    // rather than an unbounded chase.
    let mut aborted_underfoot: u32 = 0;
    loop {
        // ESCALATION: past `escalation_threshold` restarts, the
        // attempt also holds the statement's static population claims as real
        // read locks, so the re-match under the grant sees a population held
        // still instead of another optimistic snapshot.
        let escalating = restarts >= config.escalation_threshold;

        // ---- PHASE 7: compile to the four interior deltas plus premises ----
        // Grammar validation of every part, against the touched pool's OWN
        // pinned grammar, happens inside `compile` through the crate's ONE
        // validator, `PoolGrammar::validate_part` (a violation
        // refuses the WHOLE statement with a witness naming the part).
        let payload = {
            let mut ctx = CompileCtx {
                grammars: &lookup,
                pools_of_kind: &kinds,
                fuel: &mut fuel,
            };
            compile::compile(&stmt, &envelope.bindings, &matched, &hash, &mut ctx)?
        };
        // THE FLAG RULE (validate is where the flag rule refuses `refused` /
        // `flag_not_writable`, beside `sealed_body`): a ripple that writes a flag its kind and scope do
        // not grant is refused whole, on both callers, before any row exists.
        crate::flags::check_flag_grants(&envelope.provenance.initiator, &payload, &matched)?;
        // THE REGION GUARD (`crate::regions`): a commit names its regions to
        // the guard the writer installed, before any row exists; a refusal
        // writes nothing. A dry run is never asked.
        if envelope.mode == Mode::Commit {
            if let Some(guard) = cauldron.region_guard() {
                crate::regions::check_guard(cauldron, guard.as_ref(), &envelope.provenance.initiator, &payload)?;
            }
        }

        if envelope.mode == Mode::Validate {
            // The dry run the migration's validate-then-commit shape needs:
            // the SAME statement through the SAME validator, with nothing at
            // stake — no locks taken, no spine row written, nothing landed
            // anywhere (one validator, two callers). It takes no
            // locks, so it can never restart and never reaches this loop's
            // second turn.
            //
            // THE SAME VALIDATOR INCLUDES PHASE 10b: the occupancy-dependent checks used to run
            // only on the commit path, so the dry run accepted a second
            // `current_of` toward an occupied author that the commit then
            // refused `at_most_one_violated` — and the migration's manifest
            // would have marked the item Imported before the one-attempt
            // landing refused the whole set, which is exactly the split
            // validate-then-commit exists to prevent. Here the check reads a snapshot with no
            // locks held, so it is ADVISORY (the match can move before the
            // commit too — so can everything else a dry run judges); it is
            // exact only under the commit's locks. Same function, same
            // witness, same tag.
            check_pins_and_occupancy(cauldron, &payload)?;
            return Ok(Outcome::Validated { tx: hash });
        }

        // ---- PHASE 8: append the pending row, DURABLY, before any landing ----
        let seq = match spine.append_or_existing(&envelope, &hash, &payload) {
            Ok(AppendOutcome::Appended(seq)) => seq,
            // The race the pre-check at phase 4 cannot close is closed here,
            // in the same atomic step as the insert (exactly-once). A duplicate here is a LANDED or LANDING row,
            // never one of this statement's own aborted attempts.
            Ok(AppendOutcome::Duplicate(row)) => match dedup_outcome(&row) {
                Some(verdict) => return verdict,
                None => {
                    return Err(SpineError::hard(format!(
                        "the spine handed back an aborted attempt at seq {} as a duplicate; an \
                         abort must never block a fresh attempt",
                        row.seq
                    )))
                }
            },
            Err(SpineError::Hard { message }) => {
                // PHYSICS REFUSED THE APPEND (fail-closed-and-halt). The
                // record append rides with
                // the effects, so the failure fails CLOSED: no effect exists
                // without its record, this transaction never happened, and the
                // cauldron halts loudly into an effectively read-only state.
                // No retry-forever, no silent degradation, no unrecorded work.
                let reason = format!("the spine append failed: {message}");
                let recorded = recover::halt(&mut spine, &reason);
                return Err(SpineError::Halted {
                    reason: match recorded {
                        Ok(()) => reason,
                        Err(e) => {
                            format!("{reason} (and the halt itself could not be recorded: {e})")
                        }
                    },
                });
            }
            Err(other) => return Err(other),
        };

        // Everything from here can fail with a row already on disk, so it
        // runs inside one body whose failure is handled ONCE, below: an
        // un-decided row aborts (locks released, row kept as the record of
        // the attempt); a DECIDED row is left for recovery to roll forward
        // and keeps its locks, because locks release at the cleared flip
        // only.
        let mut decided = false;
        let landed = commit_after_append(
            cauldron,
            &mut spine,
            &config,
            &stmt,
            &envelope,
            &hash,
            seq,
            &payload,
            &matched,
            &static_claims,
            &pools,
            escalating,
            &mut decided,
        );

        match landed {
            Ok(Attempt::Done(outcome)) => return Ok(outcome),
            Ok(Attempt::Restart { contested }) => {
                // THE DATA MOVED between the snapshot and the grant. The
                // abandoned attempt has already released its locks and its
                // row is recorded aborted; this turn is over.
                restarts += 1;
                if restarts > config.restart_bound {
                    // The stale-match shape, in its own home: the restart
                    // bound exhausted, with the contested claims as witness.
                    return Err(SpineError::refuse(Witness::StaleMatch { contested }));
                }
                let (next_matched, next_fuel) = match_phase(
                    cauldron,
                    &stmt,
                    &envelope.bindings,
                    &config,
                    &pools,
                    &static_claims,
                )?;
                matched = next_matched;
                fuel = next_fuel;
                continue;
            }
            Ok(Attempt::AbortedUnderfoot) => {
                aborted_underfoot += 1;
                if aborted_underfoot > config.restart_bound {
                    return Err(SpineError::Busy {
                        message: format!(
                            "a recovery pass running beside this door aborted {aborted_underfoot} \
                             consecutive attempts at hash identity {hash} under it (bounded by \
                             restart_bound {}); nothing landed — submit again once recovery is \
                             not running in a loop",
                            config.restart_bound
                        ),
                    });
                }
                let (next_matched, next_fuel) = match_phase(
                    cauldron,
                    &stmt,
                    &envelope.bindings,
                    &config,
                    &pools,
                    &static_claims,
                )?;
                matched = next_matched;
                fuel = next_fuel;
                continue;
            }
            Err(e) => {
                if decided {
                    // A decided transaction is COMMITTED by the design's own
                    // definition (the row flip is the commit point). There
                    // is no roll-back of landed parts, ever —
                    // that would be a second write path into pools — so the
                    // row stays decided with its un-landed parts, its locks
                    // stay held until the cleared flip, and RECOVERY rolls it
                    // forward from the row's payload. Reported
                    // as a weak seam: a part that refuses deterministically at
                    // landing leaves this transaction stuck for recovery.
                    return Err(e);
                }
                // ABORT: refusal is WHOLE — nothing landed anywhere. The
                // row remains forever as the record of the
                // aborted attempt. Whatever this door enrolled
                // on the row is released on the way out, arbiter or no
                // arbiter (`abandon_attempt`).
                abandon_attempt(&mut spine, seq)?;
                return Err(e);
            }
        }
    }
}


/// What phase 5 establishes about a statement before any pool is read
/// (factored out of `execute` so the child pipeline runs the same
/// gates): the parsed statement, the real pools its pattern names, and the
/// static claim footprint.
struct Prepared {
    stmt: Statement,
    pools: Vec<PoolName>,
    static_claims: ClaimSet,
}

/// PHASE 5 — parse, staged coverage, parameters, provenance, the write
/// form, the import gate, the match-head states, the projected-namespace
/// nets and the ghost edge-target guard; then the pools and the static
/// claims. The comments are `execute`'s own, moved with the code.
fn prepare_statement(cauldron: &Cauldron, spine: &SpineDb, envelope: &Envelope) -> Result<Prepared> {
    // `Envelope::validate` composes exactly these gates in the door's own
    // order; it re-checks the version, which is idempotent.
    let stmt = envelope.validate()?;
    // THE WRITE FORM: a statement with no mutation clause and
    // no premise IS the bulk-read form — "the same grammar with no mutation
    // clauses ... executed as a READ TRANSACTION" — and the read door is
    // where it belongs (`Cauldron::read`). Spoken to the write door it is
    // refused by name, before any pool is opened, because of what landing
    // it would leave on the record: a decided-and-cleared row with ZERO
    // parts, which the committed-commit counter never counts (`mark_part_
    // landed` is where the counter moves, and there is no part to land) —
    // a row the spine's own exposures cannot account for.
    // That anomaly is the ground; the "code that runs and does nothing"
    // principle is not — it binds code, not the door's semantics, and the
    // door's own no-op cases (an empty match, a set
    // no-op edge) commit as asked.
    compile::check_write_form(&stmt)?;
    // WHO MAY IMPORT: the carried-id
    // form is an `Agent`'s alone. Refused whole, before any pool is opened,
    // on both modes — so a dry run from the wrong initiator is refused by
    // the same name its commit would be.
    check_import_initiator(&envelope.provenance.initiator, &stmt)?;

    let lookup = cauldron.grammar_lookup();
    // The MATCH heads' states against the closed vocabularies — the same
    // grammar facts SET refuses by name, judged before the match runs.
    compile::check_match_states(&stmt, &envelope.bindings, &lookup)?;
    let footprint = compile::footprint(&stmt);
    // The projected namespace, distinguished by WHAT TOUCHES IT:
    //
    // - A CREATE head addressed INTO it is a write, and the pen refuses:
    //   origin is code. SET/REMOVE reach it
    //   only through a binding some head names, so head checks cover all.
    // - A MATCH head in it BY IDENTIFIER is the ghost-resolution ask, and it
    //   now RESOLVES — through the one matcher (`read::run_match`), against
    //   the spine's own tables, computed on read (the projection
    //   "resolves transparently on read, refuses on write"). It takes no lock claim and is excluded from the validated
    //   read's pool set: nothing can ever write the projection. Every OTHER
    //   match form into it is `match:projected-pattern`, staged OUT and
    //   refused by name by the coverage table inside `envelope.validate()`
    //   above. The old gate here, `match:projected-namespace`, is retired;
    //   this is a DERIVED decision.
    // - An edge AIMED at a ghost that names no row is refused
    //   `GhostTargetUnknown` (an edge minted toward a ghost
    //   identifier that names no row is refused) — below, BEFORE the match
    //   runs, because a ghost that resolves to nothing would otherwise bind
    //   nothing, the edge would expand to zero deltas, and the statement
    //   would "succeed" landing nothing under the empty-match rule.
    //   `project::guard_edge_target` is that guard and this is its caller.
    //   How the source grammar DECLARES that it may aim an edge there is a
    //   reach toward kind `spine` (`schema_def::TypeRef::Reach`), judged by
    //   the one validator inside `compile` like every other cross-pool edge.
    for head in stmt.created_bindings() {
        if project::is_projected(&head.pool) {
            return Err(SpineError::refuse(Witness::ProjectedNamespaceWrite {
                pool: head.pool.clone(),
                target: format!("CREATE ({})", head.binding.0),
            }));
        }
    }
    // An IMPORT head into the projection is the same write, refused
    // by the same witness with its own head text — the text `compile`'s
    // site-4 duplicate writes.
    for head in stmt.imported_bindings() {
        if project::is_projected(&head.pool) {
            return Err(SpineError::refuse(Witness::ProjectedNamespaceWrite {
                pool: head.pool.clone(),
                target: format!("IMPORT ({})", head.binding.0),
            }));
        }
    }
    guard_ghost_edge_targets(spine, &stmt, &envelope.bindings)?;
    // The pools the validated read opens: every real pool the pattern
    // names. The projected namespace is NOT one of them — it has
    // no file to snapshot and nothing that could move under the read.
    let pools: Vec<PoolName> = footprint
        .pools
        .iter()
        .filter(|p| !project::is_projected(p))
        .cloned()
        .collect();
    // The STATIC claim footprint — the populations the pattern scans. It is
    // what a validated read escalates to real read locks on, and what the
    // write path's own escalation adds to its concrete claims so a re-match
    // under locks sees a population held still.
    let static_claims = compile::claim_footprint(&stmt, &lookup)?;
    Ok(Prepared {
        stmt,
        pools,
        static_claims,
    })
}

/// PHASE 5's import gate (the import's
/// initiator is `Agent`). A statement carrying any `IMPORT` clause from any
/// other initiator is refused whole with
/// [`Witness::ImportNotPermitted`]; a statement with none passes untouched,
/// whoever sent it. TOTAL over [`Initiator`]: `External` never reaches here
/// through either door (`Envelope::check_provenance` refuses it as `Usage`
/// earlier in phase 5), and its arm exists so a direct caller of this `pub
/// fn` gets an answer rather than a gap.
///
/// Why each refusal: `CoreMechanical` is the harness's own identity, which
/// would misattribute an agent's act; its
/// `harness {operation}` principal is the migration driver AGAINST THE
/// DAEMON, which this build does not run. A `Ripple` never carries a v5 id:
/// its scope grammar is flag fields.
pub fn check_import_initiator(initiator: &Initiator, stmt: &Statement) -> Result<()> {
    let imports = stmt
        .mutations
        .iter()
        .any(|m| matches!(m, crate::ladle::ast::Mutation::ImportNode(_)));
    if !imports {
        return Ok(());
    }
    let initiator_kind = match initiator {
        Initiator::Agent { .. } => return Ok(()),
        Initiator::CoreMechanical { .. } => "core_mechanical".to_string(),
        Initiator::Ripple { kind, .. } => kind.as_str().to_string(),
        Initiator::External => "external".to_string(),
    };
    Err(SpineError::refuse(Witness::ImportNotPermitted { initiator_kind }))
}

/// PHASE 5's ghost-target guard: for every CREATE-edge
/// clause whose far end is a by-identifier MATCH head into the projected
/// namespace, the identifier must name a spine row — resolved NOW, against
/// the door's own spine connection, through [`project::guard_edge_target`].
/// A REMOVE toward a ghost is not guarded: nothing is minted, and an edge
/// toward an address no row has cannot exist to be removed (the empty match
/// lands nothing).
///
/// The identifier's value comes from the head's `id` entry (a literal or a
/// `$param`, both already known to be supplied by `check_params`); a value
/// that is not text is left to the matcher, which refuses it as malformed.
fn guard_ghost_edge_targets(
    spine: &SpineDb,
    stmt: &Statement,
    bindings: &crate::envelope::Bindings,
) -> Result<()> {
    use crate::ladle::ast::{ArrowDir, Mutation};
    for m in &stmt.mutations {
        let Mutation::CreateEdge {
            from, to, arrow, ..
        } = m
        else {
            continue;
        };
        let dst = match arrow {
            ArrowDir::LeftToRight => to,
            ArrowDir::RightToLeft => from,
        };
        let Some(head) = stmt
            .matches
            .iter()
            .flat_map(|c| c.heads())
            .find(|h| h.binding == *dst && project::is_projected(&h.pool))
        else {
            continue;
        };
        if let Some(target) = projected_head_address(head, bindings) {
            project::guard_edge_target(spine, &target)?;
        }
    }
    Ok(())
}

/// THE GHOST GUARD UNDER A TRANCHE (the projected namespace
/// under a tranche): `ghost_target_unknown`
/// extended from `CREATE`-edge targets to EVERY projected head whose
/// identifier names no row — a child at staging step 4, a read under key before
/// its match. A row never disappears and its identity never changes, but a
/// row can APPEAR at an address (the hash identity is deterministic, so a
/// holder can name a transaction before its row is appended), and a head
/// that bound nothing at staging could bind at the landing: a `REMOVE`
/// toward it would stage zero deltas and re-compile one at re-validation, a `RETURN`
/// would answer an absence the landing cannot hold. Not called on the
/// ordinary door, whose guard stays [`guard_ghost_edge_targets`]. The moving
/// fields of an EXISTING row are the other half, refused by shape in
/// `compile::check_projected_under_tranche`.
pub(crate) fn guard_projected_heads(
    spine: &SpineDb,
    stmt: &Statement,
    bindings: &crate::envelope::Bindings,
) -> Result<()> {
    for head in stmt.matches.iter().flat_map(|c| c.heads()) {
        if !project::is_projected(&head.pool) {
            continue;
        }
        if let Some(target) = projected_head_address(head, bindings) {
            project::guard_edge_target(spine, &target)?;
        }
    }
    Ok(())
}

/// A projected head's address — its `id` entry resolved against the
/// bindings — or `None` when the head carries no `id` (the staged-out
/// pattern form, refused by name upstream), the parameter is unsupplied
/// (`check_params` refuses that), or the value is not text (the matcher
/// refuses that as malformed).
fn projected_head_address(
    head: &crate::ladle::ast::NodePattern,
    bindings: &crate::envelope::Bindings,
) -> Option<QualifiedNodeId> {
    use crate::ladle::ast::{ValueExpr, FIELD_ID};
    let value = match head.field(FIELD_ID)? {
        ValueExpr::Literal(v) => v.clone(),
        ValueExpr::Param(p) => bindings.get(p)?.clone(),
    };
    let text = value.as_str()?;
    Some(QualifiedNodeId {
        pool: head.pool.clone(),
        id: NodeId(text.to_string()),
    })
}

/// What one attempt through phases 9–14 concluded.
enum Attempt {
    /// The commit landed and cleared.
    Done(Outcome),
    /// The data moved under the granted locks. This attempt's locks are
    /// released and its row is recorded aborted; the caller appends a fresh
    /// row with a fresh sequence number.
    Restart { contested: Vec<Claim> },
    /// A `recover()` running beside this door aborted this attempt's
    /// un-decided row under it. Nothing landed
    /// (an abort never does), the locks this attempt enrolled on the now
    /// cleared row are released, and the caller appends a fresh row — the
    /// restart shape, for a reason that is not the data's.
    AbortedUnderfoot,
}

/// ABANDON this attempt's un-decided row: the abort arbiter, and — when the
/// arbiter LOST because the row is already cleared — the release of the
/// locks this door enrolled on it anyway.
///
/// `abort_row` releases only behind a won flip, correctly: a lost flip may
/// mean the row was DECIDED by someone (never for the door's own row, but the
/// rule is the arbiter's). When the row is CLEARED and un-decided, though,
/// it is an abort by somebody's hand — a `recover()` beside this door — and
/// a cleared row can never be decided afterwards, so no landing can ever
/// begin under these locks: they are orphans by the orphan sweep's own
/// reasoning, and this door, which enrolled them, releases them on its way
/// out. Otherwise the next claimant on these bodies waits to the ceiling
/// behind a ghost until some later `recover()` sweeps.
fn abandon_attempt(spine: &mut SpineDb, seq: Seq) -> Result<()> {
    let row = spine.row_at(seq)?;
    if recover::abort_row(spine, &row)? {
        return Ok(());
    }
    let row = spine.row_at(seq)?;
    if row.cleared && !row.decided {
        locks::release_at_clear(spine, seq)?;
    }
    Ok(())
}

/// Phases 9 through 14, with the row already durably on disk.
#[allow(clippy::too_many_arguments)]
fn commit_after_append(
    cauldron: &Cauldron,
    spine: &mut SpineDb,
    config: &crate::config::SpineConfig,
    stmt: &Statement,
    envelope: &Envelope,
    hash: &TxHash,
    seq: Seq,
    payload: &CompiledPayload,
    matched: &MatchSet,
    static_claims: &ClaimSet,
    pools: &[PoolName],
    escalating: bool,
    decided: &mut bool,
) -> Result<Attempt> {
    // [stage: after the row append] — the first of the four named commit
    // stages, using RECOVERY'S own vocabulary.
    fault::at_stage(Stage::RowAppended)?;

    let lookup = cauldron.grammar_lookup();
    let kinds = cauldron.kind_lookup();
    let scalar = |pool: &PoolName, kind: &str, direction: Direction| -> bool {
        cauldron
            .grammar(pool)
            .map(|g| g.is_scalar_slice(kind, direction))
            .unwrap_or(false)
    };

    // ---- PHASE 9: acquire this attempt's claims, at THIS attempt's number ----
    let mut claims = compile::claims_for(stmt, matched, payload, &lookup)?;
    if escalating {
        // ESCALATION: re-match under REAL READ LOCKS on the
        // match set — delivered by adding the statement's static population
        // claims, so the rows the pattern scans are held still while the
        // re-match runs.
        for c in static_claims.iter() {
            claims.insert(c.clone());
        }
    }
    // THE UNWIRED GUARD, WIRED: link is forbidden on an
    // at-most-one slice, and only the grammar knows which slices are scalar,
    // so `locks::acquire` cannot check it and the DOOR must. `claims_for`
    // already derives WRITE for a scalar slice; this is the net under that
    // derivation, and an unstrung net is not a net.
    //
    // CLAIMS ARE COMPLETE AT OPEN: this set is enrolled once, at this
    // attempt's own sequence number, and never grown. A restart is what
    // makes that law affordable (no in-place upgrade).
    locks::check_link_claims(&claims, &scalar)?;
    locks::acquire(spine, seq, &claims, config)?;

    // ---- PHASE 10: re-validate the premises UNDER THE GRANTED LOCKS ----
    // By re-running the same match and the same compiler against fresh
    // snapshots. One evaluator, not two: a moved EXPECT refuses here with
    // exactly the expected-versus-found witness, and a moved
    // WHERE with exactly the failing-condition witness, because it is the
    // very code that produced them the first time.
    let open = read::open_pools(cauldron, pools)?;
    let snaps = read::open_snapshots(&open)?;
    let mut check_fuel = Fuel::new(config.fuel_ceiling);
    let re_matched = read::run_match(&snaps, stmt, &envelope.bindings, &mut check_fuel)?;
    let re_payload = {
        let mut ctx = CompileCtx {
            grammars: &lookup,
            pools_of_kind: &kinds,
            fuel: &mut check_fuel,
        };
        compile::compile(stmt, &envelope.bindings, &re_matched, hash, &mut ctx)?
    };
    drop(snaps);
    drop(open);

    if re_payload != *payload {
        // THE DATA MOVED between the snapshot and the grant. RESTART, which
        // here means exactly this: this attempt
        // releases its locks, its row is recorded aborted, and the caller
        // appends a FRESH row with a FRESH sequence number. Nothing is
        // upgraded in place, so the deadlock argument is untouched.
        let contested = contested_claims(cauldron, stmt, matched, &re_matched, &re_payload, payload)?;
        // The atomic abort arbiter flips the row and releases the locks
        // behind a won flip; this attempt owns its own un-decided row, so it
        // wins unless a racing `recover()` already aborted it — in which
        // case THIS door's enrolment is the orphan, and `abandon_attempt`
        // releases it (the locks were enrolled after the abort, so the abort
        // released nothing).
        abandon_attempt(spine, seq)?;
        return Ok(Attempt::Restart { contested });
    }
    // The premises hold and the payload the row durably records is still
    // exactly what this statement means. Proceed.
    //
    // THE BOUND IDENTITIES, durably: which nodes each MATCH binding
    // bound UNDER THE GRANT — the set the RETURN projection has always been
    // taken over — recorded beside the row before the decided flip and
    // before any landing, so recovery projects from exactly this set with no
    // re-match. Not in the same SQLite transaction as the append (that
    // transaction is `spine_db.rs`'s); sound anyway,
    // because a row can only be DECIDED below this line, so every decided
    // row has its record, and an undecided row without one is aborted by
    // recovery like any other undecided row. Failure here flows to
    // `execute`'s undecided-row arm: the row is recorded aborted. From here
    // the row's record is the whole truth, and phase 13 reads it, not
    // `re_matched`.
    let bound = bound_of(stmt, &re_matched);
    drop(re_matched);
    record_bound(spine, seq, &bound)?;

    // ---- PHASE 10b: PINS AND OCCUPANCY, still BEFORE the decided flip ----
    // The last grammar checks that cannot be answered from the part alone:
    // the pin each part validated under against the pin its pool FILE holds
    // now, and the at-most-one occupancy. They run HERE, under this
    // attempt's granted locks and before anything is decided, so that a
    // violation is an ORDINARY whole-statement refusal. See
    // `check_pins_and_occupancy` for why the locks make this sound.
    check_pins_and_occupancy(cauldron, payload)?;

    // ---- PHASE 11: the commit-decided marker ----
    // Conditional: a `recover()` beside this
    // door may have ABORTED this un-decided row since the append — nothing
    // marks a row live. The locks this attempt enrolled since are then
    // orphans on a cleared row (a cleared row is never decided afterwards),
    // released here, and the attempt restarts with a fresh row. Never a
    // `Usage` to a caller whose statement was correct.
    if !spine.try_mark_decided(seq)? {
        locks::release_at_clear(spine, seq)?;
        return Ok(Attempt::AbortedUnderfoot);
    }
    *decided = true;
    // [stage: after the decided flip]
    fault::at_stage(Stage::Decided)?;
    // THE ROW'S PAYLOAD FROM HERE, NOT THE ONE IN HAND: the decided
    // flip numbered every serial id (`<prefix>-<n>`) in the same spine
    // transaction, rewriting the row's provisional ids, so the payload that
    // lands and that RETURN projects is the row's — the very one recovery
    // would roll forward. Identical to the one in hand when the statement
    // mints no prefixed type.
    let decided_payload =
        wait_turn(config, "the decided row's payload", || Ok(spine.row_at(seq)?.payload))?;
    let payload = &decided_payload;

    // ---- PHASE 12: land each part, one atomic transaction per pool ----
    //
    // FROM HERE EVERY WRITE IS MECHANICAL and "mechanical writes wait their
    // turn; they are never refused, only late". A transient
    // hold on the spine or a pool file — another process's write lock for a
    // few seconds — used to turn a flip into `Busy` after the per-statement
    // busy timeout, and the door then ABANDONED a decided commit to
    // recovery: row decided, parts landed, un-cleared, locks held, every
    // later claimant on those resources Busy at the ceiling. Each landing
    // and flip below now waits its
    // turn through `wait_turn`, up to the configured lock ceiling in total;
    // a hold that outlasts the ceiling still leaves the row for recovery,
    // loudly, and that residual is a known one.
    for (ordinal, part) in payload.parts.iter().enumerate() {
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| SpineError::usage("a statement cannot carry more parts than u32::MAX"))?;
        if ordinal > 0 {
            // [stage: between one pool's landing and the next] — the one
            // window that distinguishes "the commit point is the row" from
            // "the commit point is the storage engine".
            fault::at_stage(Stage::BetweenLandings)?;
        }
        let landing = wait_turn(config, "the pool landing", || {
            let mut pool = cauldron.open_pool(&part.pool)?;
            pool.apply_part(hash, ordinal, part)
        });
        // DEFENCE IN DEPTH, AND ITS ONE LOUD EXCEPTION. `Pool::apply_part`
        // re-validates the part — grammar, pin, occupancy — inside the
        // landing transaction and MUST keep doing so (a caller reaching the
        // store directly is not this door). But for a statement that came
        // through this door, phases 7 and 10b already judged all of it under
        // the very locks that hold it still, so ANY refusal HERE is not a
        // caller's refusal at all — it is a broken invariant of this crate,
        // and it is named as one.
        landing.map_err(|e| refusal_after_decided_is_an_invariant_violation(e, seq, ordinal))?;
        // The per-part landed flag; the committed-commit counter increments
        // inside the same spine transaction as the LAST part's flip.
        wait_turn(config, "the part's landed flip", || {
            spine.mark_part_landed(seq, ordinal)
        })?;
    }

    // ---- PHASE 13: project RETURN from the LANDED state, then clear ----
    // From the row's own record (payload + bound identities), through the
    // ONE projection recovery also runs — so a duplicate of a recovered
    // commit and a duplicate of a live one are answered from the same
    // computation.
    //
    // TWO PARTIES CAN FINISH A DECIDED ROW: this
    // door, and a `recover()` running beside it. If recovery already cleared
    // the row, its projection — taken under the row's locks — IS the
    // outcome, and anything projected here would be read under nobody's
    // lock. So: a row already cleared returns the stored outcome without
    // projecting; otherwise project, try the conditional flip, and on losing
    // it return the stored outcome anyway.
    //
    // THE RELEASE IS CONDITIONAL ON THE ROW BEING CLEARED, not on this door
    // having cleared it. The flip's rule — only the
    // winner stores the result and the audit mark — was applied to the
    // release too in a first cut, so a door that LOST the flip
    // returned without touching the lock table and trusted the winner to
    // release. The winner is a recovery pass whose flip-then-release is the
    // very two-step the orphan sweep exists for: if it dies or is late
    // between the two, the CLEARED row's granted locks survive this door's
    // return and the next claimant is Busy at the ceiling naming a holder
    // whose transaction is over. A cleared row never un-clears, so its lock
    // rows are orphans by `release_cleared_orphans`'s own definition, this
    // door enrolled them, and `release_at_clear` is a bare DELETE by seq —
    // idempotent, so releasing on both loser paths costs the winner nothing.
    // The same reasoning `abandon_attempt` already applies on the abort arm.
    if spine.row_at(seq)?.cleared {
        wait_turn(config, "the lock release", || locks::release_at_clear(spine, seq))?;
        return Ok(Attempt::Done(stored_outcome(spine, hash, seq)?));
    }
    let mut fuel = Fuel::new(config.fuel_ceiling);
    let returned = project_landed(cauldron, stmt, &envelope.bindings, payload, &bound, &mut fuel)?;
    let result = serde_json::to_value(&returned)
        .map_err(|e| SpineError::hard(format!("RETURN projection will not serialize: {e}")))?;
    // [stage: before the cleared flip]
    fault::at_stage(Stage::BeforeCleared)?;
    if !wait_turn(config, "the cleared flip", || spine.mark_cleared(seq, &result))? {
        wait_turn(config, "the lock release", || locks::release_at_clear(spine, seq))?;
        return Ok(Attempt::Done(stored_outcome(spine, hash, seq)?));
    }
    // AT EXACTLY THAT FLIP, and never per-part as parts land:
    // this release rule is one of the three ways the half-landed window
    // stays invisible.
    wait_turn(config, "the lock release", || locks::release_at_clear(spine, seq))?;

    // ---- PHASE 14 ----
    Ok(Attempt::Done(Outcome::Committed {
        tx: *hash,
        seq,
        returned,
        deduplicated: false,
    }))
}

/// A MECHANICAL WRITE WAITS ITS TURN ("they queue
/// like everyone else and land late rather than torn"). Run `op`
/// and, while it answers `Busy` — the file's write lock held by another
/// process past the per-statement `busy_timeout` — try again with the lock
/// poll's backoff, until `lock_wait_ceiling` has elapsed in total; then
/// answer the last `Busy`, naming what waited and for how long. Any other
/// outcome passes through at once. Used only after the decided flip, where
/// abandoning the row would strand a commit the design already considers
/// made.
fn wait_turn<T>(
    config: &crate::config::SpineConfig,
    what: &str,
    mut op: impl FnMut() -> Result<T>,
) -> Result<T> {
    let started = std::time::Instant::now();
    let mut interval = config.lock_poll_interval;
    let max_interval = config.lock_poll_interval * 32;
    loop {
        match op() {
            Err(SpineError::Busy { message }) => {
                if started.elapsed() >= config.lock_wait_ceiling {
                    return Err(SpineError::Busy {
                        message: format!(
                            "{what} waited its turn for {:?} (the lock wait ceiling) and the file \
                             stayed busy: {message}",
                            config.lock_wait_ceiling
                        ),
                    });
                }
                std::thread::sleep(interval);
                interval = (interval * 2).min(max_interval);
            }
            other => return other,
        }
    }
}

/// This door's OWN commit, finished by the other party (recovery beside it):
/// the outcome the winning cleared flip stored — projected under the row's
/// locks before they were released — returned as this commit's reply. Not
/// `deduplicated`: this is the statement's own submission, landed once, by
/// its own row; only the projection was computed by another hand.
fn stored_outcome(spine: &SpineDb, hash: &TxHash, seq: Seq) -> Result<Outcome> {
    let row = spine.row_at(seq)?;
    let returned = match &row.result {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            SpineError::hard(format!(
                "transaction {hash} (seq {seq}) stored a RETURN projection that will not parse: {e}"
            ))
        })?,
        None => {
            return Err(SpineError::hard(format!(
                "transaction {hash} (seq {seq}) is cleared with no stored outcome; a cleared \
                 commit always stores the projection its cleared flip was made with"
            )))
        }
    };
    Ok(Outcome::Committed {
        tx: *hash,
        seq,
        returned,
        deduplicated: false,
    })
}

/// Which end of an at-most-one edge kind a part touches. Deliberately the
/// same shape, in the same order, as `pool.rs`'s private `ScalarEnd`: the two
/// checks must agree exactly, and the ordering fixes which of several
/// violations is the one reported.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ScalarEnd {
    From(String),
    To(PoolName, String),
}

/// PHASE 10b — the STORED-STATE-DEPENDENT grammar checks, evaluated BEFORE
/// the decided flip, under this attempt's granted locks: the PIN each part
/// (and each cross-pool edge's far end) validated under, against the pin the
/// pool FILE holds now; and the at-most-one OCCUPANCY.
///
/// # The pin
///
/// `Cauldron` loads every grammar once at create/open and the door compiles
/// against that cache; `Pool::apply_part` re-opens the file and validates
/// under the pin the FILE holds. Between the two is the decided flip. A pin
/// moved under the open handle (out-of-band today; future
/// migration machinery tomorrow) therefore used to pass phase 7 and refuse
/// `version_mismatch` at phase 12 — after the row was committed — and the
/// caller was answered `Refused` ("nothing landed") while the row sat
/// decided, un-landed, locks held, recovery repeating the refusal every
/// pass: the at-most-one wedge already treated as a crate defect, on a
/// different rule. So the file's pin is compared HERE, for every part's own
/// pool and for every far pool a part records a far pin for, and a moved
/// pin refuses WHOLE by the same name the landing would have used. The
/// handle's grammar set stays fixed at open (nothing in this build moves a
/// pin through a supported operation); a caller whose refusal names a moved
/// pin reopens the cauldron. DERIVED: cache invalidation is left to the
/// migration machinery to design.
///
/// # Why occupancy exists here
///
/// A grammar rule that constrains a SLICE — today exactly one rule, the
/// at-most-one bound — cannot be judged from the compiled part
/// alone: it needs the edges already in the pool. That is why
/// [`crate::pool::Pool::apply_part`] checks it inside the landing
/// transaction. But the landing transaction runs AFTER the decided flip, and
/// a refusal there is not a refusal at all: the row is already committed by
/// the design's own definition, so nothing rolls back, the locks
/// are held to a cleared flip that never comes, and recovery rolls the row
/// forward into the same deterministic refusal forever. One ordinary
/// mistyped statement wedged the whole cauldron. The rule is that
/// a statement that fails anywhere lands nothing anywhere and returns one
/// refusal, so that was a defect, and this is the fix at the source.
///
/// # Why reading the pool here is SOUND
///
/// Occupancy is stable between this check and the landing because the
/// statement already holds a slice WRITE lock on every at-most-one slice it
/// changes. `ladle::compile::claims_for` derives `LockMode::Write` for a
/// membership change on a scalar slice, and
/// `locks::check_link_claims` refuses a mere LINK claim there. The claim set
/// was enrolled and granted at phase 9, complete, at this attempt's own
/// sequence number, and it is never grown — so no other transaction can add
/// or remove an edge on these slices between this snapshot and phase 12.
/// The lock taxonomy is untouched by this check; it only READS what the
/// taxonomy already holds still.
///
/// The witness is byte-identical in structure to the landing check's: the
/// same `at_most_one_violated` fault tag, the same pool, the same part
/// ordinal, the same message. What changes is WHEN it is raised, and
/// therefore that it is whole.
fn check_pins_and_occupancy(cauldron: &Cauldron, payload: &CompiledPayload) -> Result<()> {
    check_pins_and_occupancy_over(cauldron, payload, None)
}

/// [`check_pins_and_occupancy`] against THE VIEW when `view` is `Some`
/// (pins and occupancy are checked against the view): the pins still come
/// off the FILES — an overlay carries its
/// pool's pin — and the occupancy, the edge fields and the carried ids are
/// read off snapshots with the tranche's staged deltas layered on top, so a
/// child is judged against what the earlier children did.
fn check_pins_and_occupancy_over(
    cauldron: &Cauldron,
    payload: &CompiledPayload,
    view: Option<&View>,
) -> Result<()> {
    let mut names: Vec<PoolName> = Vec::new();
    for part in &payload.parts {
        if !names.contains(&part.pool) {
            names.push(part.pool.clone());
        }
        for far in part.far_versions.keys() {
            if !names.contains(far) && !project::is_projected(far) {
                names.push(far.clone());
            }
        }
    }
    if names.is_empty() {
        return Ok(());
    }
    cauldron.check_user_files(&names)?;
    let open = read::open_pools(cauldron, &names)?;

    // ---- the pins, off the FILES ----
    for (ordinal, part) in payload.parts.iter().enumerate() {
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| SpineError::usage("a statement cannot carry more parts than u32::MAX"))?;
        let held = open.pin_of(&part.pool).ok_or_else(|| {
            SpineError::hard(format!(
                "pool `{}` was not opened while checking its pin before the decided flip",
                part.pool
            ))
        })?;
        if held != part.schema_version {
            return Err(GrammarFault::new(
                "version_mismatch",
                format!(
                    "part validated under pin {} but pool `{}` now holds pin {held}; the pin moved \
                     under this cauldron handle — reopen the cauldron",
                    part.schema_version, part.pool
                ),
            )
            .refusal(&part.pool, Some(ordinal)));
        }
        for (far, recorded) in &part.far_versions {
            if project::is_projected(far) {
                continue;
            }
            let held = open.pin_of(far).ok_or_else(|| {
                SpineError::hard(format!(
                    "far pool `{far}` was not opened while checking its pin before the decided flip"
                ))
            })?;
            if held != *recorded {
                return Err(GrammarFault::new(
                    "version_mismatch",
                    format!(
                        "part records far pin {recorded} for pool `{far}` but that pool now holds \
                         pin {held}; the pin moved under this cauldron handle — reopen the cauldron"
                    ),
                )
                .in_pool(far)
                .refusal(&part.pool, Some(ordinal)));
            }
        }
    }

    // ---- the occupancy, off the SNAPSHOTS (over the view, under a tranche) ----
    let snaps = match view {
        Some(v) => read::open_snapshots_over(&open, v)?,
        None => read::open_snapshots(&open)?,
    };
    for (ordinal, part) in payload.parts.iter().enumerate() {
        let ordinal = u32::try_from(ordinal)
            .map_err(|_| SpineError::usage("a statement cannot carry more parts than u32::MAX"))?;
        let grammar = cauldron.grammar(&part.pool).ok_or_else(|| {
            SpineError::hard(format!(
                "no pinned grammar for pool `{}` while checking at-most-one occupancy",
                part.pool
            ))
        })?;
        let snap = snaps.of(&part.pool)?;

        // EXACTLY `pool::note_scalars`' rule, including "only an AddEdge can
        // put a slice over its bound" and "an at-most-one `to` is only
        // enforceable when the slice lives in THIS pool". The two must not
        // drift; a mutation of either is meant to red the same tests.
        //
        // A far end in the projected `spine` namespace is skipped by the
        // `dst.pool == part.pool` test like any other far pool, and stays
        // skipped on purpose: a ghost's incoming slice is stored
        // in no pool — it is the union, across every pool, of edges aimed
        // at one address — and `check_wellformed` refuses at-most-one on
        // every reach, so no bound can exist for this check to enforce.
        let mut ends: std::collections::BTreeSet<(String, ScalarEnd)> =
            std::collections::BTreeSet::new();
        for delta in &part.deltas {
            if let Delta::AddEdge { src, kind, dst, .. } = delta {
                if grammar.is_scalar_slice(kind, Direction::Out) {
                    ends.insert((kind.clone(), ScalarEnd::From(src.0.clone())));
                }
                if grammar.is_scalar_slice(kind, Direction::In) && dst.pool == part.pool {
                    ends.insert((
                        kind.clone(),
                        ScalarEnd::To(dst.pool.clone(), dst.id.0.clone()),
                    ));
                }
            }
        }

        for (kind, end) in &ends {
            let n = projected_occupancy(snap, part, kind, end)?;
            if n > 1 {
                let which = match end {
                    ScalarEnd::From(s) => format!("leaving `{s}`"),
                    ScalarEnd::To(p, d) => format!("arriving at `{p}/{d}`"),
                };
                return Err(GrammarFault::new(
                    "at_most_one_violated",
                    format!(
                        "edge kind `{kind}` is an at-most-one slice {which}; the part leaves {n}"
                    ),
                )
                .refusal(&part.pool, Some(ordinal)));
            }
        }

        check_edge_fields_agree(snap, part, ordinal)?;
        check_carried_ids_absent(snap, part, ordinal)?;
    }
    Ok(())
}

/// A CARRIED id (an `IMPORT`'s) the pool already holds refuses
/// `duplicate_node_id` HERE — before the decided flip, whole — and not at
/// the landing. A store-minted id (hash or provisional serial) is fresh by
/// construction; a carried one is the caller's, so it is the one CreateNode
/// id that can collide with a stored node the statement never MATCHed, and
/// compile's validator (which knows only the match set) cannot see that.
/// The landing's own check would see it — after the flip, where a refusal
/// is the wedge `check_pins_and_occupancy`'s doc describes. Same tag, same
/// message as the validator's.
///
/// Sound under the commit's locks: a `CreateNode` claims WRITE on
/// `Resource::Body { node: id }` (`ladle::compile::claims_for`), so two
/// imports of one id serialize and the second reads the first's landed node
/// here. In the dry run (no locks) it is advisory, like occupancy.
fn check_carried_ids_absent(
    snap: &crate::pool::Snapshot<'_>,
    part: &crate::delta::CompiledPart,
    ordinal: u32,
) -> Result<()> {
    let single = crate::delta::CompiledPayload {
        parts: vec![part.clone()],
        minted: Default::default(),
    };
    for (_, prefix, n) in single.carried_serials() {
        let id = crate::delta::serial_node_id(&prefix, n);
        if snap.node(&id)?.is_some() {
            return Err(GrammarFault::new(
                "duplicate_node_id",
                format!("node `{id}` already exists in pool `{}`", part.pool),
            )
            .refusal(&part.pool, Some(ordinal)));
        }
    }
    Ok(())
}

/// ONE EDGE PER TRIPLE, with fields. An edge's identity is
/// (src, kind, dst) and edges are a set: a create of an edge that
/// already exists — stored, or added earlier in this same part and not
/// removed since — is a no-op when its fields are IDENTICAL, and refuses
/// `edge_fields_differ` when they are not. Neither silently overwrite nor
/// silently drop: the FIRST edge is kept and deduplication belongs to
/// whoever derives the edges, so a refusal on the second is the spine's
/// side of the same fact. A
/// caller who means to change an edge's fields says so: REMOVE then CREATE,
/// which this walk reads in delta order exactly as the landing applies it.
///
/// Read from the phase-10b snapshot like occupancy. Two statements cannot
/// race the same triple past this check: every `AddEdge`/`RemoveEdge` claims
/// an EXCLUSIVE `LockMode::Write` on `Resource::Edge { src, kind, dst }`
/// (`ladle::compile::claims_for`), so the second waits until the first has
/// landed and then reads its edge here. (The edge SLICE is held by LINK, but
/// the triple is not.) Measured: 0/40 collisions
/// with the lock, 2/40 with the triple's Write claim removed. The landing's
/// `ON CONFLICT DO NOTHING` is therefore a set-semantics no-op for an
/// identical re-add, not the arbiter of a race.
fn check_edge_fields_agree(
    snap: &crate::pool::Snapshot<'_>,
    part: &crate::delta::CompiledPart,
    ordinal: u32,
) -> Result<()> {
    use std::collections::BTreeMap;
    // (src, kind, dst) -> the fields the edge holds at this point of the
    // walk; `None` = absent. Seeded lazily from the snapshot.
    let mut current: BTreeMap<(NodeId, String, QualifiedNodeId), Option<BTreeMap<String, serde_json::Value>>> =
        BTreeMap::new();
    let stored = |src: &NodeId, kind: &str, dst: &QualifiedNodeId| -> Result<Option<BTreeMap<String, serde_json::Value>>> {
        Ok(snap
            .edges_from(src, Some(kind))?
            .into_iter()
            .find(|e| e.dst == *dst)
            .map(|e| e.fields))
    };
    for delta in &part.deltas {
        match delta {
            Delta::AddEdge {
                src,
                kind,
                dst,
                fields,
                ..
            } => {
                let key = (src.clone(), kind.clone(), dst.clone());
                if !current.contains_key(&key) {
                    let seed = stored(src, kind, dst)?;
                    current.insert(key.clone(), seed);
                }
                match current.get(&key).cloned().flatten() {
                    None => {
                        current.insert(key, Some(fields.clone()));
                    }
                    Some(held) if held == *fields => {}
                    Some(held) => {
                        let names: std::collections::BTreeSet<&String> =
                            held.keys().chain(fields.keys()).collect();
                        let differing: Vec<String> = names
                            .into_iter()
                            .filter(|n| held.get(*n) != fields.get(*n))
                            .map(|n| {
                                format!(
                                    "`{n}` held {} offered {}",
                                    held.get(n).map_or("absent".to_string(), |v| v.to_string()),
                                    fields.get(n).map_or("absent".to_string(), |v| v.to_string())
                                )
                            })
                            .collect();
                        return Err(GrammarFault::new(
                            "edge_fields_differ",
                            format!(
                                "edge `{src}` -[{kind}]-> `{dst}` already exists with different fields ({}); an edge is one per (src, kind, dst) and the first stands — REMOVE it and CREATE it to change its fields",
                                differing.join("; ")
                            ),
                        )
                        .refusal(&part.pool, Some(ordinal)));
                    }
                }
            }
            Delta::RemoveEdge { src, kind, dst } => {
                current.insert((src.clone(), kind.clone(), dst.clone()), None);
            }
            _ => {}
        }
    }
    Ok(())
}

/// How many edges would occupy one at-most-one slice once this part landed:
/// the slice as the pool holds it now, with the part's own adds and removes
/// applied IN ORDER (so a destroy-then-create inside one part reads exactly
/// as it will at landing time — the reason the landing check counts after
/// every delta rather than per delta).
///
/// Edges are a SET keyed on (src, kind, dst_pool, dst) — `apply_part`'s
/// `ON CONFLICT DO NOTHING` is what makes that true — so re-adding an edge
/// already present moves nothing here either.
fn projected_occupancy(
    snap: &crate::pool::Snapshot<'_>,
    part: &crate::delta::CompiledPart,
    kind: &str,
    end: &ScalarEnd,
) -> Result<usize> {
    match end {
        ScalarEnd::From(src) => {
            let src = NodeId(src.clone());
            let mut occupants: std::collections::BTreeSet<QualifiedNodeId> = snap
                .edges_from(&src, Some(kind))?
                .into_iter()
                .map(|e| e.dst)
                .collect();
            for delta in &part.deltas {
                match delta {
                    Delta::AddEdge {
                        src: s,
                        kind: k,
                        dst,
                        ..
                    } if *s == src && k == kind => {
                        occupants.insert(dst.clone());
                    }
                    Delta::RemoveEdge {
                        src: s,
                        kind: k,
                        dst,
                    } if *s == src && k == kind => {
                        occupants.remove(dst);
                    }
                    _ => {}
                }
            }
            Ok(occupants.len())
        }
        ScalarEnd::To(pool, dst) => {
            let dst = QualifiedNodeId {
                pool: pool.clone(),
                id: NodeId(dst.clone()),
            };
            let mut occupants: std::collections::BTreeSet<NodeId> = snap
                .edges_to(&dst, Some(kind))?
                .into_iter()
                .map(|e| e.src)
                .collect();
            for delta in &part.deltas {
                match delta {
                    Delta::AddEdge {
                        src,
                        kind: k,
                        dst: d,
                        ..
                    } if k == kind && *d == dst => {
                        occupants.insert(src.clone());
                    }
                    Delta::RemoveEdge {
                        src,
                        kind: k,
                        dst: d,
                    } if k == kind && *d == dst => {
                        occupants.remove(src);
                    }
                    _ => {}
                }
            }
            Ok(occupants.len())
        }
    }
}

/// The frozen prefix of the internal-invariant failure below. It is a
/// CONSTANT so a test can assert on it structurally rather than on prose.
pub const REFUSED_AFTER_DECIDED: &str =
    "SPINE INTERNAL INVARIANT VIOLATED: a pool landing REFUSED a part AFTER the decided flip";

/// A REFUSAL at phase 12 — any refusal — is UNREACHABLE for any statement
/// phases 7 and 10b validated, and it is reported as the crate defect it
/// would be rather than as a caller's refusal.
///
/// It is unreachable because the door judged the same part under the same
/// grammar (phase 7), against the same pin and the same occupancy (phase
/// 10b), while holding the locks that keep the stored state still until
/// phase 13's release — so the only things that could produce a different
/// verdict here are a divergence between the door's checks and the landing's,
/// or a writer that bypassed the lock table. Both are defects of this crate,
/// not of the statement, and neither may be dressed up as a `Refused`: a
/// refusal means "nothing landed", and past the decided flip that is exactly
/// what this build cannot promise (the row flip IS the commit,
/// there is no roll-back of landed parts, and the locks hold to the cleared
/// flip). So it becomes a loud, named `Hard` that says the transaction is
/// stuck for recovery and why.
///
/// It used to catch ONLY the occupancy tag and pass
/// every other landing refusal through as the caller's `Refused` — the lie
/// a moved pin exhibited: the
/// row was committed and the caller was told nothing landed. Every refusal
/// past the decided flip is the same lie; every one is named now. Storage
/// failures (`Hard`, `Busy`) pass through untouched — they are physics, not
/// a verdict on the statement.
fn refusal_after_decided_is_an_invariant_violation(
    e: SpineError,
    seq: Seq,
    ordinal: u32,
) -> SpineError {
    if !matches!(e, SpineError::Refused { .. }) {
        return e;
    }
    SpineError::hard(format!(
        "{REFUSED_AFTER_DECIDED} (seq {seq}, part {ordinal}): {}. The door judged this part \
         under its grammar (phase 7) and against the pool's pin and occupancy under the locks it \
         holds (phase 10b), so either the door's checks and the landing's have drifted or \
         something changed the pool outside the lock table. This transaction is now DECIDED \
         with un-landed parts and its locks held to a cleared flip that will not come; recovery \
         will roll it forward into the same refusal. Fix the crate — this is not the caller's \
         fault and it is not a refusal.",
        e.message()
    ))
}

/// The match phase (phase 6) as a validated snapshot read, with a fresh fuel
/// tank per attempt.
///
/// INTERIOR CHOICE: the fuel ceiling bounds ONE
/// evaluation, not a retry storm. A discarded attempt's fuel is not carried
/// forward, so a contended statement fails as `Busy` — which it is — rather
/// than as `FuelExhausted`, which would blame the statement for the
/// cauldron's traffic. The tank that survives the successful attempt carries
/// on into compilation and the RETURN projection, so one statement's whole
/// evaluation is still bounded by one ceiling.
fn match_phase(
    cauldron: &Cauldron,
    stmt: &Statement,
    params: &crate::envelope::Bindings,
    config: &crate::config::SpineConfig,
    pools: &[PoolName],
    static_claims: &ClaimSet,
) -> Result<(MatchSet, Fuel)> {
    let ceiling = config.fuel_ceiling;
    let mut spent: Option<Fuel> = None;
    let result = read::run_validated(cauldron, pools, config, static_claims, |snaps| {
        let mut fuel = Fuel::new(ceiling);
        let matched = read::run_match(snaps, stmt, params, &mut fuel)?;
        spent = Some(fuel);
        Ok(matched)
    })?;
    let fuel = spent.ok_or_else(|| {
        SpineError::hard("the match phase returned without ever running its queries")
    })?;
    Ok((result.value, fuel))
}

/// The bound identities of a match set, as the row records them:
/// one entry per MATCHed binding the statement names — including a binding
/// that bound zero nodes, whose entry is empty — in match order. Derived
/// from the statement's own binding index rather than from the match set's
/// keys so that "absent" can only ever mean "not recorded" (see
/// [`BoundIdentities`]).
pub(crate) fn bound_of(stmt: &Statement, matched: &MatchSet) -> BoundIdentities {
    let mut out = BoundIdentities::new();
    for name in stmt.matched_bindings() {
        let ids = matched
            .bindings
            .get(&name)
            .map(|rows| {
                rows.iter()
                    .map(|n| QualifiedNodeId {
                        pool: n.pool.clone(),
                        id: n.node.id.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.insert(name, ids);
    }
    out
}

/// The spine-file table that holds each row's bound identities, keyed by
/// sequence number. Owned here and not in `spine_db::SPINE_DDL`
/// only because of the ownership blocker recorded on
/// [`BoundIdentities`]; it may later fold into the payload column.
/// Created on first use, so a spine file written before this table existed
/// needs no migration to be opened.
pub const BOUND_TABLE: &str = "tx_bound";

/// The table's DDL, public so a suite that stages a record by hand (direct
/// SQL on the spine is legal for a test) runs THIS and never a copy.
pub const BOUND_DDL: &str = "CREATE TABLE IF NOT EXISTS tx_bound (
  seq   INTEGER PRIMARY KEY,
  bound TEXT NOT NULL CHECK (json_valid(bound))
)";

/// Record a row's bound identities durably. Written by the door
/// once per attempt, from the match re-validated under the grant, before
/// the decided flip; a fresh sequence number per attempt means never a
/// conflict.
pub(crate) fn record_bound(spine: &SpineDb, seq: Seq, bound: &BoundIdentities) -> Result<()> {
    let conn = spine.conn();
    conn.execute_batch(BOUND_DDL)?;
    let json = serde_json::to_string(bound)
        .map_err(|e| SpineError::hard(format!("bound identities will not serialize: {e}")))?;
    conn.execute(
        "INSERT INTO tx_bound (seq, bound) VALUES (?1, ?2)",
        rusqlite::params![seq.0, json],
    )?;
    Ok(())
}

/// A row's recorded bound identities, or `None` when the row has no record
/// at all (a row appended before records existed, or one staged by hand). `None` is
/// NOT an empty map: a projection that needs a matched binding refuses
/// loudly on a missing record rather than projecting nothing (see
/// [`project_landed`]).
pub(crate) fn read_bound(spine: &SpineDb, seq: Seq) -> Result<Option<BoundIdentities>> {
    use rusqlite::OptionalExtension;
    let conn = spine.conn();
    conn.execute_batch(BOUND_DDL)?;
    let raw: Option<String> = conn
        .query_row(
            "SELECT bound FROM tx_bound WHERE seq = ?1",
            rusqlite::params![seq.0],
            |r| r.get(0),
        )
        .optional()?;
    match raw {
        None => Ok(None),
        Some(s) => serde_json::from_str(&s).map(Some).map_err(|e| {
            SpineError::hard(format!(
                "the bound identities recorded for seq {seq} will not parse: {e}"
            ))
        }),
    }
}

/// THE RETURN PROJECTION, taken from the LANDED state before release (the
/// RETURN clause projects identifiers/revisions/fields from the
/// landed state before release) — and the ONE place it is computed, for
/// both callers:
///
/// - the live door, at phase 13, under the locks it still holds;
/// - recovery's roll-forward, after every part has landed and before the
///   cleared flip, under the SAME locks — which recovery still holds at
///   that point because release is at the cleared flip only.
///
/// Both read the row's own record and nothing else: the statement (parsed —
/// parsing is deterministic and is not matching), its parameters, the
/// compiled payload (for the minted identities and the per-part stamps),
/// and the bound identities (which nodes each MATCH binding bound). Every
/// bound node is re-read from its pool NOW — its revisions and fields are
/// therefore post-landing, exactly as the live door has always projected
/// them — and a ghost is re-resolved from the spine (computed on
/// read; the row it names is an earlier transaction's). A CREATEd binding
/// projects from its CREATE clause and its minted identifier, which is
/// `project_returns`' own frozen rule.
///
/// Two things are LOUD rather than silently empty, because a projection
/// that is quietly missing rows is the one outcome exactly-once's "original
/// outcome" cannot tolerate: a RETURN naming a matched binding the record
/// does not carry (`bound` has no entry — a row written before the record
/// existed, or a record that went missing), and a bound node that no
/// longer reads (nodes are never removed — REMOVE removes edges — so this is
/// a broken invariant, not an empty match). Both are named `Hard`.
pub(crate) fn project_landed(
    cauldron: &Cauldron,
    stmt: &Statement,
    params: &Bindings,
    payload: &CompiledPayload,
    bound: &BoundIdentities,
    fuel: &mut Fuel,
) -> Result<Returned> {
    // Every matched binding the RETURN names must be IN THE RECORD. Checked
    // before any pool is opened, so the refusal names the binding and not a
    // downstream symptom.
    if let Some(ret) = &stmt.returns {
        for item in &ret.items {
            let name = &item.binding().0;
            if payload.minted.contains_key(name) || bound.contains_key(name) {
                continue;
            }
            if stmt.matched_bindings().contains(name) {
                return Err(SpineError::hard(format!(
                    "RETURN names the matched binding `{name}` but the row records no bound \
                     identities for it; the projection cannot be computed without re-matching, \
                     and re-matching is never done. The record is incomplete — a \
                     row written before bound identities were recorded, or a record that has \
                     gone missing — repair it out-of-band"
                )));
            }
            // A binding the statement does not bind at all is the compiler's
            // malformed-statement refusal, raised by `project_returns` below.
        }
    }

    let mut pools: Vec<PoolName> = Vec::new();
    for ids in bound.values() {
        for q in ids {
            if !pools.contains(&q.pool) && !project::is_projected(&q.pool) {
                pools.push(q.pool.clone());
            }
        }
    }
    let mut landed = MatchSet::default();
    if !pools.is_empty() || !bound.is_empty() {
        let open = read::open_pools(cauldron, &pools)?;
        let snaps = read::open_snapshots(&open)?;
        for (binding, ids) in bound {
            let mut fresh = Vec::with_capacity(ids.len());
            for q in ids {
                // A GHOST is re-resolved from the spine, not from a pool:
                // the projection is computed on read, and this
                // commit landed nothing in it — the row it names is an
                // EARLIER transaction's (a statement cannot name its own
                // hash), so what the projection shows is that row as it
                // stands now.
                let record = if project::is_projected(&q.pool) {
                    snaps.resolve_ghost(&q.id)?
                } else {
                    snaps.of(&q.pool)?.node(&q.id)?
                };
                let Some(node) = record else {
                    return Err(SpineError::hard(format!(
                        "the bound node `{}` (binding `{binding}`) no longer reads from its pool; \
                         nodes are never removed, so this is a broken invariant of \
                         the store, not an empty match — the RETURN projection is refused rather \
                         than silently missing a row",
                        q
                    )));
                };
                fresh.push(BoundNode {
                    pool: q.pool.clone(),
                    node,
                });
            }
            landed.bindings.insert(binding.clone(), fresh);
        }
    }
    compile::project_returns(stmt, params, &landed, payload, fuel)
}

/// What a duplicate hash identity answers (exactly-once): the ORIGINAL
/// outcome, from the record alone, with no new
/// machinery and nothing landed a second time.
/// `None` means "this identity has NO verdict yet" — every row it has is an
/// aborted attempt, an abort landed nothing, and the caller may proceed with
/// a FRESH attempt. That is the dedup rule made concrete: dedup keys
/// on "did this LAND", never on "was this attempted".
///
/// # The RECOVERED landing — no longer a special case
///
/// A row that RECOVERY rolled forward used to be unanswerable here: its
/// result column held recovery's own marker instead of a projection, and
/// the door refused it by name (`RecoveredCommitReplyUnruled`, since
/// withdrawn). That variant, and the
/// `is_recovery_record` marker test beside this function, are GONE: recovery
/// now records which nodes each binding bound and projects RETURN at its own
/// cleared flip through the very same [`project_landed`] the live door runs,
/// so a recovered row's `result` IS a projection, and it is answered by the
/// arm below like any other landed row. Whether a row was recovered is an
/// audit fact on the row (`TxRow::recovered`), never inferred from the shape
/// of `result`. The cost is a record added to every row.
fn dedup_outcome(row: &TxRow) -> Option<Result<Outcome>> {
    if row.decided && row.cleared {
        // LANDED. The original outcome, replayed from the row alone —
        // recovered or not.
        let returned = match &row.result {
            Some(v) => match serde_json::from_value(v.clone()) {
                Ok(r) => r,
                Err(e) => {
                    // GENUINE corruption, reported as such. A recovered row
                    // never lands here: its result is a real projection.
                    return Some(Err(SpineError::hard(format!(
                        "transaction {} stored a RETURN projection that will not parse: {e}",
                        row.hash
                    ))))
                }
            },
            None => Returned::default(),
        };
        return Some(Ok(Outcome::Committed {
            tx: row.hash,
            seq: row.seq,
            returned,
            deduplicated: true,
        }));
    }
    if !row.cleared {
        // IN FLIGHT — decided and landing, or appended and not yet decided.
        // Its outcome does not exist yet and the row says so; `Busy` is
        // wait-and-re-ask, which is exactly what the caller should do. It is
        // never a second landing: only one attempt per identity may be
        // un-cleared at a time (`idx_tx_live`), and only one may ever be
        // decided (`idx_tx_committed`).
        return Some(Err(SpineError::Busy {
            message: format!(
                "hash identity {} is already in flight at seq {} (attempt {}, {}); its outcome \
                 does not exist yet",
                row.hash,
                row.seq,
                row.attempt,
                if row.decided {
                    "decided, landing"
                } else {
                    "appended, not yet decided"
                }
            ),
        }));
    }
    // Cleared and never decided: an ABORTED attempt. It landed nothing, so
    // it is not an answer to "did this land" — the caller gets a fresh
    // attempt with a fresh sequence number.
    None
}

/// The claims a stale match contests (the stale-match shape): what
/// the re-derived claim set and the granted one disagree about. When they
/// agree exactly — the payload moved without moving a claim — the whole
/// re-derived set is named rather than an empty list, because a witness that
/// names nothing is not a witness.
///
/// The HELD set is derived from the PRE-GRANT match set and the payload the
/// row records — the pair phase 9 enrolled from — not from `re_matched` with
/// the old payload: a `SET` of `state` on a
/// node the re-match no longer binds would otherwise fall through to
/// `claims_for`'s over-restricted fallback (`link` on every state slice of
/// the pool) and the witness would name slices the door never held. With
/// the true pair the fallback is reached by no door path at all.
fn contested_claims(
    cauldron: &Cauldron,
    stmt: &Statement,
    matched: &MatchSet,
    re_matched: &MatchSet,
    re_payload: &CompiledPayload,
    payload: &CompiledPayload,
) -> Result<Vec<Claim>> {
    let lookup = cauldron.grammar_lookup();
    let fresh = compile::claims_for(stmt, re_matched, re_payload, &lookup)?;
    let held = compile::claims_for(stmt, matched, payload, &lookup)?;
    let mut out: Vec<Claim> = fresh
        .iter()
        .filter(|c| !held.iter().any(|h| h == *c))
        .cloned()
        .collect();
    for c in held.iter() {
        if !fresh.iter().any(|f| f == c) {
            out.push(c.clone());
        }
    }
    if out.is_empty() {
        out = fresh.iter().cloned().collect();
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// STAGING A CHILD
// ---------------------------------------------------------------------------

/// The match phase AGAINST THE VIEW (staging step 5): one snapshot per
/// touched pool with the tranche's overlay on top, the matcher run once,
/// the hops' traversals handed back for the claim derivation.
fn view_match(
    cauldron: &Cauldron,
    view: &View,
    pools: &[PoolName],
    stmt: &Statement,
    params: &Bindings,
    fuel: &mut Fuel,
) -> Result<(MatchSet, Vec<HopTrace>)> {
    let open = read::open_pools(cauldron, pools)?;
    let snaps = read::open_snapshots_over(&open, view)?;
    read::run_match_traced(&snaps, stmt, params, fuel)
}

/// A child's COMPLETE claim set (all eleven rows of the claim table):
/// what the ordinary door derives for a write (`claims_for` — the
/// bound bodies and the write side, including the state-slice `link`s a
/// state change or a create with an initial state takes), the static
/// population reads (`claim_footprint` — a scanning head's type slice, a
/// create's link, a count's slice), and the tranche's three read-side
/// additions (`tranche_read_claims` — the state slice a filtered scan read,
/// the by-identifier head that bound nothing, the traversed edge slices).
fn child_claims(
    stmt: &Statement,
    params: &Bindings,
    matched: &MatchSet,
    trace: &[HopTrace],
    payload: &CompiledPayload,
    static_claims: &ClaimSet,
    lookup: &dyn Fn(&PoolName) -> Option<crate::schema_def::PoolGrammar>,
) -> Result<ClaimSet> {
    let mut claims = compile::claims_for(stmt, matched, payload, lookup)?;
    for c in static_claims.iter() {
        claims.insert(c.clone());
    }
    for c in compile::tranche_read_claims(stmt, params, trace)?.iter() {
        claims.insert(c.clone());
    }
    Ok(claims)
}

/// `minted` as a child's reply carries it: binding → `<pool>/<id>`.
fn minted_strings(payload: &CompiledPayload) -> std::collections::BTreeMap<String, String> {
    payload
        .minted
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string()))
        .collect()
}

/// The reply's `changed`: true when the payload carries a node creation, a field
/// set or a seal — a body write; false for a statement that only adds or
/// removes edges (the spine does not report whether an edge set moved).
fn payload_changes_a_body(payload: &CompiledPayload) -> bool {
    payload.parts.iter().any(|p| {
        p.deltas.iter().any(|d| {
            matches!(
                d,
                Delta::CreateNode { .. } | Delta::SetField { .. } | Delta::Seal { .. }
            )
        })
    })
}

/// STAGE ONE CHILD (eleven steps), under the tranche's
/// mutex (the caller's, `Cauldron::tranche_stage`), against the view. The
/// ordinary pipeline through compile, with substitutions, staging instead
/// of appending a pending row:
///
///  1. the halt gate and the quiesce gate (a standing quiesce refuses
///     staging with the spine's quiesced `busy`);
///  2. the envelope version;
///  3. the hash identity; dedup against THREE places — a landed row
///     (its original outcome, `deduplicated`, nothing staged), an in-flight
///     row (`busy`), this tranche's own children (the original ordinal,
///     `deduplicated`);
///  4. parse, validate, the write form — `prepare_statement`, the ordinary
///     door's own phase 5 — AND the tranche-only projected check (the
///     projected namespace under a tranche): a `state`
///     entry on a projected head, a `WHERE`/`EXPECT` over a projected
///     binding, a `RETURN` of a projected field other than `id` are refused
///     `unimplemented_form match:projected-pattern`, and every projected
///     head must name a row that EXISTS (`ghost_target_unknown`, extended
///     from `CREATE`-edge targets to every projected head);
///  5. the match phase runs AGAINST THE VIEW;
///  6. compile (provisional identifiers minted as always); THE FLAG RULE
///     per child, with the child's own initiator; pins and occupancy
///     against the view; `validate` mode stops here and answers;
///  7. THE TOUCHED SET GROWS: the child's complete claims are
///     enrolled under the TRANCHE's `seq` and granted all-or-nothing
///     (`locks::acquire_for_tranche` — rules 1 and 2 inside; rule 3 is the
///     mutex we run under), waiting up to the ceiling with every poll a
///     heartbeat; on the ceiling the queued rows are withdrawn and the
///     child is refused `busy` naming the holders, nothing staged;
///  8. under the granted claims the match is re-run and the payload
///     re-compiled; if it differs, step 5 is repeated with the larger claim
///     set, bounded by `restart_bound`, refusing `busy` past it;
///  9. STAGE DURABLY, one immediate spine transaction: re-read the state
///     (`landing` → release the rows this call granted, `refused
///     tranche_past_rollback`; terminal → the same release, `refused
///     tranche_terminal`), the size cap, the `tranche_children` row
///     (ordinal, hash, envelope, payload, bound identities, `staged`),
///     `staged + 1`, `open → staging`;
/// 10. apply the child's parts to the overlay;
/// 11. answer with the child's reply.
///
/// NOTHING TOUCHES A POOL FILE: a staged child is a spine row and an entry
/// in the in-memory overlay. Every refusal after step 7 releases exactly the
/// lock rows THIS CALL enrolled (an earlier child's rows are the tranche's
/// and stay).
pub(crate) fn stage(cauldron: &Cauldron, ctx: &mut StageCtx<'_>, envelope: Envelope) -> Result<ChildOutcome> {
    let config = cauldron.config().clone();
    let mut spine = cauldron.spine()?;

    // ---- 1. the halt gate, the quiesce gate, the fault-seam gate ----
    if let Some(reason) = spine.halted()? {
        return Err(SpineError::Halted { reason });
    }
    let standing = spine.quiescers()?;
    if !standing.is_empty() {
        return Err(recover::quiesced_busy(&standing));
    }
    fault::gate()?;

    // ---- 2. the envelope format version ----
    envelope.check_format_version()?;
    // A child is a type-one or type-two step that writes as
    // itself; `type_three` is the ANCHOR row's provenance alone.
    if let Initiator::Ripple {
        kind: RippleKind::TypeThree,
        ..
    } = &envelope.provenance.initiator
    {
        return Err(SpineError::usage(
            "a tranche child carries its own type-one or type-two ripple provenance; `type_three` \
             is the anchor row's and never a child's",
        ));
    }

    // ---- 3. the hash identity, and dedup in three places ----
    // Not in validate mode, as the ordinary door (the dry run is judged on
    // the statement alone).
    let hash = envelope.hash_identity()?;
    if envelope.mode == Mode::Commit {
        if let Some(row) = spine.find_by_hash(&hash)? {
            match dedup_outcome(&row) {
                // LANDED elsewhere: the step already happened.
                Some(Ok(Outcome::Committed { tx, seq, .. })) => {
                    return Ok(ChildOutcome::LandedElsewhere {
                        tx,
                        seq,
                        minted: minted_strings(&row.payload),
                    })
                }
                Some(Ok(other)) => {
                    return Err(SpineError::hard(format!(
                        "dedup answered a non-commit outcome for a landed row: {other:?}"
                    )))
                }
                // IN FLIGHT elsewhere: `busy`, as today.
                Some(Err(e)) => return Err(e),
                // Every attempt aborted: not an answer. Proceed.
                None => {}
            }
        }
        // THIS TRANCHE's children.
        let own: Option<(i64, String)> = spine
            .conn()
            .query_row(
                "SELECT ordinal, payload FROM tranche_children
                 WHERE tranche_id = ?1 AND tranche_attempt = ?2 AND hash = ?3",
                rusqlite::params![ctx.tranche, ctx.attempt, hash.to_hex()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((ordinal, payload)) = own {
            let payload: CompiledPayload = serde_json::from_str(&payload).map_err(|e| {
                SpineError::hard(format!(
                    "tranche `{}` attempt {} child {ordinal}: unparseable payload: {e}",
                    ctx.tranche, ctx.attempt
                ))
            })?;
            return Ok(ChildOutcome::Deduplicated {
                tx: hash,
                ordinal: ordinal as u32,
                minted: minted_strings(&payload),
            });
        }
    }

    // ---- 4. parse, validate, the write form (the ordinary door's phase 5) ----
    let Prepared {
        stmt,
        pools,
        static_claims,
    } = prepare_statement(cauldron, &spine, &envelope)?;
    // ...and the TRANCHE-ONLY projected check (the projected
    // namespace under a tranche): a tranche cannot restart its landing and
    // no lock row holds a spine row's lifecycle still, so what a child may
    // rely on in the projected namespace is narrower than at the ordinary
    // door — by shape (`check_projected_under_tranche`), and by existence
    // (`guard_projected_heads`: every projected head names a row).
    compile::check_projected_under_tranche(&stmt)?;
    guard_projected_heads(&spine, &stmt, &envelope.bindings)?;
    let lookup = cauldron.grammar_lookup();
    let kinds = cauldron.kind_lookup();
    let scalar = |pool: &PoolName, kind: &str, direction: Direction| -> bool {
        cauldron
            .grammar(pool)
            .map(|g| g.is_scalar_slice(kind, direction))
            .unwrap_or(false)
    };
    let compile_now = |matched: &MatchSet, fuel: &mut Fuel| -> Result<CompiledPayload> {
        let mut c = CompileCtx {
            grammars: &lookup,
            pools_of_kind: &kinds,
            fuel,
        };
        compile::compile(&stmt, &envelope.bindings, matched, &hash, &mut c)
    };

    // ---- 5. the match phase, against the view ----
    let mut fuel = Fuel::new(config.fuel_ceiling);
    let (mut matched, mut trace) =
        view_match(cauldron, ctx.view, &pools, &stmt, &envelope.bindings, &mut fuel)?;

    // ---- 6. compile; the flag rule per child; pins and occupancy against the view ----
    let mut payload = compile_now(&matched, &mut fuel)?;
    // THE FLAG RULE: per child, with the CHILD's own initiator and
    // payload, as it is submitted — so a type-one child setting
    // `meta.blocking` is refused `flag_not_writable` here and the tranche
    // stays open. The type-three identity grants nothing: the anchor has no
    // deltas and this check never consults it.
    crate::flags::check_flag_grants(&envelope.provenance.initiator, &payload, &matched)?;
    check_pins_and_occupancy_over(cauldron, &payload, Some(ctx.view))?;
    if envelope.mode == Mode::Validate {
        return Ok(ChildOutcome::Validated { tx: hash });
    }

    // ---- 7 and 8. the touched set grows; the re-match under the grant ----
    let claimant = TrancheClaimant {
        now_ms: ctx.now_ms,
        timeout_ms: ctx.timeout_ms,
        started: std::time::Instant::now(),
    };
    let mut claims = child_claims(&stmt, &envelope.bindings, &matched, &trace, &payload, &static_claims, &lookup)?;
    // The rows THIS CALL enrolls, released on every refusal from here on.
    let mut mine: Vec<Claim> = Vec::new();
    let staged = (|| -> Result<(u32, CompiledPayload)> {
        let mut restarts: u32 = 0;
        loop {
            locks::check_link_claims(&claims, &scalar)?;
            // Rules 1 and 2 live inside; rule 3 is the mutex we hold.
            let fresh = locks::acquire_for_tranche(&spine, ctx.seq, &claims, &config, &claimant)?;
            mine.extend(fresh);
            // 8. UNDER THE GRANTED CLAIMS: the same match, the same
            // compiler, against the view — and the CLAIMS DERIVED AGAIN
            // from what that match read. The comparison is on the claim
            // set, never on the payload alone (`read::read_under`'s rule):
            // a `MATCH` binding that is read and not
            // written leaves no mark on the payload, so a re-match that
            // bound MORE under the grant would otherwise be recorded in
            // `bound` with bodies the tranche never held — the claim table
            // broken, and a "defective claim derivation" born here.
            let mut check_fuel = Fuel::new(config.fuel_ceiling);
            let (re_matched, re_trace) =
                view_match(cauldron, ctx.view, &pools, &stmt, &envelope.bindings, &mut check_fuel)?;
            let re_payload = compile_now(&re_matched, &mut check_fuel)?;
            let re_claims = child_claims(
                &stmt,
                &envelope.bindings,
                &re_matched,
                &re_trace,
                &re_payload,
                &static_claims,
                &lookup,
            )?;
            if re_claims == claims && re_payload == payload {
                matched = re_matched;
                trace = re_trace;
                break;
            }
            // THE VIEW MOVED between the snapshot and the grant: a foreign
            // writer landed on something this child read before the read
            // was held — the match binds differently, or the payload it
            // compiles to differs. Step 5 again, with the claim set the
            // match under the grant implies (what was granted stays
            // granted under the tranche's seq: the touched set grows).
            restarts += 1;
            if restarts > config.restart_bound {
                return Err(SpineError::Busy {
                    message: format!(
                        "staging gave up after {restarts} restarts: the view moved under every \
                         complete claim set of this child (hash {hash}) — the lock design's starvation \
                         residual, bounded by restart_bound {}; nothing staged",
                        config.restart_bound
                    ),
                });
            }
            payload = re_payload;
            matched = re_matched;
            trace = re_trace;
            crate::flags::check_flag_grants(&envelope.provenance.initiator, &payload, &matched)?;
            claims = re_claims;
        }
        // Pins and occupancy, now under the grant (the ordinary door's 10b).
        check_pins_and_occupancy_over(cauldron, &payload, Some(ctx.view))?;

        // ---- 9. STAGE DURABLY, one immediate spine transaction ----
        let bound = bound_of(&stmt, &matched);
        let bound_json = serde_json::to_string(&bound)
            .map_err(|e| SpineError::hard(format!("bound identities will not serialize: {e}")))?;
        let envelope_json = serde_json::to_string(&envelope)
            .map_err(|e| SpineError::hard(format!("the child's envelope will not serialize: {e}")))?;
        let payload_json = serde_json::to_string(&payload)
            .map_err(|e| SpineError::hard(format!("the child's payload will not serialize: {e}")))?;
        let tx = spine
            .conn_mut()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some(reason) = {
            let (halted, reason): (i64, Option<String>) = tx.query_row(
                "SELECT halted, halted_reason FROM spine_state WHERE singleton = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            (halted != 0).then(|| reason.unwrap_or_else(|| "halted with no recorded reason".into()))
        } {
            return Err(SpineError::Halted { reason });
        }
        // The state, re-read in the staging transaction (step 9; the
        // second legality check's guarantee): only open or staging proceeds.
        crate::tranche::claim_state_check(&tx, ctx.seq)?;
        let staged: i64 = tx.query_row(
            "SELECT staged FROM tranches WHERE seq = ?1",
            rusqlite::params![ctx.seq.0],
            |r| r.get(0),
        )?;
        let ordinal = u32::try_from(staged).map_err(|_| SpineError::hard("staged count overflows u32"))?;
        // The size cap: the `max`-th child is refused `tranche_full`.
        if ordinal >= ctx.max_children {
            return Err(SpineError::refuse(Witness::TrancheFull {
                tranche: ctx.tranche.clone(),
                attempt: ctx.attempt,
                max: ctx.max_children,
            }));
        }
        tx.execute(
            "INSERT INTO tranche_children
               (tranche_id, tranche_attempt, ordinal, hash, envelope, payload, bound, state, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'staged', NULL)",
            rusqlite::params![
                ctx.tranche,
                ctx.attempt,
                ordinal,
                hash.to_hex(),
                envelope_json,
                payload_json,
                bound_json
            ],
        )?;
        let flipped = tx.execute(
            "UPDATE tranches SET staged = staged + 1, state = 'staging'
             WHERE seq = ?1 AND state IN ('open','staging')",
            rusqlite::params![ctx.seq.0],
        )?;
        if flipped != 1 {
            return Err(SpineError::hard(format!(
                "tranche `{}` attempt {}: the state moved inside the staging transaction",
                ctx.tranche, ctx.attempt
            )));
        }
        tx.commit()?;
        Ok((ordinal, payload))
    })();
    let (ordinal, payload) = match staged {
        Ok(v) => v,
        Err(e) => {
            // Whatever refused this child after its claims were granted —
            // rule 2 at step 9, the size cap, a moved pin, the restart
            // bound — the rows THIS CALL enrolled go; the tranche's earlier
            // rows stay (step 9: release the rows this call granted).
            locks::release_claims(&spine, ctx.seq, &mine)?;
            return Err(e);
        }
    };

    // ---- 10. apply the child's parts to the overlay ----
    // The row is durable; a failure here leaves the view behind the record,
    // and the caller drops it so the next act rebuilds from the rows.
    if let Err(e) = ctx.view.apply_child(cauldron, &hash, &payload) {
        ctx.view_dirty = true;
        return Err(e);
    }

    // ---- 11. the reply ----
    Ok(ChildOutcome::Staged {
        tx: hash,
        ordinal,
        minted: minted_strings(&payload),
        changed: payload_changes_a_body(&payload),
    })
}

// ---------------------------------------------------------------------------
// THE LANDER
// ---------------------------------------------------------------------------

/// One staged child as the lander reads it off `tranche_children`.
struct StagedChild {
    ordinal: u32,
    hash: TxHash,
    envelope: Envelope,
    payload: CompiledPayload,
    bound: BoundIdentities,
    state: String,
    seq: Option<i64>,
}

fn load_children(spine: &SpineDb, tranche: &str, attempt: u32) -> Result<Vec<StagedChild>> {
    let mut stmt = spine.conn().prepare(
        "SELECT ordinal, hash, envelope, payload, bound, state, seq FROM tranche_children
         WHERE tranche_id = ?1 AND tranche_attempt = ?2 ORDER BY ordinal",
    )?;
    let rows = stmt.query_map(rusqlite::params![tranche, attempt], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, Option<i64>>(6)?,
        ))
    })?;
    let mut out = Vec::new();
    for r in rows {
        let (ordinal, hash, envelope, payload, bound, state, seq) = r?;
        let torn = |what: &str, e: &dyn std::fmt::Display| {
            SpineError::hard(format!(
                "tranche `{tranche}` attempt {attempt} child {ordinal}: unparseable {what}: {e}"
            ))
        };
        let hash = TxHash::from_hex(&hash).ok_or_else(|| torn("hash", &hash))?;
        let envelope: Envelope = serde_json::from_str(&envelope).map_err(|e| torn("envelope", &e))?;
        let payload: CompiledPayload = serde_json::from_str(&payload).map_err(|e| torn("payload", &e))?;
        let bound: BoundIdentities = match bound {
            None => BoundIdentities::new(),
            Some(b) => serde_json::from_str(&b).map_err(|e| torn("bound identities", &e))?,
        };
        out.push(StagedChild {
            ordinal: ordinal as u32,
            hash,
            envelope,
            payload,
            bound,
            state,
            seq,
        });
    }
    Ok(out)
}

/// A file held past the ceiling is a STOP, not a failure: the
/// scheduler retries. Anything else passes through.
fn stop_if_busy(e: SpineError, why: &'static str, at_child: Option<u32>) -> std::result::Result<LandingStop, SpineError> {
    match e {
        SpineError::Busy { message } => Ok(LandingStop {
            why,
            at_child,
            colliding_seq: None,
            message,
        }),
        other => Err(other),
    }
}

/// THE LANDER, run by the scheduler's lander thread
/// under the tranche's mutex (`tranche::run_lander`), never by the `commit`
/// request. Everything it needs is in the spine, so a holder gone after its
/// flip, a dropped connection and a daemon restart all leave a landing
/// this finishes without anyone. Idempotent by the record at every step:
/// a retry from any stop re-runs exactly what is not yet done.
///
/// 0. Exit recording nothing unless the tranche is `landing`.
/// 1. UNDECIDED (`tranches.decided = 0`):
///    - **Re-validation**, child by child in ordinal order against the
///      view as it stood for that child (children `0 … k−1` overlaid, built
///      fresh here): the same gates, the same match, the same compiler,
///      against the same hash, so the provisional ids come out identical;
///      the payload compared BYTE FOR BYTE with the staged one; pins and
///      occupancy re-checked against the view. Under the claims the tranche
///      already holds — no new claim, no window check, no wait. The flag rule
///      is not re-run (a static fact of the payload). A mismatch or a refusal
///      FAILS THE TRANCHE WHOLE (`tranche::fail_landing`) before
///      anything is decided.
///    - **The decision**, one immediate transaction: own it
///      conditionally (zero rows → already decided, fall through to 2); for
///      every child hash, the foreign rows — un-cleared → STOP
///      `child_in_flight {colliding_seq}`; landed with a byte-identical
///      payload and a child that minted nothing → `landed_elsewhere`
///      (`deduplicated: true`); landed otherwise → FAIL
///      `tranche_landing_refused {why: child_landed_elsewhere}`; number every
///      provisional of every landing child through ONE map
///      (`spine_db::mint_serials`), rewriting each child's payload, bound
///      identities and the lock rows under the tranche's `seq`, and storing
///      the map as `renumbered`; RESERVE one undecided `transactions` row per
///      landing child — its own hash, provenance, statement, bindings, the
///      numbered payload, `touched`, and the five tranche columns
///      (`ordinal`, `index`, `of`) — with its `tx_parts` (`landed = 0`)
///      and `tx_bound`; mark each staging row `reserved` (or
///      `landed_elsewhere`) with its row's `seq`.
/// 2. **Landing**, each reserved child in ordinal order, one commit
///    each: the decided flip (serial numbering finds nothing to do), each
///    part applied to its pool as one atomic transaction behind the
///    `applied_parts` marker, each part's landed flag (the last stamps
///    `committed_at`, ascending across children), RETURN projected from the
///    row's own record, the row cleared. Premises are NOT re-checked
///    and the child's clear RELEASES NO LOCK: its claims are the
///    tranche's, under the anchor's `seq`. A pool file held past the ceiling
///    → STOP `file_held {at_child}`; any refusal here is an invariant
///    violation named as such.
/// 3. **Clearing** (`tranche::clear_landed`).
///
/// THE INTERNAL PATH: nothing here calls `execute`,
/// `append_or_existing` or `locks::acquire`. The children land UNDER THE
/// ANCHOR'S lock rows; no child row's `seq` ever appears in `locks`, and
/// the tranche gate exempts the landing by the anchor's `seq`. A child whose
/// landing would need a lock row the tranche does not hold is a claim-derivation
/// defect to report, never a fresh acquire.
///
/// The four seam calls (`fault::at_lander_stage`) sit at `before-decision`,
/// `after-decision`, `between-children` and `before-clear`: the crash
/// tests kill a lander there and prove the `landing` rows.
pub(crate) fn land(
    cauldron: &Cauldron,
    slot: &mut Slot,
    tranche: &str,
    attempt: u32,
    now_ms: i64,
) -> Result<LandOutcome> {
    let config = cauldron.config().clone();
    let mut spine = cauldron.spine()?;
    if let Some(reason) = spine.halted()? {
        return Err(SpineError::Halted { reason });
    }
    // ---- 0. the state, re-read under the mutex ----
    let (state, decided, anchor_seq): (String, i64, i64) = match spine
        .conn()
        .query_row(
            "SELECT state, decided, seq FROM tranches WHERE id = ?1 AND attempt = ?2",
            rusqlite::params![tranche, attempt],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
    {
        Some(row) => row,
        None => return Ok(LandOutcome::NotLanding),
    };
    if state != "landing" {
        return Ok(LandOutcome::NotLanding);
    }
    let anchor = Seq(anchor_seq);
    let mut children = load_children(&spine, tranche, attempt)?;

    if decided == 0 {
        // [lander stage: before the decision]
        fault::at_lander_stage(LanderStage::BeforeDecision)?;

        // ---- 1a. RE-VALIDATION ----
        let lookup = cauldron.grammar_lookup();
        let kinds = cauldron.kind_lookup();
        let mut view = View::empty();
        for child in &children {
            let refused = |witnesses: Vec<Witness>| Witness::TrancheLandingRefused {
                tranche: tranche.to_string(),
                attempt,
                ordinal: child.ordinal,
                child_tx: child.hash.to_hex(),
                why: "revalidation".to_string(),
                witnesses,
                colliding_seq: None,
            };
            let outcome = (|| -> Result<Option<Witness>> {
                let Prepared { stmt, pools, .. } = prepare_statement(cauldron, &spine, &child.envelope)?;
                compile::check_projected_under_tranche(&stmt)?;
                let mut fuel = Fuel::new(config.fuel_ceiling);
                let (matched, _trace) =
                    view_match(cauldron, &view, &pools, &stmt, &child.envelope.bindings, &mut fuel)?;
                let re_payload = {
                    let mut c = CompileCtx {
                        grammars: &lookup,
                        pools_of_kind: &kinds,
                        fuel: &mut fuel,
                    };
                    compile::compile(&stmt, &child.envelope.bindings, &matched, &child.hash, &mut c)?
                };
                if re_payload != child.payload {
                    return Ok(Some(refused(vec![])));
                }
                check_pins_and_occupancy_over(cauldron, &child.payload, Some(&view))?;
                Ok(None)
            })();
            let witness = match outcome {
                Ok(None) => None,
                Ok(Some(w)) => Some(w),
                // The statement no longer judges clean against the view:
                // that is a mismatch, carrying the child's own witnesses.
                Err(SpineError::Refused { witnesses }) => Some(refused(witnesses)),
                Err(e) => {
                    return match stop_if_busy(e, "file_held", None) {
                        Ok(stop) => Ok(LandOutcome::Stopped(stop)),
                        Err(e) => Err(e),
                    }
                }
            };
            if let Some(w) = witness {
                drop(spine);
                return if crate::tranche::fail_landing(cauldron, slot, tranche, attempt, &w, now_ms)? {
                    Ok(LandOutcome::Failed)
                } else {
                    Ok(LandOutcome::NotLanding)
                };
            }
            view.apply_child(cauldron, &child.hash, &child.payload)?;
        }
        drop(view);

        // ---- 1b. THE DECISION, one immediate transaction ----
        let decision = {
            let tx = spine
                .conn_mut()
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            // A STANDING QUIESCE POSTPONES THE DECISION (this covers the
            // window between `commit`'s flip and this transaction). The
            // decision reserves child rows and the landing writes pools —
            // exactly what a backup taken on `quiesce()`'s assurance must
            // not contain half-done. Read INSIDE the decision's own
            // immediate transaction, as the append gate reads it, so the
            // two writes serialise: the quiescer commits first and this
            // decision stops `quiesced` (the scheduler retries after the
            // backoff, by which time `resume` has lifted it), or the
            // decision commits first and the quiesce's drain loop sees the
            // anchor and the reserved rows and waits for them. A quiesce
            // can never drain while a tranche is open anyway (the
            // anchor); this keeps the reserved rows out of the window too.
            let standing: i64 = tx.query_row("SELECT COUNT(*) FROM quiescers", [], |r| r.get(0))?;
            if standing != 0 {
                let q = crate::spine_db::quiescers_in(&tx)?;
                drop(tx);
                return Ok(LandOutcome::Stopped(LandingStop {
                    why: "quiesced",
                    at_child: None,
                    colliding_seq: None,
                    message: format!(
                        "the landing decision is postponed while a quiesce stands ({}); the \
                         scheduler retries once it is resumed",
                        q.iter().map(|q| format!("{} since {}", q.holder, q.since)).collect::<Vec<_>>().join("; ")
                    ),
                }));
            }
            let owned = tx.execute(
                "UPDATE tranches SET decided = 1
                 WHERE id = ?1 AND attempt = ?2 AND state = 'landing' AND decided = 0",
                rusqlite::params![tranche, attempt],
            )?;
            if owned != 1 {
                // Already decided (a second process; unreachable within one
                // daemon by the mutex) — or no longer landing. Roll back and
                // continue from the landing as the retry would, or exit.
                let (state, decided): (String, i64) = tx.query_row(
                    "SELECT state, decided FROM tranches WHERE id = ?1 AND attempt = ?2",
                    rusqlite::params![tranche, attempt],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                drop(tx);
                if state != "landing" || decided == 0 {
                    return Ok(LandOutcome::NotLanding);
                }
                Decision::AlreadyMade
            } else {
                let d = decide(&tx, tranche, attempt, anchor, &mut children)?;
                if matches!(d, Decision::Made) {
                    tx.commit()?;
                } else {
                    drop(tx);
                }
                d
            }
        };
        match decision {
            Decision::Made | Decision::AlreadyMade => {}
            Decision::Stop(stop) => return Ok(LandOutcome::Stopped(stop)),
            Decision::Fail(w) => {
                drop(spine);
                return if crate::tranche::fail_landing(cauldron, slot, tranche, attempt, &w, now_ms)? {
                    Ok(LandOutcome::Failed)
                } else {
                    Ok(LandOutcome::NotLanding)
                };
            }
        }
        // [lander stage: after the decision]
        fault::at_lander_stage(LanderStage::AfterDecision)?;
        children = load_children(&spine, tranche, attempt)?;
    }

    // ---- 2. LANDING: each reserved child, one commit each, in order ----
    let mut first = true;
    for child in &children {
        if child.state == crate::tranche::CHILD_LANDED_ELSEWHERE {
            continue;
        }
        let seq = Seq(child.seq.ok_or_else(|| {
            SpineError::hard(format!(
                "tranche `{tranche}` attempt {attempt} is decided but child {} ({}) names no reserved row",
                child.ordinal, child.state
            ))
        })?);
        let row = spine.row_at(seq)?;
        if row.cleared {
            if !row.decided {
                return Err(SpineError::hard(format!(
                    "SPINE INTERNAL INVARIANT VIOLATED: tranche `{tranche}` attempt {attempt} child {} \
                     (seq {seq}) is cleared and un-decided — a reserved child row was aborted under \
                     the landing",
                    child.ordinal
                )));
            }
            mark_child_landed(&spine, tranche, attempt, child.ordinal)?;
            first = false;
            continue;
        }
        if !first {
            // [lander stage: between two children]
            fault::at_lander_stage(LanderStage::BetweenChildren)?;
        }
        first = false;
        if !row.decided && !spine.try_mark_decided(seq)? {
            return Err(SpineError::hard(format!(
                "SPINE INTERNAL INVARIANT VIOLATED: tranche `{tranche}` attempt {attempt} child {} \
                 (seq {seq}) refused the decided flip: its reserved row was aborted under the landing",
                child.ordinal
            )));
        }
        let row = spine.row_at(seq)?;
        let payload = &row.payload;
        for (ordinal, part) in payload.parts.iter().enumerate() {
            let part_ordinal = u32::try_from(ordinal)
                .map_err(|_| SpineError::usage("a statement cannot carry more parts than u32::MAX"))?;
            if part_ordinal > 0 {
                // [tranche stage: between a child's parts] — one part of
                // this child applied and flagged, the next not begun.
                fault::at_tranche_stage(crate::tranche::TrancheStage::BetweenChildParts)?;
            }
            let landing = wait_turn(&config, "the pool landing", || {
                let mut pool = cauldron.open_pool(&part.pool)?;
                pool.apply_part(&row.hash, part_ordinal, part)
            });
            if let Err(e) = landing {
                return match stop_if_busy(e, "file_held", Some(child.ordinal)) {
                    Ok(stop) => Ok(LandOutcome::Stopped(stop)),
                    Err(e) => Err(refusal_after_decided_is_an_invariant_violation(e, seq, part_ordinal)),
                };
            }
            if let Err(e) = wait_turn(&config, "the part's landed flip", || {
                spine.mark_part_landed(seq, part_ordinal)
            }) {
                return match stop_if_busy(e, "file_held", Some(child.ordinal)) {
                    Ok(stop) => Ok(LandOutcome::Stopped(stop)),
                    Err(e) => Err(e),
                };
            }
        }
        // RETURN from the row's own record, under the
        // tranche's locks, still held.
        let stmt = crate::ladle::parse::parse(&row.statement)?;
        let bound = read_bound(&spine, seq)?.ok_or_else(|| {
            SpineError::hard(format!(
                "tranche `{tranche}` attempt {attempt} child {} (seq {seq}) has no bound identities \
                 recorded at its reservation",
                child.ordinal
            ))
        })?;
        let mut fuel = Fuel::new(config.fuel_ceiling);
        let returned = project_landed(cauldron, &stmt, &row.bindings, payload, &bound, &mut fuel)?;
        let result = serde_json::to_value(&returned)
            .map_err(|e| SpineError::hard(format!("RETURN projection will not serialize: {e}")))?;
        // [tranche stage: before a child's clear] — every part of this child
        // applied and flagged, its row not yet cleared: the point at which a
        // retry re-applies the parts and the pool's `applied_parts` marker
        // is what keeps each from landing twice.
        fault::at_tranche_stage(crate::tranche::TrancheStage::BeforeChildClear)?;
        // The cleared flip. NO lock release: the child holds none of its
        // own — the tranche's rows go at the clearing.
        if let Err(e) = wait_turn(&config, "the cleared flip", || spine.mark_cleared(seq, &result)) {
            return match stop_if_busy(e, "file_held", Some(child.ordinal)) {
                Ok(stop) => Ok(LandOutcome::Stopped(stop)),
                Err(e) => Err(e),
            };
        }
        mark_child_landed(&spine, tranche, attempt, child.ordinal)?;
    }

    // ---- 3. CLEARING ----
    // [lander stage: before the clearing]
    fault::at_lander_stage(LanderStage::BeforeClear)?;
    drop(spine);
    if crate::tranche::clear_landed(cauldron, slot, tranche, attempt, now_ms)? {
        Ok(LandOutcome::Landed)
    } else {
        Ok(LandOutcome::NotLanding)
    }
}

/// `tranche_children.state = landed` and `landed_children + 1`, idempotent
/// (counted once per child, on the flip from `reserved`); a retry that finds
/// a child already cleared records nothing twice. Clears `landing_error`:
/// progress is what clears it.
fn mark_child_landed(spine: &SpineDb, tranche: &str, attempt: u32, ordinal: u32) -> Result<()> {
    let conn = spine.conn();
    let flipped = conn.execute(
        "UPDATE tranche_children SET state = ?4
         WHERE tranche_id = ?1 AND tranche_attempt = ?2 AND ordinal = ?3 AND state = ?5",
        rusqlite::params![
            tranche,
            attempt,
            ordinal,
            crate::tranche::CHILD_LANDED,
            crate::tranche::CHILD_RESERVED
        ],
    )?;
    if flipped == 1 {
        conn.execute(
            "UPDATE tranches SET landed_children = landed_children + 1, landing_error = NULL
             WHERE id = ?1 AND attempt = ?2",
            rusqlite::params![tranche, attempt],
        )?;
    }
    Ok(())
}

/// What the decision transaction concluded.
enum Decision {
    Made,
    AlreadyMade,
    Stop(LandingStop),
    Fail(Witness),
}

/// The decision's steps 2–5 inside its transaction (step 1 — owning it —
/// is the caller's). On `Stop` or `Fail` the caller DROPS the transaction
/// (rolling back step 1 with it); on `Made` it is committed here.
fn decide(
    tx: &rusqlite::Transaction<'_>,
    tranche: &str,
    attempt: u32,
    anchor: Seq,
    children: &mut [StagedChild],
) -> Result<Decision> {
    // ---- step 2: foreign rows, per child hash, in ordinal order ----
    let mut elsewhere: Vec<(u32, i64)> = Vec::new();
    for child in children.iter() {
        let mut stmt = tx.prepare(
            "SELECT seq, decided, cleared, payload, tranche_id, tranche_attempt
             FROM transactions WHERE hash = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map(rusqlite::params![child.hash.to_hex()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<i64>>(5)?,
            ))
        })?;
        for r in rows {
            let (seq, decided, cleared, payload, t_id, t_attempt) = r?;
            let own = t_id.as_deref() == Some(tranche) && t_attempt == Some(i64::from(attempt));
            if own {
                // Cannot exist before the decision; a second process's
                // reservation would have owned the decision first.
                return Err(SpineError::hard(format!(
                    "SPINE INTERNAL INVARIANT VIOLATED: tranche `{tranche}` attempt {attempt} is \
                     undecided but child {} already has a reserved row (seq {seq})",
                    child.ordinal
                )));
            }
            if cleared == 0 {
                return Ok(Decision::Stop(LandingStop {
                    why: "child_in_flight",
                    at_child: Some(child.ordinal),
                    colliding_seq: Some(seq),
                    message: format!(
                        "child {} (hash {}) is in flight elsewhere at seq {seq} ({}); the landing \
                         retries once that row lands or aborts",
                        child.ordinal,
                        child.hash,
                        if decided != 0 { "decided, landing" } else { "appended, not yet decided" }
                    ),
                }));
            }
            if decided == 0 {
                // An aborted attempt at this identity: landed nothing.
                continue;
            }
            // LANDED elsewhere. Payload equality, not envelope equality
            // (step 2's argument); a child that minted is never
            // deduplicated.
            let foreign: CompiledPayload = serde_json::from_str(&payload).map_err(|e| {
                SpineError::hard(format!("transaction row seq {seq}: stored payload will not parse: {e}"))
            })?;
            let minted_nothing =
                child.payload.minted.is_empty() && child.payload.provisional_mints().is_empty();
            if minted_nothing && foreign == child.payload {
                elsewhere.push((child.ordinal, seq));
            } else {
                return Ok(Decision::Fail(Witness::TrancheLandingRefused {
                    tranche: tranche.to_string(),
                    attempt,
                    ordinal: child.ordinal,
                    child_tx: child.hash.to_hex(),
                    why: "child_landed_elsewhere".to_string(),
                    witnesses: vec![],
                    colliding_seq: Some(seq),
                }));
            }
        }
    }
    let is_elsewhere = |ordinal: u32| elsewhere.iter().any(|(o, _)| *o == ordinal);

    // ---- step 3: numbering through ONE map ----
    let landing: Vec<CompiledPayload> = children
        .iter()
        .filter(|c| !is_elsewhere(c.ordinal))
        .map(|c| c.payload.clone())
        .collect();
    let map = crate::spine_db::mint_serials(tx, &landing, anchor)?;
    let mut renumbered: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (provisional, serial) in &map {
        tx.execute(
            "UPDATE locks SET resource = replace(resource, ?1, ?2)
             WHERE seq = ?3 AND instr(resource, ?1) > 0",
            rusqlite::params![provisional.id.0, serial.0, anchor.0],
        )?;
        renumbered.insert(
            provisional.to_string(),
            QualifiedNodeId {
                pool: provisional.pool.clone(),
                id: serial.clone(),
            }
            .to_string(),
        );
    }
    for child in children.iter_mut() {
        if is_elsewhere(child.ordinal) {
            continue;
        }
        child.payload.assign_ids(&map);
        for ids in child.bound.values_mut() {
            for q in ids.iter_mut() {
                if let Some(to) = map.get(q) {
                    q.id = to.clone();
                }
            }
        }
    }

    // ---- step 4: reserve the child rows, undecided, consecutive `seq` ----
    let of = u32::try_from(children.len() - elsewhere.len())
        .map_err(|_| SpineError::hard("a tranche cannot carry more children than u32::MAX"))?;
    let ts: String = tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |r| r.get(0))?;
    tx.execute_batch(BOUND_DDL)?;
    let json = |v: &dyn reserve_json::ToJson| -> Result<String> { v.to_json() };
    let mut index: u32 = 0;
    for child in children.iter() {
        if let Some((_, foreign_seq)) = elsewhere.iter().find(|(o, _)| *o == child.ordinal) {
            tx.execute(
                "UPDATE tranche_children SET state = ?4, seq = ?5
                 WHERE tranche_id = ?1 AND tranche_attempt = ?2 AND ordinal = ?3",
                rusqlite::params![
                    tranche,
                    attempt,
                    child.ordinal,
                    crate::tranche::CHILD_LANDED_ELSEWHERE,
                    foreign_seq
                ],
            )?;
            continue;
        }
        let touched: Vec<&str> = {
            let mut out: Vec<&str> = Vec::new();
            for part in &child.payload.parts {
                if !part.deltas.is_empty() && !out.contains(&part.pool.as_str()) {
                    out.push(part.pool.as_str());
                }
            }
            out
        };
        let inserted = tx.execute(
            "INSERT INTO transactions
               (hash, attempt, ts, envelope_version, initiator, instrument, statement, bindings,
                payload, touched, tranche_id, tranche_attempt, tranche_ordinal, tranche_index,
                tranche_of)
             VALUES (?1,
                     (SELECT COALESCE(MAX(attempt), 0) + 1 FROM transactions WHERE hash = ?1),
                     ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                child.hash.to_hex(),
                ts,
                child.envelope.format_version,
                json(&child.envelope.provenance.initiator)?,
                json(&child.envelope.provenance.instrument)?,
                child.envelope.statement,
                json(&child.envelope.bindings)?,
                json(&child.payload)?,
                json(&touched)?,
                tranche,
                attempt,
                child.ordinal,
                index,
                of,
            ],
        );
        if let Err(rusqlite::Error::SqliteFailure(f, _)) = &inserted {
            if f.code == rusqlite::ErrorCode::ConstraintViolation {
                return Err(SpineError::hard(format!(
                    "SPINE INTERNAL INVARIANT VIOLATED: reserving child {} of tranche `{tranche}` \
                     attempt {attempt} hit the one-live-row-per-hash index after the foreign-row scan \
                     found none in flight",
                    child.ordinal
                )));
            }
        }
        inserted?;
        let seq = tx.last_insert_rowid();
        for (i, part) in child.payload.parts.iter().enumerate() {
            tx.execute(
                "INSERT INTO tx_parts (seq, part, pool, schema_version, landed)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                rusqlite::params![seq, i as i64, part.pool.as_str(), part.schema_version],
            )?;
        }
        tx.execute(
            "INSERT INTO tx_bound (seq, bound) VALUES (?1, ?2)",
            rusqlite::params![seq, json(&child.bound)?],
        )?;
        // ---- step 5: the staging row, reserved — numbered payload and bound ----
        tx.execute(
            "UPDATE tranche_children SET state = ?4, seq = ?5, payload = ?6, bound = ?7
             WHERE tranche_id = ?1 AND tranche_attempt = ?2 AND ordinal = ?3",
            rusqlite::params![
                tranche,
                attempt,
                child.ordinal,
                crate::tranche::CHILD_RESERVED,
                seq,
                json(&child.payload)?,
                json(&child.bound)?
            ],
        )?;
        index += 1;
    }
    tx.execute(
        "UPDATE tranches SET renumbered = ?3, landing_error = NULL WHERE id = ?1 AND attempt = ?2",
        rusqlite::params![tranche, attempt, json(&renumbered)?],
    )?;
    Ok(Decision::Made)
}

/// Serialize-to-JSON for the reservation's columns, loud on failure.
mod reserve_json {
    use crate::error::{Result, SpineError};
    pub(super) trait ToJson {
        fn to_json(&self) -> Result<String>;
    }
    impl<T: serde::Serialize> ToJson for T {
        fn to_json(&self) -> Result<String> {
            serde_json::to_string(self)
                .map_err(|e| SpineError::hard(format!("a reserved child row's column will not serialize: {e}")))
        }
    }
}
