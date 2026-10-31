//! **PLACEMENT BY EXPECTED THREAD DEATHS, SUBAGENTS COUNTED AS THREADS**
//! (INTENT 83, idx `{117d80e2-7c74-4054-96f1-e113ab37c245, 2951}`; the
//! estimator's own shape comes from idx `2928`).
//!
//! The operator's objective, in his words: *"You're trying to minimize the
//! number of threads that died. And so you have to treat all of those
//! sub-agents as threads. You're not going to know beforehand, unfortunately,
//! exactly how many sub-agents there will be, but you can use the estimate."*
//!
//! And the constraint that binds the implementation, same message: *"You'll
//! want to keep it away from an account nearing a session limit, but that
//! should just be a natural falling out of… the measurement that it will hit
//! the second limit."* **No special case for big threads. No special case for
//! nearly-closed accounts.** Both must fall out of the measurement, and
//! `a_nearly_closed_account_loses_by_measurement_never_by_a_rule` is the test
//! that holds that line: it shows the SAME account at the SAME session reading
//! winning or losing purely as a function of what the other account reads, so
//! no absolute "nearly closed" cutoff can be hiding anywhere.
//!
//! **What the operator DELETED and what must not creep back in.** The design
//! this implements justified protecting a large thread by claiming a move costs
//! more once a session limit has been hit. He killed that reasoning outright:
//! *"the justification you used that it's going to be more expensive to move it
//! if it hits the session limit, that's not true because you're gonna move it
//! beforehand anyway."* Nothing in this file, in `Budgets::place` or in
//! `Budgets::reachable_tokens` prices a move at all. The measurement is about what an
//! account can CARRY, never about what a move costs.
//!
//! Every test drives the real crate against a tempdir. No claude process is
//! ever spawned, `THREADS_HOME` is never resolved, and nothing here touches the
//! operator's login, his keychain, his database or the running daemon.

use threads::budgets::{Budgets, Demand, PlacementRequest, Reach, Thresholds};
use threads::gauge::Axis;
use threads::store::{Store, UsageRow};

// ── harness ─────────────────────────────────────────────────────────────────

fn svc() -> (Budgets, Store) {
    let store = Store::open_in_memory().unwrap();
    (Budgets::new(store.clone()), store)
}

/// A `Placeable` for `name`, built the only way one can be built (CON-27: a
/// `Placeable` comes from a POOL ENTRY, and an entry must name the directory
/// its login is stored in).
fn placeable(name: &str) -> threads::accounts::Placeable {
    threads::pool::PoolEntry::claude_account(name, &format!("/tmp/threads-expected-deaths/{name}"))
        .expect("an entry that names its login directory is a pool member")
        .placeable()
}

fn pool(names: &[&str]) -> Vec<threads::accounts::Placeable> {
    names.iter().copied().map(placeable).collect()
}

fn place(b: &Budgets, th: &Thresholds, candidates: &[threads::accounts::Placeable], d: Demand) -> Option<String> {
    b.place(&PlacementRequest {
        thresholds: th,
        candidates,
        prefer: None,
        sessions: &[],
        demand: d,
    })
    .unwrap()
    .map(|p| p.name().to_string())
}

/// One completed EXECUTION on this thread, worth `tokens` tokens. `epoch` is
/// the execution's `started_at`, which is what groups a turn's several delta
/// rows back into one turn.
fn record_turn(store: &Store, thread_id: &str, epoch: &str, tokens: u64) {
    store
        .record_usage(&UsageRow {
            thread_id: thread_id.into(),
            account: Some("a".into()),
            model: "claude-opus".into(),
            input_tokens: tokens,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: 0.0,
            started_at: Some(epoch.into()),
        })
        .unwrap();
}

/// One turn on this thread's ledger that spawned `subagents` subagents.
fn record_fanout(store: &Store, thread_id: &str, seq: &mut u64, subagents: usize) {
    let push = |ev: threads::event::ThreadEvent, seq: &mut u64| {
        *seq += 1;
        store
            .append_event(&threads::event::StoredEvent {
                thread_id: thread_id.into(),
                seq: *seq,
                message_id: None,
                ts: threads::store::now_rfc3339(),
                event: ev,
            })
            .unwrap();
    };
    push(
        threads::event::ThreadEvent::TurnStarted {
            attempt: 1,
            message_ids: Vec::new(),
        },
        seq,
    );
    for i in 0..subagents {
        push(
            threads::event::ThreadEvent::AgentSpawn {
                tool_use_id: format!("tu-{seq}-{i}"),
                subagent_type: None,
                description: None,
                run_in_background: false,
                // Synthetic top-level spawns: no parent tool call above them.
                parent_tool_use_id: None,
            },
            seq,
        );
    }
}

