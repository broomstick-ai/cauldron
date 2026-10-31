//! # THE WALL-CLOCK SEAM — one door, for the instants this crate COMPARES
//!
//! ## The bug this exists for, measured twice in one day
//!
//! On 2026-10-07 the same defect was found from two directions:
//!
//! 1. **Tests fail under any optimized build.** They read the wall clock twice
//!    and, with optimization on, *both reads land in the same microsecond*.
//!    There are two ways that lands:
//!
//!    * in [`crate::budgets::Budgets::calibrate_overlap_window`] it collapses an
//!      execution window to `start == end`, and
//!      [`crate::gauge::evaluate_window`] rejects it as `Unbracketed`;
//!    * in a shape test it hits the STAMP RULE: an input stamped at or before a
//!      cell's `folded_at` is *refused*, so two `Hub::emit` calls in one
//!      microsecond mean the second one is **dropped** and the window never
//!      fills.
//!
//!    **The count was five, and five was wrong.** Five is what ONE release run
//!    names, because each of these is a race the machine's load decides.
//!    Repeating the runs at `290113e` makes it **thirteen**: the release `--lib`
//!    suite alone is red on 10 of 10 runs with a different subset of SEVEN
//!    `budgets::tests` each time, and the integration tests add six more —
//!    `budgets_e2e` (1), `recent_updates` (2), `retention` (2) and `lag_repair`
//!    (1). Ten debug runs of the same `--lib` suite: 0 red.
//!
//!    Eleven of the thirteen were confirmed at the mechanism — a collapsed
//!    window, or two byte-identical instants in the failure text (the clearest
//!    being `recent_updates::a_seen_moves_the_roll_up_and_never_the_order`:
//!    "folded_at=…674895 UTC, the seen was emitted at …674895 UTC"). The
//!    `lag_repair` one is classed here by family and by its fixture, not by its
//!    own instant-level evidence.
//!
//!    Thirteen remembered lines is not a fix, which is why this one is in the
//!    FIXTURES.
//! 2. **A fixture rotted.** A 429 fixture hard-coded `resets 2:40am
//!    (America/Chicago)` and was only ever green in 6 of 24 hourly frames. This
//!    seam does NOT reach that one — see the end of this doc.
//!
//! Both are the same shape of mistake: *time is read, and the value is then
//! compared against another reading*, with nothing in the type system or the
//! build saying that the interval between the two reads is load-bearing. The
//! tests only passed because debug codegen is slow enough for the clock to tick
//! between two back-to-back calls. A suite whose greenness depends on the
//! binary being slow is not a suite.
//!
//! [`crate::gauge::prose_to_rfc3339`] already carries the scar tissue: its
//! docstring records that `Local::now()` and `Utc::now()` used to be called
//! microseconds apart and that the drift between them poisoned every
//! calibration pair. That was fixed in one function, by hand, by sampling ONE
//! instant. This module is that fix generalized into something a new call site
//! cannot avoid.
//!
//! ## What it is NOT
//!
//! It is **not a tolerance** and **not a sleep**. Widening a tolerance hides
//! the invariant: if a calibration bracket genuinely must be non-degenerate, a
//! tolerance lets a real collapse through. A sleep is the same timing
//! dependence with a bigger constant and a slower suite.
//!
//! It is also **not a production behaviour change**. [`now`] with no override
//! installed is `chrono::Utc::now()` and nothing else — no monotonic clamp, no
//! de-duplication, no truncation. A strictly-increasing *production* clock
//! would make every one of those tests pass with no test changes at all, and it
//! is a tempting one-line fix; it is deliberately not taken here, because whether a
//! zero-width bracket is a thing production must tolerate is a question about
//! the calibration logic, and making a test deterministic must not silently
//! decide it. See the report that accompanies this change.
//!
//! ## The shape: a process-wide injectable, because the count decided it
//!
//! Counted before choosing, on `lib/threads/src` at `290113e`:
//!
//! | primitive | production sites |
//! |---|---|
//! | [`crate::store::now_rfc3339`] (itself one `Utc::now()`) | 57 callers |
//! | direct `chrono::Utc::now()` | 38 |
//! | direct `chrono::Local::now()` | 1 |
//!
//! Across twelve modules, two of which are 9.8k and 11k lines, with `Budgets`,
//! `Store` and `ThreadManager` built in roughly a hundred test fixtures. A
//! generic clock parameter would have to be threaded through all of them, and a
//! `dyn Clock` field would have to be plumbed into every constructor and every
//! fixture. Threading a generic through everything is worse than the disease,
//! so the injection is an **AMBIENT OVERRIDE** — no type, no constructor, no
//! parameter — and the call sites change by exactly one token.
//!
//! **The two modes have DIFFERENT scopes, and the asymmetry is load-bearing:**
//!
//! * [`testing::ticking`] — real time that may not repeat — is **process-wide**,
//!   armed once and never disarmed. It has to be: a cell's `folded_at` is shared
//!   by every thread that folds into it, so two threads emitting in the same
//!   microsecond collide exactly as two emits on one thread do, and a
//!   `#[tokio::test(flavor = "multi_thread")]` emit loop moves between worker
//!   threads across its awaits. Uniqueness holds across the process or it does
//!   not hold. It is safe to share and safe to leak because it is the real clock
//!   with ONE property added; it cannot move any test's notion of now away from
//!   wall time, so it cannot change what a neighbouring test sees.
//!   *(It was thread-local first. That version could not reach
//!   `lag_repair`'s `burst`, which emits, awaits, and emits again.)*
//! * [`testing::freeze_at`] — every read answers one instant — is
//!   **thread-local and RAII**, because it DOES move a thread's notion of now
//!   and would corrupt every test running beside it. `cargo test` runs tests in
//!   parallel threads inside one process; the freeze is installed per thread and
//!   lifted by [`testing::Installed`]'s `Drop`, so it cannot outlive the scope
//!   that asked for it even if that scope panics. There is deliberately no
//!   process-wide freeze.
//!
//! The two compose: a freeze nested inside a ticking process freezes its own
//! scope and restores the ticking clock on drop.
//!
//! ## WHICH SIDE A NEW CALL SITE BELONGS ON — the rule, in three clauses
//!
//! The seam is deliberately PARTIAL. A seam nobody can avoid using beats a seam
//! everywhere, but a partial seam with no stated rule rots, so here is the rule.
//! Two clauses bind PRODUCTION code; the third binds TESTS.
//!
//! **Clause 1 — the free ride.** [`crate::store::now_rfc3339`] is this crate's
//! one door to a timestamp that gets *persisted*, and it is inside the seam. Any
//! module that calls it is on the seam's side for free, wherever it lives. A new
//! site that needs a timestamp to store calls `now_rfc3339()` and has no
//! decision to make. This is what carries 57 of the production sites.
//!
//! **Clause 2 — the five files.** A read that does not go through
//! `now_rfc3339` must go through [`now`] if the value is **differenced against,
//! or compared with, another clock read taken by this process**:
//!
//! * a **shape stamp** — compared against a cell's `folded_at`, where a tie is
//!   not a rounding error but a *dropped input*;
//! * a **calibration `now`** — the upper bound of the window
//!   `calibrate_overlap_window` brackets against persisted observation rows;
//! * a **reset resolved from prose** — compared for equality against the
//!   previous poll's resolution of the *same* prose, which is what
//!   `prose_to_rfc3339`'s docstring is about.
//!
//! If the value is only **reported, logged, formatted or displayed** — a
//! `now_ms` latency readout, a `captured_at` on a snapshot, a pressure row's
//! `now` — then `chrono::Utc::now()` is legal and preferred, because routing it
//! through the seam would let a test's frozen clock silently zero a latency
//! number nobody meant to freeze.
//!
//! Clause 2 is enforced as a **FILE LIST**, because that is what a test can
//! check: `store.rs`, `budgets.rs`, `gauge.rs`, `shape.rs` and `runtime.rs` are
//! INSIDE, and a direct wall-clock read anywhere in one of them — production
//! code or test code — fails
//! [`tests::the_five_in_seam_files_hold_no_direct_wall_clock_read`]. Those five
//! are exactly where this crate mints an instant it later compares. Every other
//! module is outside. A sixth file joins the set by being added to
//! [`IN_SEAM`] — nothing joins it implicitly, and nothing leaves it quietly
//! either.
//!
//! **Clause 3 — a TEST that drives the hub is on the seam, by its fixture.**
//! Clauses 1 and 2 say where production reads the clock. This one says where a
//! test must INSTALL one, and it is on the file rather than on the test: a
//! `tests/*.rs` that emits through the hub calls
//! [`testing::ticking`] in the fixture that builds the manager,
//! so every test in it — including the next one anybody writes — is covered
//! without being told. [`tests::every_hub_emitting_test_file_installs_the_clock_seam`]
//! enforces it; it found `tests/nesting.rs` on its first run, which is the
//! argument for having it. `budgets::tests::svc()` is the same move for the unit
//! tests, and it is why those seven calibration tests are now deterministic
//! rather than seven remembered lines.
//!
//! **Why the per-thread install needs no guard to hold and is safe to leak:**
//! [`Mode::Increasing`] is the real clock with ONE property added, that it may
//! not repeat. It cannot move a test's notion of now away from wall time, so it
//! cannot change the outcome of a test that it leaks onto. A FROZEN clock could,
//! which is why freezing is RAII-only and has no per-thread form.
//!
//! ## Wall versus monotonic: ONE seam, and here is why
//!
//! The bracket bug is about an *interval*, which ordinarily wants a monotonic
//! source — and that is the wrong answer here. The interval's endpoints are
//! **persisted** (`usage_ledger.started_at` and `.ts`, as RFC3339 text),
//! **queried with SQL string comparisons**
//! ([`crate::store::Store::execution_intervals`]), and **compared against
//! wall instants this process did not produce** — the `resets_at` prose the
//! Claude API reports. A `std::time::Instant` is not representable in any of
//! those three places. So the endpoints must be wall time; what they additionally
//! need is to be *distinct*, which is a property of the reads, not of the clock
//! domain. Hence one seam, wall-clock, and no monotonic twin.
//!
//! The 41 `Instant::now()` sites in this crate are a genuinely separate
//! concern — elapsed-time measurements for timeouts and performance budgets,
//! never persisted and never compared against a stored value — and this module
//! deliberately does not touch them. Conflating the two would put a test's
//! frozen clock in front of a timeout.
//!
//! The 429 fixture is the third case and it is honest to say the seam does not
//! reach it: `tests/fixtures/fake-claude` is a *separate process*, so its `now`
//! is its own. That fixture was fixed where it had to be — by computing its
//! reset relative to its own clock — and
//! `the_fixtures_429_resolves_to_a_live_window_in_every_hourly_frame` sweeps all
//! 24 hourly frames to keep it that way. The in-process half of that same
//! question, `prose_to_rfc3339`, IS on the seam.

