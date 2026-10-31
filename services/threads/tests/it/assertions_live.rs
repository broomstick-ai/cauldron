//! ASSERTIONS-ARE-LIVE — the proof that moving this daemon to a release build
//! did not silently delete its invariant guards.
//!
//! WHY THIS FILE EXISTS AS A TEST RATHER THAN AS A NOTE IN THE MANIFEST. The
//! daemon used to run out of `target/debug/threads`, which was slow enough to
//! stall health probes. The fix was to run a release build with
//! `debug-assertions` and `overflow-checks` turned back ON in
//! `[profile.release]` (see the block at the bottom of the workspace
//! `Cargo.toml`). That is one line of TOML standing between this daemon and a
//! build with NO invariant checking at all, and a `debug_assert!` that has
//! silently vanished is strictly worse than the slow build it replaced: the
//! latency bug was visible in a log, a missing guard is visible only when it
//! has already written a wrong number to the system of record.
//!
//! So the knobs are asserted BEHAVIOURALLY, in the build they are supposed to
//! govern, and not by reading the manifest back.
//!
//! THE `#![cfg(debug_assertions)]` ON THE WHOLE FILE IS THE POINT, not a
//! convenience. It makes this file a DETECTOR with two distinguishable states
//! rather than a test that merely passes:
//!
//!   * built with debug assertions on (`cargo build`/`test`, or a release build
//!     carrying our `[profile.release]`) — the tests below EXIST and must PASS;
//!   * built with debug assertions off (a plain `[profile.release]` with the
//!     block deleted) — this file compiles to NOTHING and the binary reports
//!     `0 tests`.
//!
//! `0 tests` is therefore the signature of the regression we are guarding
//! against, and "the tests passed" cannot be confused with "the tests were
//! compiled out", which is the failure mode a plain `assert!(cfg!(..))` has.
//!
//! WHY `catch_unwind` AND NOT `#[should_panic]`. `#[should_panic]` would also
//! pass if the function panicked for some unrelated reason. Catching the unwind
//! around ONE expression proves that THAT expression is what panicked, and it
//! lets both halves of the invariant be checked in one test body. It does mean
//! this file depends on `panic = "unwind"`, which the manifest block commits to
//! keeping for exactly this reason.

#![cfg(debug_assertions)]

use std::hint::black_box;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Run `f`, swallowing the panic message so a passing run does not print a
/// backtrace that looks like a failure. Returns true if `f` panicked.
///
/// The hook is restored before returning, so one test cannot silence another.
fn panics(f: impl FnOnce()) -> bool {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    outcome.is_err()
}

/// `debug_assert!` must still PANIC in this build.
///
/// This is the knob the four real guards in this daemon's dependency closure
/// depend on — `threads/src/server.rs`, `threads/src/budgets.rs`,
/// `shapewire/src/hub.rs` and `shapewire/src/server.rs`. Each of those asserts
/// an arm that is supposed to be unreachable or an exclusion that is supposed
/// to have already happened, so each of them is a no-op in a build where this
/// test would fail.
#[test]
fn debug_assert_still_panics() {
    assert!(
        panics(|| debug_assert!(black_box(false), "proof: debug_assert! is live")),
        "debug_assert!(false) did NOT panic, yet this file was compiled, which \
         means cfg(debug_assertions) is set while the macro is inert — the four \
         invariant guards in this daemon are silently no-ops"
    );
}

/// `debug_assert_eq!` must still panic too.
///
/// Checked separately because it is a different macro with its own expansion;
/// `lib/telemetry/src/estimator.rs` uses this form rather than the plain one.
#[test]
fn debug_assert_eq_still_panics() {
    assert!(
        panics(|| debug_assert_eq!(black_box(1u32), black_box(2u32))),
        "debug_assert_eq! did NOT panic in a build that has cfg(debug_assertions) set"
    );
}

/// Arithmetic overflow must PANIC, not wrap.
///
/// `overflow-checks` has no `cfg` of its own, so this test is gated on
/// `debug_assertions` and in doing so encodes the invariant the manifest block
/// actually wants: *a build of this workspace that claims debug assertions must
/// also have overflow checks live.* The two knobs are set together there and
/// this is what stops one of them being dropped without the other.
///
/// `black_box` on both operands is load-bearing. Without it `255u8 + 1` is a
/// const-evaluable expression and rustc rejects it at compile time with the
/// deny-by-default `arithmetic_overflow` lint — which would make this a
/// compile error rather than the runtime check we are trying to observe.
#[test]
fn arithmetic_overflow_panics_instead_of_wrapping() {
    assert!(
        panics(|| {
            let a: u8 = black_box(u8::MAX);
            let b: u8 = black_box(1);
            black_box(a + b);
        }),
        "u8::MAX + 1 WRAPPED instead of panicking — overflow-checks is off in \
         this build, so every wrapping subtraction in the budget and cost \
         arithmetic now writes a wrong number instead of crashing"
    );
}

/// The same check on the subtraction side, which is the shape that actually
/// occurs in this codebase: an unsigned `a - b` where `b` turns out to exceed
/// `a` underflows to a huge number and is then written down as a cost or a
/// remaining budget.
#[test]
fn unsigned_underflow_panics_instead_of_wrapping() {
    assert!(
        panics(|| {
            let spent: u64 = black_box(0);
            let refund: u64 = black_box(1);
            black_box(spent - refund);
        }),
        "0u64 - 1 WRAPPED to u64::MAX instead of panicking — overflow-checks is off"
    );
}