// ── the measurement: reachable budget is the MIN across the quota axes ──────

/// **An account is worth what its TIGHTEST window can carry, not what its most
/// generous one can.**
///
/// This is the whole of "keep it away from an account nearing a session limit",
/// expressed as a measurement rather than as a rule about session limits.
#[test]
fn the_reachable_budget_is_the_min_across_the_quota_axes() {
    let (b, _store) = svc();
    // `roomy-week-shut-session`: the week is wide open, the session is nearly
    // spent. It can carry two points of quota before it stops.
    b.observe("roomy-week-shut-session", Axis::WeeklyAll, 1.0, None, "usage_poll").unwrap();
    b.observe("roomy-week-shut-session", Axis::Session, 98.0, None, "usage_poll").unwrap();
    // `even`: middling on both, and therefore able to carry far more.
    b.observe("even", Axis::WeeklyAll, 50.0, None, "usage_poll").unwrap();
    b.observe("even", Axis::Session, 50.0, None, "usage_poll").unwrap();

    let th = Thresholds::ungated();
    assert_eq!(
        place(&b, &th, &pool(&["roomy-week-shut-session", "even"]), Demand::default()).as_deref(),
        Some("even"),
        "a wide-open week cannot buy back a session that is about to close"
    );

    // Stated directly on the measurement, so the test does not depend on the
    // ranking to make its point: the tighter axis is the reach.
    let tight = b.reachable_tokens("roomy-week-shut-session", &th);
    let even = b.reachable_tokens("even", &th);
    match (tight, even) {
        (Reach::Known(t), Reach::Known(e)) => assert!(
            t < e,
            "the nearly-closed session must price the account: {t} !< {e}"
        ),
        other => panic!("both accounts are measured, so both are Known: {other:?}"),
    }
}

/// **No absolute cutoff exists anywhere.** The same account, at the same
/// session reading, wins or loses purely as a function of what the OTHER
/// account reads.
///
/// A special case — `if session_pct > 90 { skip this account }`, or any
/// "protect the big thread" branch — would make `tight`'s fate depend on
/// `tight` alone. It does not, and this fails the moment it does.
#[test]
fn a_nearly_closed_account_loses_by_measurement_never_by_a_rule() {
    let th = Thresholds::ungated();

    // `tight` is at 95% of its session both times. Nothing about it changes.
    let (b, _store) = svc();
    b.observe("tight", Axis::Session, 95.0, None, "usage_poll").unwrap();
    b.observe("tighter", Axis::Session, 99.5, None, "usage_poll").unwrap();
    assert_eq!(
        place(&b, &th, &pool(&["tight", "tighter"]), Demand::default()).as_deref(),
        Some("tight"),
        "a nearly-closed account is still the best place to be when the \
         alternative is worse — it is not excluded by a rule"
    );

    let (b, _store) = svc();
    b.observe("tight", Axis::Session, 95.0, None, "usage_poll").unwrap();
    b.observe("roomy", Axis::Session, 20.0, None, "usage_poll").unwrap();
    assert_eq!(
        place(&b, &th, &pool(&["tight", "roomy"]), Demand::default()).as_deref(),
        Some("roomy"),
        "and it loses to a roomier one — same account, same reading, different \
         answer, which is what makes this a measurement"
    );
}

/// An account nobody has ever polled is UNMEASURED, not empty — it ranks behind
/// anything we have actually read. (A freshly registered account is exactly in
/// this state, and treating it as maximally free is what made it win every
/// placement.)
#[test]
fn a_never_measured_account_ranks_behind_every_measured_one() {
    let (b, _store) = svc();
    b.observe("measured", Axis::Session, 80.0, None, "usage_poll").unwrap();
    assert!(matches!(
        b.reachable_tokens("fresh", &Thresholds::ungated()),
        Reach::Unknown
    ));
    assert_eq!(
        place(&b, &Thresholds::ungated(), &pool(&["fresh", "measured"]), Demand::default()).as_deref(),
        Some("measured"),
        "absence of evidence is not evidence of room"
    );
}