use chrono::{DateTime, Local, Utc};

/// The files in which a direct wall-clock read is banned — clause 2 of the rule
/// in this module's docs, in the only form a test can check.
///
/// Adding a file here is how the seam grows. It is a deliberate, visible edit;
/// there is no implicit membership and no wildcard.
pub const IN_SEAM: &[&str] = &[
    "src/store.rs",
    "src/budgets.rs",
    "src/gauge.rs",
    "src/shape.rs",
    "src/runtime.rs",
];

use std::sync::atomic::{AtomicI64, Ordering};

/// **THE TICKING CLOCK IS PROCESS-WIDE, AND THAT IS THE CORRECTION, NOT A
/// COMPROMISE.**
///
/// It was thread-local first, and that was wrong twice over:
///
/// * **It did not reach the tests that need it.** Five of the hub-driving test
///   files run tests on `#[tokio::test(flavor = "multi_thread")]`. A future that
///   emits, awaits, and emits again resumes on whatever worker thread is free,
///   so a per-thread override covers the first half of the loop and not the
///   second. `lag_repair::the_repair_for_a_lagging_consumer_keeps_its_negotiated_retain`
///   is exactly that shape — `burst` emits, sleeps, emits.
/// * **It was the wrong requirement.** A cell's `folded_at` is shared by every
///   thread that folds into it, so two threads emitting in the same microsecond
///   collide just as surely as two emits on one thread. Uniqueness has to hold
///   across the PROCESS or it does not hold.
///
/// Zero means off, and off is the only value production ever sees.
static TICK_STEP_NANOS: AtomicI64 = AtomicI64::new(0);

