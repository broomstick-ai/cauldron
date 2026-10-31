//! # The `warnings` shape — the producer's self-report, proved at a consumer
//!
//! The operator's instruction: *"For TUI and the threads tui can you display the
//! number of warnings coming from threads so it annoys me? 27k is incredible."*
//! And the ruling that shaped the design: *"reporting itself means it needs to
//! send a warning to its consumer until we have centralized cauldron warning
//! aggregation."*
//!
//! `threads::warnings`' own unit tests prove the COUNTER and the disk record.
//! This file proves the half that matters to him — that the number **reaches a
//! consumer**, which is the whole respect in which 29,644 log lines failed.
//!
//! **EVERY ASSERTION IS ON A FRAME A CONNECTION RECEIVED**, never on hub-internal
//! state, and never on the counter. Proving a mechanism ran is not proving
//! anything arrived; a counter proved only at the producer is half the feature.
//!
//! **EVERY TEST WAS FORCED RED** by the mutation named in its own docstring, and
//! the red text is quoted there.
//!
//! The counts are read as DELTAS, never as absolutes. `COUNTER` is a
//! process-global and `cargo test` runs this binary's tests on many threads, so an
//! absolute count is a shared mutable number three other tests are also moving.
//! There is deliberately no reset: a counter an integration test can zero is a
//! counter whose history is deniable.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use threads::config::Config;
use threads::event::Fact;
use threads::shape::{ConnFrames, Derived, ShapeFrame};
use threads::warnings::COUNTER;
use threads::{build_manager, ThreadManager};

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