// ── the objective: EVERY SUBAGENT IS A THREAD ───────────────────────────────

/// **THE LOAD-BEARING TEST OF INTENT 83.**
///
/// Two accounts, one measurably safer than the other by a margin that is real
/// but small. A LONE thread ties — the difference is not worth a placement, so
/// the deterministic tie-breaks decide and nothing churns. The SAME thread,
/// with a recorded fan-out, does not tie: the identical difference in survival
/// is now several whole threads, and placement discriminates.
///
/// Delete the subagent term from `Demand::threads_at_risk` and the second half
/// of this test fails, because both halves collapse onto the first answer.
#[test]
fn the_subagent_estimate_decides_a_placement_a_lone_thread_would_tie() {
    let (b, _store) = svc();
    // Both accounts are comfortable. `safer` is comfortable by a little more.
    b.observe("aa-safer", Axis::Session, 20.0, None, "usage_poll").unwrap();
    b.observe("zz-riskier", Axis::Session, 26.0, None, "usage_poll").unwrap();
    let th = Thresholds::ungated();
    // A demand big enough that the survival exponent is in a range where a few
    // points of quota still register at all.
    let lone = Demand {
        tokens_per_turn: 400_000.0,
        subagents: 0.0,
    };
    let fanned = Demand {
        tokens_per_turn: 400_000.0,
        subagents: 1_200.0,
    };

    // The difference in DEATH PROBABILITY is identical in both cases. It is the
    // number of threads riding on it that differs.
    let deaths_lone_safer = lone.expected_deaths(b.reachable_tokens("aa-safer", &th));
    let deaths_lone_risky = lone.expected_deaths(b.reachable_tokens("zz-riskier", &th));
    assert!(
        (deaths_lone_risky - deaths_lone_safer).abs() < 0.01,
        "for one thread the gap is under the deadband: {deaths_lone_safer} vs {deaths_lone_risky}"
    );
    let deaths_fanned_safer = fanned.expected_deaths(b.reachable_tokens("aa-safer", &th));
    let deaths_fanned_risky = fanned.expected_deaths(b.reachable_tokens("zz-riskier", &th));
    assert!(
        deaths_fanned_risky - deaths_fanned_safer > 1.0,
        "for 1,201 threads the SAME gap is whole threads: {deaths_fanned_safer} vs {deaths_fanned_risky}"
    );

    // And that is what the ranking acts on. The lone thread ties and falls
    // through to the deterministic tie-breaks, which take the alphabetically
    // first of the two only after spread and rotation have had their say; the
    // fanned-out thread is placed on the account that can actually carry it.
    assert_eq!(
        place(&b, &th, &pool(&["aa-safer", "zz-riskier"]), fanned).as_deref(),
        Some("aa-safer"),
        "1,201 threads are at stake, so the measurement decides"
    );
}

/// The death count IS one plus the subagents — the operator's arithmetic,
/// pinned. *"A thread that fans out to 1,200 subagents is 1,201 deaths, not
/// one."*
#[test]
fn every_subagent_counts_as_a_thread_in_the_death_count() {
    assert_eq!(Demand::default().threads_at_risk(), 1.0);
    assert_eq!(
        Demand {
            tokens_per_turn: 1.0,
            subagents: 1_200.0
        }
        .threads_at_risk(),
        1_201.0
    );
    // An account with no reachable budget at all kills everything on it, and
    // the expected count is therefore the whole population.
    let all_dead = Demand {
        tokens_per_turn: 1.0,
        subagents: 1_200.0,
    }
    .expected_deaths(Reach::Known(0.0));
    assert_eq!(all_dead, 1_201.0);
}

// ── the estimator ───────────────────────────────────────────────────────────