/// The newest instant this clock has handed out, process-wide, in nanoseconds
/// since the epoch. Only read and written while [`TICK_STEP_NANOS`] is non-zero.
static LAST_NANOS: AtomicI64 = AtomicI64::new(0);

thread_local! {
    /// **FREEZING STAYS THREAD-LOCAL**, unlike ticking, because the two are not
    /// symmetrical. A ticking clock is the real clock with one property added and
    /// is harmless to share; a FROZEN clock moves a thread's notion of now and
    /// would corrupt every test running beside it. `cargo test` runs tests in
    /// parallel threads in one process, so this one is per-thread and RAII.
    ///
    /// `None` — the only value production ever sees.
    static FROZEN: std::cell::Cell<Option<DateTime<Utc>>> =
        const { std::cell::Cell::new(None) };
}

/// **The crate's wall clock.** With no override installed this is
/// `chrono::Utc::now()` and nothing more — one thread-local read and one relaxed
/// atomic load ahead of it, both of which the branch predictor eats.
///
/// Call this instead of `chrono::Utc::now()` in any of [`IN_SEAM`]; see this
/// module's docs for which side a new call site belongs on.
pub fn now() -> DateTime<Utc> {
    if let Some(at) = FROZEN.with(|f| f.get()) {
        return at;
    }
    let step = TICK_STEP_NANOS.load(Ordering::Relaxed);
    if step == 0 {
        return Utc::now();
    }
    // Real time, except that it may never repeat or go backwards — for any
    // thread. The compare-exchange is what makes that true under concurrency:
    // two threads reading in the same microsecond cannot both win the same
    // nanosecond, so they cannot both stamp the same instant.
    let real = Utc::now();
    let real_nanos = real.timestamp_nanos_opt().unwrap_or(i64::MAX);
    let mut prev = LAST_NANOS.load(Ordering::Relaxed);
    loop {
        let next = if real_nanos > prev {
            real_nanos
        } else {
            prev.saturating_add(step)
        };
        match LAST_NANOS.compare_exchange_weak(
            prev,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return DateTime::from_timestamp_nanos(next),
            Err(seen) => prev = seen,
        }
    }
}