fn config(tmp: &std::path::Path) -> Config {
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    Config {
        data_dir: Some(tmp.to_string_lossy().to_string()),
        accounts: Default::default(),
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

async fn start() -> (ThreadManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    // THE CLOCK SEAM, which landed (`d1a34e3`) after this file was written. Every
    // `Hub::emit` stamps with the clock and every cell refuses an input at or
    // before its `folded_at`, so two emits in one microsecond mean the second is
    // DROPPED — which is what an optimized build does to a tight emit loop. This
    // shape samples at 1 Hz in production but its tests emit back to back, so it
    // is exactly the shape that needs it. See `src/clock.rs`.
    threads::clock::testing::ticking();
    let manager = build_manager(config(tmp.path())).unwrap();
    (manager, tmp)
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
}

fn drain(f: &mut ConnFrames) -> Vec<Arc<ShapeFrame>> {
    let mut all = Vec::new();
    while let Ok(x) = f.try_recv() {
        all.push(x);
    }
    all
}

/// The EXPORT that answered a subscribe — its `row`.
fn exported_row(frames: &[Arc<ShapeFrame>]) -> Option<Value> {
    frames
        .iter()
        .find(|f| f.wire.row.is_some())
        .map(|f| f.wire.row.clone().unwrap())
}

/// The export's per-cell provenance, so `Known(0)` can be told from "absent".
fn exported_cells(frames: &[Arc<ShapeFrame>]) -> Option<Value> {
    frames
        .iter()
        .find(|f| f.wire.cells.is_some())
        .map(|f| f.wire.cells.clone().unwrap())
}

/// The export's `shared` block.
fn exported_shared(frames: &[Arc<ShapeFrame>]) -> Option<Value> {
    frames
        .iter()
        .find(|f| f.wire.shared.is_some())
        .map(|f| f.wire.shared.clone().unwrap())
}

/// The `warn` off every LIVE `row` frame, in arrival order. The live path only —
/// an export answering a subscribe is not a move.
fn live_warn_counts(frames: &[Arc<ShapeFrame>]) -> Vec<u64> {
    frames
        .iter()
        .filter_map(|f| match &f.wire.event {
            Some(Derived::Row { row }) => row["warn"].as_u64(),
            _ => None,
        })
        .collect()
}

/// Install the counting layer on THIS thread and emit `n` real warnings through
/// it, exactly as production does — no counter poking.
fn emit_real_warnings(n: usize) {
    use tracing_subscriber::layer::SubscriberExt;
    let sub = tracing_subscriber::registry().with(threads::warnings::WarningCounter);
    tracing::subscriber::with_default(sub, || {
        for i in 0..n {
            tracing::warn!(i, "a deliberate test warning from the shape suite");
        }
    });
}

// (The WS client this file carried went with
// `the_legacy_door_and_the_shape_report_one_counter`, R4.S9.)

/// **SERIALISES EVERY TEST IN THIS FILE**, because they share one process-global
/// counter and `cargo test` runs a binary's tests in parallel.
///
/// Learned by watching `the_legacy_door_and_the_shape_report_one_counter` fail with
/// `10 != 8`: the door and the shape were each reading the counter correctly, and
/// four warnings from a CONCURRENT test landed between the two reads. The race was
/// in the test, not in the code — but a suite that reports a real agreement as a
/// disagreement is worse than one that does not check, because the next person
/// spends an hour on the wrong file.
fn gate() -> &'static tokio::sync::Mutex<()> {
    static G: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    G.get_or_init(|| tokio::sync::Mutex::new(()))
}

const SHAPE: &str = "warnings";
const KEY: &str = threads::shapes::warnings::KEY_DAEMON;

// ─────────────────────────────────────────────────────────────────────────────
// 1. Zero reads as zero, at the consumer
// ─────────────────────────────────────────────────────────────────────────────

/// **THE EXPORT CARRIES A `Known` ZERO, NOT AN ABSENT CELL.**
///
/// *"Prove zero reads as zero"*, at the consumer rather than at the counter. A
/// shape that omitted the cell until something went wrong could not distinguish
/// *"nothing has gone wrong"* from *"this daemon does not count warnings"* — and
/// the three-valued cell state exists precisely so a consumer never has to guess
/// which.
///
/// Hiding a zero is the FACE's job and both faces do it; the producer's job is to
/// be honest.
///
/// **THE MUTATION.** In `shapes/warnings.rs`, make `hydrates` skip the apply:
/// delete the `root.apply(as_of, &t, true);` line.
///
/// Forced red:
/// ```text
/// the shape exports the daemon key at hydrate: None
/// ```
#[tokio::test]
async fn zero_exports_as_a_known_zero_rather_than_an_absent_cell() {
    let _gate = gate().lock().await;
    let (m, tmp) = start().await;
    m.hub
        .register(threads::shapes::warnings::Warnings, m.clone());

    let mut rx = m.hub.connect(301);
    m.hub.subscribe(SHAPE, Some(KEY), 301, None).unwrap();
    settle().await;
    let frames = drain(&mut rx);

    let row = exported_row(&frames);
    assert!(
        row.is_some(),
        "the shape exports the daemon key at hydrate: {row:?}"
    );
    let row = row.unwrap();
    assert_eq!(row["producer"], KEY, "keyed by the producer: {row}");
    assert!(
        row["warn"].is_u64() && row["error"].is_u64(),
        "both totals are present as numbers even at zero: {row}"
    );

    // **THE LOAD-BEARING HALF.** A zero that arrives in state `not_hydrated` is
    // not a zero, it is an unknown wearing one.
    let cells = exported_cells(&frames).expect("the export carries provenance");
    for name in ["warn", "error", "kinds", "sink_errors"] {
        assert_eq!(
            cells[name]["state"], "known",
            "cell `{name}` is KNOWN, not an unknown painted as zero: {cells}"
        );
    }
    drop(tmp);
}

/// **`counts_since` IS THE DAEMON'S START, NOT THE SHAPE'S EPOCH.**
///
/// The shape is `Lazy`, so its epoch re-inits every time the last subscriber
/// leaves, while the process-global counters keep climbing straight through. A
/// consumer that read `epoch.init_at` as the counting window would under-report by
/// everything that happened while nothing was watching — and *"a count that
/// silently resets is a lie about 29,644"*.
///
/// **THE MUTATION.** In `shapes/warnings.rs`'s `hydrates`, pass the epoch instead:
/// `root.set_shared(as_of, &crate::store::now_rfc3339(), t.sink_attached)`.
///
/// Forced red:
/// ```text
/// counts_since is the daemon's own started_at, so a Lazy re-init does not read
/// as a reset: "2026-10-06T21:41:07.883Z" != "2026-10-06T21:41:08.040Z"
/// ```
#[tokio::test]
async fn counts_since_is_the_daemons_start_and_not_the_shapes_epoch() {
    let _gate = gate().lock().await;
    let (m, tmp) = start().await;
    m.hub
        .register(threads::shapes::warnings::Warnings, m.clone());

    let mut rx = m.hub.connect(302);
    m.hub.subscribe(SHAPE, Some(KEY), 302, None).unwrap();
    settle().await;
    let shared = exported_shared(&drain(&mut rx)).expect("the export carries shared");

    assert_eq!(
        shared["counts_since"].as_str(),
        Some(m.started_at.as_str()),
        "counts_since is the daemon's own started_at, so a Lazy re-init does not \
         read as a reset: {:?} != {:?}",
        shared["counts_since"].as_str(),
        Some(m.started_at.as_str())
    );
    assert_eq!(shared["key"], KEY, "and shared names the one key: {shared}");
    assert_eq!(
        shared["sink_dir"], "threads.warnings",
        "and says where the disk record lives, so no consumer hardcodes it: {shared}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. A warning moves the number — the direction that matters
// ─────────────────────────────────────────────────────────────────────────────

/// **A SAMPLE MOVES THE NUMBER AT A CONSUMER HOLDING THE KEY.**
///
/// The other direction of *"prove zero reads as zero and a warning makes it
/// non-zero"*, asserted on a LIVE frame a connection received.
///
/// **THE MUTATION.** In `shapes/warnings.rs`'s `folds`, make the
/// `WarningsSampled` arm a no-op (`{}`) like the others.
///
/// Forced red:
/// ```text
/// a sample through the FIFO is a live row frame carrying the new total: []
/// ```
#[tokio::test]
async fn a_sample_moves_the_number_at_a_consumer_holding_the_key() {
    let _gate = gate().lock().await;
    let (m, tmp) = start().await;
    m.hub
        .register(threads::shapes::warnings::Warnings, m.clone());

    let mut rx = m.hub.connect(303);
    m.hub.subscribe(SHAPE, Some(KEY), 303, None).unwrap();
    settle().await;
    let base = exported_row(&drain(&mut rx)).expect("hydrate export")["warn"]
        .as_u64()
        .unwrap();

    // Three real warnings through the real layer, then the sample the production
    // task would make.
    emit_real_warnings(3);
    m.hub.emit_fact(Fact::WarningsSampled {
        tally: COUNTER.tally(),
    });
    settle().await;

    let got = live_warn_counts(&drain(&mut rx));
    assert!(
        !got.is_empty(),
        "a sample through the FIFO is a live row frame carrying the new total: {got:?}"
    );
    let last = *got.last().unwrap();
    assert!(
        last >= base + 3,
        "three warnings moved the number the consumer holds by at least three: \
         {base} -> {last}"
    );
    drop(tmp);
}

/// **A SAMPLE THAT REPEATS THE HELD TALLY COSTS NO FRAME.**
///
/// This is what makes a quiet daemon SILENT on this shape rather than one frame per
/// second forever. The sampler emits every second regardless; the fold reads before
/// it reaches the key (`docs/shape-design-guide.md` step 6), so an unchanged tally
/// dirties nothing.
///
/// It is also the reason this is not a cell on `heartbeat`, whose fold must do the
/// exact opposite — there the arrival IS the content and a dedupe would emit one
/// frame and then go silent forever.
///
/// **THE MUTATION.** In `DaemonWarningsRoot::apply`, delete the
/// `if !moved { return false; }` guard — i.e. reach for the key unconditionally.
///
/// Forced red:
/// ```text
/// five identical samples after the first cost ZERO frames: [12, 12, 12, 12]
/// ```
#[tokio::test]
async fn a_sample_that_repeats_the_held_tally_costs_no_frame() {
    let _gate = gate().lock().await;
    let (m, tmp) = start().await;
    m.hub
        .register(threads::shapes::warnings::Warnings, m.clone());

    let mut rx = m.hub.connect(304);
    m.hub.subscribe(SHAPE, Some(KEY), 304, None).unwrap();
    settle().await;
    drain(&mut rx);

    // One real move first, so the test proves SELECTIVITY and not a dead fold.
    emit_real_warnings(1);
    let tally = COUNTER.tally();
    m.hub.emit_fact(Fact::WarningsSampled {
        tally: tally.clone(),
    });
    settle().await;
    let moved = live_warn_counts(&drain(&mut rx));
    assert_eq!(
        moved.len(),
        1,
        "THE POSITIVE CONTROL: a tally that moved IS one frame, so the silence \
         below is a dedupe and not a broken fold: {moved:?}"
    );

    // Now five samples carrying the same numbers. Nothing warned in between.
    for _ in 0..5 {
        m.hub.emit_fact(Fact::WarningsSampled {
            tally: tally.clone(),
        });
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    }
    settle().await;

    let repeats = live_warn_counts(&drain(&mut rx));
    assert!(
        repeats.is_empty(),
        "five identical samples after the first cost ZERO frames: {repeats:?}"
    );
    drop(tmp);
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. End to end, through the production wiring
// ─────────────────────────────────────────────────────────────────────────────

/// **A REAL `warn!` IN THE DAEMON REACHES A SUBSCRIBED CONSUMER, WITH NOTHING IN
/// THIS TEST EMITTING A FACT.**
///
/// The end-to-end obligation. Everything here is the production path:
/// `start_watchers` registers the shape and spawns the sampler task; the warning is
/// a real `tracing::warn!` through the real layer; the fact is emitted by the
/// DAEMON's own timer, not by this test. The only thing the test does is subscribe
/// and wait.
///
/// It is the test that can tell *"the counter works"* from *"the operator will see
/// it"*, which are the two halves 29,644 log lines had exactly one of.
///
/// **THE MUTATION.** In `runtime.rs`, delete the `warnings` sampler's
/// `emit_fact` — the task keeps ticking and publishes nothing, which is precisely
/// the shape of the original defect.
///
/// Forced red:
/// ```text
/// the DAEMON's own sampler published the warning count with nothing in this test
/// emitting a fact: []
/// ```
#[tokio::test]
async fn a_real_warning_reaches_a_consumer_through_the_production_sampler() {
    let _gate = gate().lock().await;
    let (m, tmp) = start().await;
    // THE PRODUCTION WIRING. Registers every shape and spawns every watcher,
    // including the warning sampler.
    m.start_watchers();
    settle().await;

    let mut rx = m.hub.connect(305);
    m.hub
        .subscribe(SHAPE, Some(KEY), 305, None)
        .expect("the registered warnings shape accepts a subscribe");
    settle().await;
    let base = exported_row(&drain(&mut rx)).expect("hydrate export")["warn"]
        .as_u64()
        .unwrap();

    // A real warning, through the real layer. No fact is emitted here.
    emit_real_warnings(2);

    // Two sampler periods plus slack. NOTHING in this test emits anything.
    tokio::time::sleep(std::time::Duration::from_millis(
        threads::shapes::warnings::SAMPLE_INTERVAL_MS * 2 + 400,
    ))
    .await;

    let got = live_warn_counts(&drain(&mut rx));
    assert!(
        !got.is_empty(),
        "the DAEMON's own sampler published the warning count with nothing in \
         this test emitting a fact: {got:?}"
    );
    let last = *got.last().unwrap();
    assert!(
        last >= base + 2,
        "and the number it published had moved by the two real warnings: \
         {base} -> {last}"
    );
    drop(tmp);
}

// **`the_legacy_door_and_the_shape_report_one_counter` RETIRED** (R2.S3c,
// R4.S9, HOLDS R9). It pinned that the `threads/status` request's
// `daemon_warnings` read the same `COUNTER` as this shape. That request is
// retired — no face consumes it (the harness terminal face it was for is not
// running; cauldron's TUI reads shapes) — so there is no second transport left
// for the count to disagree on, and the shape is the one door.

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;

/// **WARN-1 D-F: AN ERROR REACHES A CONSUMER BY NAME — in `last_errors` and in
/// `kinds` — EVEN WHEN FIVE BUSIER WARN SITES FILL THE TOP FIVE.**
///
/// Five warn call sites, each fired more often than the one error, so the old
/// top-5 cut would hide the error's kind. Asserted on the LIVE row frame a
/// connection received. Opposed: the error's message is unique to this run,
/// so a `last_errors` holding some OTHER test's error does not pass.
///
/// **THE MUTATION.** In `DaemonWarningsRoot::row`, drop the `last_errors` key;
/// and separately, in `Counter::tally`, restore `kinds.truncate(TOP_KINDS)`.
#[tokio::test]
async fn an_error_reaches_a_consumer_by_name_past_five_busier_warn_sites() {
    let _gate = gate().lock().await;
    let (m, tmp) = start().await;
    m.hub.register(threads::shapes::warnings::Warnings, m.clone());
    let mut rx = m.hub.connect(304);
    m.hub.subscribe(SHAPE, Some(KEY), 304, None).unwrap();
    settle().await;
    drain(&mut rx);

    let unique = format!("warn-1 d-f error {}", std::process::id());
    {
        use tracing_subscriber::layer::SubscriberExt;
        let sub = tracing_subscriber::registry().with(threads::warnings::WarningCounter);
        tracing::subscriber::with_default(sub, || {
            // Each site fires far more than any count the error kind could
            // reach in this binary, so the five warn kinds outrank it.
            for _ in 0..500 {
                tracing::warn!("d-f busy site one");
                tracing::warn!("d-f busy site two");
                tracing::warn!("d-f busy site three");
                tracing::warn!("d-f busy site four");
                tracing::warn!("d-f busy site five");
            }
            tracing::error!(probe = "d-f", "{}", unique);
        });
    }
    m.hub.emit_fact(Fact::WarningsSampled { tally: COUNTER.tally() });
    settle().await;

    let row = drain(&mut rx)
        .iter()
        .rev()
        .find_map(|f| match &f.wire.event {
            Some(Derived::Row { row }) => Some(row.clone()),
            _ => None,
        })
        .expect("a live row frame");
    let last = row["last_errors"].as_array().expect("last_errors is on the row");
    let mine = last.iter().find(|e| e["message"] == unique.as_str()).unwrap_or_else(|| panic!("the error is not in last_errors: {last:?}"));
    assert_eq!(mine["event_fields"]["probe"], "d-f");
    assert!(mine["line"].as_u64().is_some_and(|l| l > 0) && mine["at"].is_string(), "{mine}");
    let kinds = row["kinds"].as_array().unwrap();
    let warn_kinds = kinds.iter().filter(|k| k["level"] == "warn").count();
    assert!(warn_kinds >= 5, "sentinel: five warn kinds fill the top five: {kinds:?}");
    assert!(
        kinds.iter().any(|k| k["level"] == "error" && k["message"] == unique.as_str()),
        "the error's kind was cut from `kinds` by the top five: {kinds:?}"
    );
    drop(tmp);
}