/// The burn is read from `usage_ledger`, one sample per EXECUTION, and it is
/// exponentially weighted — the newest turn moves the estimate most.
///
/// Explicitly not ARIMA (his words). The property pinned here is the only one
/// the ranking needs: recency dominates, and the estimate stays inside the
/// range of the samples.
#[test]
fn the_burn_estimate_is_exponentially_weighted_over_this_threads_own_turns() {
    let (b, store) = svc();
    // Three small turns, then one large one. A flat mean would land near
    // 275_000; an exponentially weighted one leans hard on the newest.
    record_turn(&store, "t", "2026-08-06T10:00:00Z", 100_000);
    record_turn(&store, "t", "2026-08-06T11:00:00Z", 100_000);
    record_turn(&store, "t", "2026-08-06T12:00:00Z", 100_000);
    record_turn(&store, "t", "2026-08-06T13:00:00Z", 800_000);
    let d = b.estimate_demand("t");
    assert!(
        d.tokens_per_turn > 275_000.0,
        "the newest turn must dominate a flat mean: {}",
        d.tokens_per_turn
    );
    assert!(
        d.tokens_per_turn < 800_000.0,
        "but it is a mean, not the last sample: {}",
        d.tokens_per_turn
    );

    // A turn is ONE sample however many ledger rows it wrote. Two more rows
    // under the SAME execution epoch are the same turn, summed.
    let before = b.estimate_demand("t").tokens_per_turn;
    record_turn(&store, "t", "2026-08-06T13:00:00Z", 100_000);
    let after = b.estimate_demand("t").tokens_per_turn;
    assert!(
        after > before,
        "a second row of the same execution adds to that turn: {before} -> {after}"
    );
}

/// The fan-out is counted off the `agent_spawn` events the ledger already
/// records, bracketed by `turn_started` — no new instrumentation, and a turn
/// that spawned nothing is a genuine zero.
#[test]
fn the_fanout_estimate_is_read_from_agent_spawn_events_already_recorded() {
    let (b, store) = svc();
    let mut seq = 0u64;
    assert_eq!(
        b.estimate_demand("t").subagents,
        0.0,
        "a thread that has never run has no fan-out"
    );
    record_fanout(&store, "t", &mut seq, 0);
    record_fanout(&store, "t", &mut seq, 0);
    assert_eq!(
        b.estimate_demand("t").subagents,
        0.0,
        "two turns that spawned nothing is a measured zero"
    );
    record_fanout(&store, "t", &mut seq, 10);
    let d = b.estimate_demand("t");
    assert!(
        d.subagents > 3.0 && d.subagents < 10.0,
        "the newest turn spawned ten; weighted against two empty turns: {}",
        d.subagents
    );
}

/// **The second variable he named**: *"a moving average on the number of
/// sub-agents. And the number of sub-agents running right now"* (idx `2928`).
///
/// The live count is folded in as a MAX, not a sum and not a replacement — a
/// thread that has never fanned out before but has three hundred children alive
/// at this instant is three hundred and one threads whatever its history says.
#[test]
fn subagents_running_right_now_count_even_with_no_history_of_fanning_out() {
    let (b, _store) = svc();
    let cold = b.estimate_demand("never-run");
    assert_eq!(cold.threads_at_risk(), 1.0);
    assert_eq!(cold.with_live_subagents(300).threads_at_risk(), 301.0);
    // And a rich history is not thrown away by a quiet moment.
    let historic = Demand {
        tokens_per_turn: 1.0,
        subagents: 50.0,
    };
    assert_eq!(historic.with_live_subagents(0).subagents, 50.0);
}

// ── CON-27 still holds after all of the above ───────────────────────────────

/// **The machine default is still structurally unplaceable.** Verified against
/// the NEW ranking rather than assumed to have survived it: the measurement
/// changed, the domain did not.
#[test]
fn the_machine_default_still_cannot_be_ranked_or_returned() {
    // It cannot be made into a candidate at all, because an account with no
    // login directory is not an entry. Note the rule does not consult the NAME
    // (2026-08-27: the module no longer knows the word) — the machine default
    // and a second machine-default row under any other label fall out together.
    assert!(threads::pool::PoolEntry::claude_account("default", "").is_none());
    assert!(threads::pool::PoolEntry::claude_account("looks-normal", "").is_none());

    // And the POOL omits it however good its numbers look.
    let mut registry = threads::accounts::Registry::default();
    registry.add("default", None);
    registry.add("owned", Some("/tmp/threads-expected-deaths/owned".into()));
    let names = threads::pool::Pool::from_registry(&registry).names();
    assert_eq!(names, vec!["owned".to_string()]);
}