/// **The same instant, in the local zone.**
///
/// `Local::now()` and `Utc::now()` are two reads of two clocks, and the
/// difference between them is precisely what poisoned `prose_to_rfc3339` before
/// it was fixed by hand. This is one read, converted — so a caller that needs
/// both views cannot reintroduce that drift, and the seam reaches the local-zone
/// path too.
pub fn now_local() -> DateTime<Local> {
    now().with_timezone(&Local)
}

/// **Installing a clock. TESTS ONLY.**
///
/// This is `pub` because integration tests in `tests/` link this crate as an
/// external crate and cannot reach a `#[cfg(test)]` item — there is no way to
/// make the seam usable from `tests/budgets_e2e.rs` without it. It is
/// `#[doc(hidden)]` and named so that its one legal use is obvious, and
/// [`super::tests::no_production_path_installs_a_test_clock`] fails the build if
/// a `src/` production path ever calls it.
#[doc(hidden)]
pub mod testing {
    use super::{FROZEN, LAST_NANOS, TICK_STEP_NANOS};
    use chrono::{DateTime, Utc};
    use std::sync::atomic::Ordering;

    /// **A FROZEN clock, put back the way it was when this is dropped.**
    ///
    /// Hold it for as long as the test needs the clock: `let _c = ...;`. `Drop`
    /// rather than an explicit uninstall so a panicking test cannot leak a
    /// frozen clock onto whatever libtest runs on that thread next, and `!Send`
    /// so the guard cannot be moved off the thread whose override it owns.
    ///
    /// **Restore, not clear**, so nesting is legal and means what it reads: a
    /// `freeze_at` inside a test that is otherwise ticking freezes exactly the
    /// statements in its scope and gives the ticking clock back afterwards. That
    /// is the shape every "and now make the two reads collide" assertion wants,
    /// and it is what
    /// `budgets::tests::a_zero_width_execution_window_is_refused_and_writes_no_estimate`
    /// is built on.
    #[must_use = "the clock is unfrozen as soon as this guard is dropped — bind it"]
    pub struct Installed {
        previous: Option<DateTime<Utc>>,
        _not_send: std::marker::PhantomData<*const ()>,
    }

    impl Drop for Installed {
        fn drop(&mut self) {
            let prev = self.previous;
            FROZEN.with(|f| f.set(prev));
        }
    }

    /// **Every read answers `at`, on this thread.** The degenerate case, on
    /// purpose.
    ///
    /// Production cannot produce a zero-width calibration window — `started_at`
    /// is read off the execution's `running_turns` row, written when the turn
    /// began, and the ledger row's `ts` is written when its usage lands, so the
    /// gap is a whole claude turn. This is the only way to assert what the code
    /// DOES with one.
    ///
    /// Thread-local and RAII, unlike [`ticking`], because a frozen clock moves a
    /// thread's notion of now and WOULD corrupt a test running beside it.
    pub fn freeze_at(at: DateTime<Utc>) -> Installed {
        let previous = FROZEN.with(|f| f.replace(Some(at)));
        Installed {
            previous,
            _not_send: std::marker::PhantomData,
        }
    }

    /// [`freeze_at`] anchored on the real clock, so the rest of the test stays
    /// in the same era.
    pub fn freeze() -> Installed {
        freeze_at(super::now())
    }

    /// **REAL TIME THAT MAY NOT REPEAT, for the whole process. No guard to
    /// hold, and no way back.**
    ///
    /// This is the one a FIXTURE calls — `budgets::tests::svc()`, and the
    /// manager-building fixture of every integration test that drives the hub —
    /// so that every test built on it is on the seam and a new one cannot be
    /// written off the seam by forgetting a line. A fixture cannot hand back a
    /// guard without changing the signature of every one of its callers, and a
    /// requirement each of eighty call sites has to remember is not a
    /// requirement.
    ///
    /// **Process-wide, not per-thread, for two reasons** — see
    /// [`TICK_STEP_NANOS`]: a `multi_thread` tokio test's emit loop moves
    /// between worker threads across its awaits, and a cell's `folded_at` is
    /// shared by every thread that folds into it, so uniqueness has to hold
    /// across the process or it does not hold at all.
    ///
    /// **Why having no way back is safe.** This is the real clock with ONE
    /// property added, that it may not repeat. It cannot move any test's notion
    /// of now away from wall time, so it cannot change the outcome of a test
    /// that runs beside it or after it. [`freeze_at`] could, which is why
    /// freezing is thread-local and RAII and has no process-wide form.
    ///
    /// Idempotent. One microsecond, and not one millisecond, because a test that
    /// drives the daemon shares this clock with it: at 1 µs even a million reads
    /// move the clock one second, so nothing drifts far enough to disturb a reset
    /// window (`reset::RESET_SLACK` is 60 s). It is still enough to make the
    /// RFC3339 text strictly ordered, which is all the ledger needs.
    pub fn ticking() {
        // Seed the watermark from the real clock before arming, so the first
        // read cannot be handed an instant from 1970.
        let _ = LAST_NANOS.fetch_max(
            Utc::now().timestamp_nanos_opt().unwrap_or(0),
            Ordering::Relaxed,
        );
        TICK_STEP_NANOS.store(1_000, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── the seam itself ──────────────────────────────────────────────────

    /// **THE PRODUCTION CONTRACT: the seam is REAL TIME, not an invented era.**
    ///
    /// The tighter assertion this started as — `before <= seam <= after` around
    /// two raw `Utc::now()` reads — cannot be made honestly any more, and saying
    /// why is the point. [`testing::ticking`] is process-wide and one-way, and
    /// `budgets::tests::svc()` arms it, so by the time this test runs in the
    /// `--lib` binary the ticking clock may well be on. A ticking clock can sit a
    /// few microseconds AHEAD of the raw clock — that is exactly the property it
    /// exists to have — so the upper bracket would be a coin flip, and a coin
    /// flip is the thing this whole change is about removing.
    ///
    /// What is asserted instead is the property that actually matters in
    /// production: the seam answers wall time. One second is orders of magnitude
    /// above any lead a one-microsecond step can accumulate in a test binary, and
    /// orders of magnitude below a fake epoch. The exactness of the no-override
    /// arm is held by
    /// [`tests::the_five_in_seam_files_hold_no_direct_wall_clock_read`] instead,
    /// which proves this module holds the crate's ONLY wall-clock read.
    #[test]
    fn with_no_override_the_seam_is_the_real_clock() {
        let real = Utc::now();
        let seam = now();
        let drift = (seam - real).abs();
        assert!(
            drift < chrono::Duration::seconds(1),
            "the seam is not tracking wall time: real {real:?}, seam {seam:?}, \
             drift {drift}"
        );
    }

    /// **THE DEFECT, REPRODUCED AT THE SEAM.** Two reads of a frozen clock are
    /// identical — which is exactly what an optimized build does to two reads of
    /// the real one, and what the rest of this change exists to stop being an
    /// accident.
    #[test]
    fn a_frozen_clock_answers_the_same_instant_twice() {
        let at = Utc::now();
        let _c = testing::freeze_at(at);
        assert_eq!(now(), at);
        assert_eq!(now(), at, "frozen means frozen, however many reads");
        assert_eq!(now(), now());
    }

    /// The property the whole class depends on: no two reads are ever equal, and
    /// the order is the read order.
    #[test]
    fn a_ticking_clock_never_repeats_however_tight_the_loop() {
        testing::ticking();
        let reads: Vec<_> = (0..10_000).map(|_| now()).collect();
        for pair in reads.windows(2) {
            assert!(
                pair[0] < pair[1],
                "two reads collided: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
        // …and the RFC3339 TEXT is strictly ordered too, because that — not the
        // `DateTime` — is what the ledger stores and SQL compares.
        let text: Vec<String> = reads.iter().map(|t| t.to_rfc3339()).collect();
        let mut sorted = text.clone();
        sorted.sort();
        assert_eq!(text, sorted, "the serialized form must order as the reads did");
        assert_eq!(
            text.iter().collect::<std::collections::BTreeSet<_>>().len(),
            text.len(),
            "and no two serialized reads may be the same string"
        );
    }

    /// **AND IT HOLDS ACROSS THREADS, which is why it is not a thread-local.**
    ///
    /// This is the assertion the per-thread version could not make, and the
    /// reason it was replaced: a cell's `folded_at` is shared by every thread
    /// that folds into it, and a `multi_thread` tokio test's emit loop moves
    /// between workers across its awaits. Eight threads reading as fast as they
    /// can must still produce 8,000 DISTINCT instants.
    #[test]
    fn a_ticking_clock_never_repeats_across_threads_either() {
        testing::ticking();
        let hands: Vec<_> = (0..8)
            .map(|_| std::thread::spawn(|| (0..1_000).map(|_| now()).collect::<Vec<_>>()))
            .collect();
        let all: Vec<_> = hands.into_iter().flat_map(|h| h.join().unwrap()).collect();
        let distinct: std::collections::BTreeSet<_> = all.iter().collect();
        assert_eq!(
            distinct.len(),
            all.len(),
            "{} of {} reads collided across threads",
            all.len() - distinct.len(),
            all.len()
        );
    }

    /// A FREEZE belongs to the thread that installed it — the half that is still
    /// thread-local, and must be, because `cargo test` runs these in parallel and
    /// a frozen clock WOULD change a neighbour's outcome.
    #[test]
    fn a_freeze_does_not_reach_another_thread() {
        let at = "2020-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let _c = testing::freeze_at(at);
        assert_eq!(now(), at);
        let elsewhere = std::thread::spawn(now).join().unwrap();
        assert!(
            elsewhere > at,
            "another thread read the freezing thread's clock: {elsewhere:?}"
        );
    }

    /// And it does not outlive the scope that asked for it, even on the same
    /// thread — which is what stops one test's freeze reaching the next.
    #[test]
    fn the_freeze_is_gone_when_the_guard_drops() {
        let at = "2020-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        {
            let _c = testing::freeze_at(at);
            assert_eq!(now(), at);
        }
        assert!(now() > at, "the guard's Drop did not lift the freeze");
    }

    /// **NESTING RESTORES, IT DOES NOT CLEAR.** A freeze inside a ticking test
    /// freezes its own scope and gives the ticking clock back — which is what
    /// `budgets::tests`' degenerate-window test is built on: the fixture ticks,
    /// and one statement collides on purpose.
    #[test]
    fn a_freeze_nested_inside_a_ticking_clock_gives_the_ticking_one_back() {
        testing::ticking();
        let a = now();
        let b = now();
        assert!(a < b, "the process clock is not ticking");
        {
            let _inner = testing::freeze_at(a);
            assert_eq!(now(), a);
            assert_eq!(now(), a, "the inner freeze is not frozen");
        }
        let c = now();
        let d = now();
        assert!(
            c > a && d > c,
            "the ticking clock was not restored after the freeze: {c:?} then {d:?}"
        );
    }

    /// The fixture form: idempotent, and it never overwrites a FREEZE the test
    /// chose for itself — which is what makes a `freeze_at` inside a `svc()`
    /// fixture mean what it reads.
    #[test]
    fn the_fixture_form_is_idempotent_and_never_overwrites_a_freeze() {
        testing::ticking();
        let a = now();
        let b = now();
        assert!(a < b, "the fixture form did not arm a ticking clock");

        let at = "2020-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let _c = testing::freeze_at(at);
        testing::ticking();
        assert_eq!(
            now(),
            at,
            "the fixture form trampled a freeze the test had chosen"
        );
    }

    /// `now_local` is ONE read converted, not a second clock. The bug
    /// `prose_to_rfc3339`'s docstring records is unrepresentable through it.
    #[test]
    fn the_local_view_is_the_same_instant_and_not_a_second_read() {
        let at = Utc::now();
        let _c = testing::freeze_at(at);
        assert_eq!(now_local().with_timezone(&Utc), at);
        assert_eq!(now_local(), now_local());
    }

    // ── the guard: the seam cannot be bypassed inside the five files ─────

    /// The banned primitives, assembled at RUNTIME.
    ///
    /// `concat!` and not a literal so that the strings this guard hunts for
    /// never appear in this file as the thing being banned. A source-scanning
    /// ban that can be satisfied by the comment explaining it has happened
    /// before in this repo; the needles are built from halves so it cannot
    /// happen here, and the scan skips this file anyway.
    fn banned() -> Vec<String> {
        vec![
            concat!("Utc", "::now()").to_string(),
            concat!("Local", "::now()").to_string(),
        ]
    }

    /// One source file, with `//` comment tails removed.
    ///
    /// Stripping is not optional: `gauge.rs`'s own docstring NAMES both banned
    /// primitives while explaining the bug they caused, and a scan that counted
    /// prose as code would be red on a file that is clean. The `"` parity test
    /// keeps a `//` inside a string literal from truncating a line that has real
    /// code after it.
    fn code_only(src: &str) -> String {
        src.lines()
            .map(|line| {
                let b = line.as_bytes();
                let mut quotes = 0usize;
                let mut i = 0;
                while i < b.len() {
                    match b[i] {
                        b'\\' => i += 1,
                        b'"' => quotes += 1,
                        b'/' if quotes % 2 == 0 && b.get(i + 1) == Some(&b'/') => {
                            return &line[..i];
                        }
                        _ => {}
                    }
                    i += 1;
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Every line of `src` that reads a wall clock directly, 1-based.
    fn direct_reads(src: &str) -> Vec<(usize, String)> {
        let needles = banned();
        code_only(src)
            .lines()
            .enumerate()
            .filter_map(|(i, line)| {
                needles
                    .iter()
                    .find(|n| line.contains(n.as_str()))
                    .map(|n| (i + 1, format!("{n}  ->  {}", line.trim())))
            })
            .collect()
    }

    /// `src/<file>` read off disk at test time, so the guard sees the tree as it
    /// is rather than as it was when this binary was compiled.
    fn read_in_seam(rel: &str) -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("{} is in IN_SEAM but unreadable: {e}", p.display()))
    }

    /// **THE GUARD IS NOT VACUOUS.** Three ways it could pass while proving
    /// nothing, each closed:
    ///
    /// 1. it could scan nothing — the file list is asserted non-empty and every
    ///    entry is asserted to be real, substantial source;
    /// 2. its matcher could never match — a planted violation is asserted to be
    ///    FOUND, with its line number;
    /// 3. its comment stripper could eat everything — a line whose banned read
    ///    sits after a string containing `//` is asserted to be found, and a
    ///    line where the read is genuinely inside a comment is asserted NOT to
    ///    be.
    #[test]
    fn the_guard_finds_a_planted_read_and_scans_real_files() {
        assert!(!IN_SEAM.is_empty(), "the seam's file list is empty");
        for rel in IN_SEAM {
            let src = read_in_seam(rel);
            assert!(
                src.len() > 2_000,
                "{rel} is {} bytes — the guard is scanning a stub",
                src.len()
            );
            assert!(
                src.contains("fn "),
                "{rel} holds no function — the guard is scanning the wrong thing"
            );
        }

        let planted = format!(
            "fn f() {{\n    let a = {};\n}}\n",
            concat!("Utc", "::now()")
        );
        let hits = direct_reads(&planted);
        assert_eq!(
            hits.len(),
            1,
            "a planted direct read was not caught: {hits:?}"
        );
        assert_eq!(hits[0].0, 2, "the guard reports the wrong line: {hits:?}");

        let planted_local = format!("let b = {};", concat!("Local", "::now()"));
        assert_eq!(
            direct_reads(&planted_local).len(),
            1,
            "the local-zone primitive is not banned"
        );

        // After a string that contains `//` — the stripper must not truncate here.
        let after_url = format!(
            "    let u = \"https://x/y\"; let t = {};",
            concat!("Utc", "::now()")
        );
        assert_eq!(
            direct_reads(&after_url).len(),
            1,
            "the comment stripper swallowed real code after a URL literal"
        );

        // …and prose is prose. `gauge.rs` depends on this.
        let in_prose = format!("/// 1. `{}` and the drift it caused.", concat!("Utc", "::now()"));
        assert!(
            direct_reads(&in_prose).is_empty(),
            "a docstring naming the primitive is being read as code"
        );
    }

    /// **THE SEAM CANNOT BE BYPASSED INSIDE THE FIVE FILES.**
    ///
    /// Clause 2 of the rule in this module's docs, mechanically. A new direct
    /// `Utc::now()` or `Local::now()` anywhere in [`IN_SEAM`] — production code
    /// or test code — fails here, with the file and line.
    ///
    /// Test code is NOT exempt, on purpose. A test in one of these files that
    /// reads the real clock is the exact defect this change is for: three of the
    /// five tests that failed under optimization were unit tests in
    /// `budgets.rs`.
    ///
    /// This file is not in the list and does not scan itself. The one legal
    /// wall-clock read in the crate's compared-instant path is [`now`]'s, and it
    /// is here.
    #[test]
    fn the_five_in_seam_files_hold_no_direct_wall_clock_read() {
        let mut offences = Vec::new();
        for rel in IN_SEAM {
            for (line, what) in direct_reads(&read_in_seam(rel)) {
                offences.push(format!("{rel}:{line}: {what}"));
            }
        }
        assert!(
            offences.is_empty(),
            "a direct wall-clock read inside the seam — these files mint instants \
             this crate later COMPARES, so the read must go through \
             `crate::clock::now()` (or `now_local()`). See `src/clock.rs`'s docs \
             for which side a call site belongs on:\n  {}",
            offences.join("\n  ")
        );
    }

    /// **THE TEST HOOK IS NOT A PRODUCTION PATH.** [`testing`] has to be `pub`
    /// so `tests/*.rs` can reach it; that is the only reason, and this is the
    /// fence around it. No file under `src/` may install a clock outside its own
    /// `mod tests`.
    #[test]
    fn no_production_path_installs_a_test_clock() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs(&root, &mut files);
        assert!(
            files.len() > 20,
            "only {} source files found — this scan walked the wrong tree",
            files.len()
        );

        let needle = concat!("clock", "::testing");
        let mut offences = Vec::new();
        let mut scanned_a_file_mentioning_the_seam = false;
        for path in &files {
            let src = std::fs::read_to_string(path).unwrap();
            if src.contains(concat!("clock", "::now()")) {
                scanned_a_file_mentioning_the_seam = true;
            }
            // Everything from the file's own `mod tests` onwards is test code.
            let code = match src.find("\nmod tests {") {
                Some(i) => &src[..i],
                None => &src[..],
            };
            if path.file_name().unwrap() == "clock.rs" {
                continue; // the hook's own home
            }
            for (i, line) in code_only(code).lines().enumerate() {
                if line.contains(needle) {
                    offences.push(format!(
                        "{}:{}: {}",
                        path.strip_prefix(&root).unwrap().display(),
                        i + 1,
                        line.trim()
                    ));
                }
            }
        }
        assert!(
            scanned_a_file_mentioning_the_seam,
            "no scanned file calls the seam at all — this scan proves nothing"
        );
        assert!(
            offences.is_empty(),
            "a production path installs a test clock:\n  {}",
            offences.join("\n  ")
        );
    }

    /// **CLAUSE 3: A TEST THAT DRIVES THE HUB IS ON THE SEAM.**
    ///
    /// `Hub::emit` stamps with the clock and every cell refuses an input at or
    /// before its `folded_at`, so two emits in one microsecond mean the second
    /// is DROPPED. That is not a rounding error, it is a lost input, and an
    /// optimized build does it to any tight emit loop. At `290113e` it took
    /// `recent_updates`, `retention` and `lag_repair` red — but only SOMETIMES,
    /// and only under load, which is why one baseline run named two of them and
    /// repeated runs named five.
    ///
    /// So the requirement is on the FILE and not on the test: a `tests/*.rs`
    /// that emits through the hub installs the seam in its fixture, and then
    /// every test in it — including the next one anybody writes — is covered
    /// without being told. Enforced here rather than trusted, because "remember
    /// to add a line" is exactly the discipline that produced the defect.
    #[test]
    fn every_hub_emitting_test_file_installs_the_clock_seam() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        let mut files = Vec::new();
        collect_rs(&dir, &mut files);
        assert!(
            files.len() > 50,
            "only {} integration test files found — this scan walked the wrong tree",
            files.len()
        );

        let emits = concat!("hub", ".emit");
        let installs = concat!("clock::testing::", "ticking()");
        let mut drivers = 0;
        let mut offences = Vec::new();
        for path in &files {
            let src = std::fs::read_to_string(path).unwrap();
            let code = code_only(&src);
            if !code.contains(emits) {
                continue;
            }
            drivers += 1;
            if !code.contains(installs) {
                offences.push(path.file_name().unwrap().to_string_lossy().to_string());
            }
        }
        assert!(
            drivers >= 10,
            "only {drivers} test files were found to drive the hub — the matcher is \
             broken, so a pass here proves nothing"
        );
        assert!(
            offences.is_empty(),
            "{} test file(s) emit through the hub without installing the clock seam. \
             Two emits in one microsecond are one DROPPED input, so a tight emit loop \
             in these files is a coin flip under optimization. Add \
             `threads::clock::testing::ticking();` to the fixture that \
             builds the manager; see `src/clock.rs`:\n  {}",
            offences.len(),
            offences.join("\n  ")
        );
    }

    fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                collect_rs(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
}
