//! **A TRANSCRIPT CARRY MUST NOT PARK A TOKIO WORKER.**
//!
//! `accounts::port_transcript_file` is `std::fs` from end to end, and the
//! volumes it moves are not small: this daemon's log records 61 sidecar carries
//! totalling 2.10 GB, the largest a single 207,942,818-byte carry across 457
//! files, and a 159 MB one landing inside the 2026-09-11 stall window. It is
//! reached from two places and BOTH of them are on the runtime — the
//! `threads.reconcile_transcript` verb out of `server::handle`, and the
//! account-switch path `admit_or_park` → `move_thread` → `port_conversation`.
//! Neither offloaded it, so a carry took one of ten workers out of service for
//! its whole duration (`reports/0093-stability-threads.md`, F4).
//!
//! **This is a CONCURRENCY property, so it is measured with more than one task
//! and on a runtime with exactly one worker.** One worker is the whole point:
//! with N workers a blocked one is hidden by its siblings, and "did the other
//! task make progress" stops being a question about the fix and becomes a
//! question about how many cores the machine running the test has. At
//! `worker_threads = 1` the answer is unambiguous — either the copy handed the
//! worker over or nothing else in the process ran.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use threads::accounts::{port_transcript_file, projects_dir_for};

fn claude_home() -> PathBuf {
    std::env::temp_dir().join("threads-tests-offload-home")
}

fn uniq() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

/// A transcript plus the sidecar directory beside it — the shape that actually
/// costs: `files` files of `each` bytes, which is how a 457-file carry is built.
fn plant_with_sidecar(
    config_dir: &str,
    home: &Path,
    slug: &str,
    session: &str,
    files: usize,
    each: usize,
) -> PathBuf {
    let dir = projects_dir_for(Some(config_dir), home).join(slug);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{session}.jsonl"));
    std::fs::write(&path, "{\"type\":\"user\"}\n".repeat(64)).unwrap();
    let side = dir.join(session).join("subagents");
    std::fs::create_dir_all(&side).unwrap();
    let blob = vec![b'x'; each];
    for i in 0..files {
        std::fs::write(side.join(format!("sub-{i}.jsonl")), &blob).unwrap();
    }
    path
}

/// The ticker's period. Small enough that a copy taking a few hundred
/// milliseconds should produce dozens of ticks, and large enough that it is
/// not itself a busy loop.
const TICK: Duration = Duration::from_millis(2);

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_sidecar_carry_lets_the_rest_of_the_runtime_keep_running() {
    let tmp = tempfile::tempdir().unwrap();
    let home = claude_home();
    let session = format!("sess-{}", uniq());
    let slug = format!("-tmp-offload-{}", uniq());
    let src_store = tmp.path().join("src").to_string_lossy().to_string();
    let dst_store = tmp.path().join("dst").to_string_lossy().to_string();

    // ~48 MB across 384 files — a quarter of the largest carry this daemon has
    // actually done, chosen so the test costs a fraction of a second rather
    // than the two seconds the real one would.
    let source = plant_with_sidecar(&src_store, &home, &slug, &session, 384, 128 * 1024);

    // THE SECOND TASK. It does nothing but prove the runtime is alive.
    let ticks = Arc::new(AtomicU64::new(0));
    let t = ticks.clone();
    let ticker = tokio::spawn(async move {
        loop {
            tokio::time::sleep(TICK).await;
            t.fetch_add(1, Ordering::SeqCst);
        }
    });
    // Let the ticker actually start on the single worker before we measure, so
    // a zero can only mean "blocked", never "never scheduled".
    tokio::time::sleep(Duration::from_millis(20)).await;
    let before = ticks.load(Ordering::SeqCst);

    let started = Instant::now();
    let home2 = home.clone();
    let session2 = session.clone();
    let dst2 = dst_store.clone();
    // Driven through `spawn` rather than inline, because inline `.await`-less
    // work in the test body is not what production does: production calls this
    // from inside a task on the worker, which is exactly what this is.
    let carry = tokio::spawn(async move {
        port_transcript_file(&source, Some(&dst2), &home2, &session2).unwrap()
    });
    let outcome = carry.await.unwrap();
    let elapsed = started.elapsed();
    let during = ticks.load(Ordering::SeqCst) - before;
    ticker.abort();

    assert_eq!(outcome.as_str(), "ported", "the carry itself must still work");
    assert!(
        std::fs::read_dir(
            projects_dir_for(Some(&dst_store), &home)
                .join(&slug)
                .join(&session)
                .join("subagents")
        )
        .unwrap()
        .count()
            == 384,
        "every sidecar file must land — the offload is not allowed to cost content"
    );

    // The copy has to have taken long enough for the question to mean
    // anything. If the machine copied 48 MB in under 20 ms there is nothing
    // here to prove and the test says so rather than passing vacuously.
    assert!(
        elapsed >= Duration::from_millis(20),
        "the carry finished in {elapsed:?}, which is too fast to be evidence \
         about blocking; raise the fixture size"
    );

    // THE PROPERTY. Ticks are 2 ms apart, so a copy of `elapsed` should yield
    // roughly `elapsed / 2ms` of them. A QUARTER of that is the floor, and it
    // is deliberately far below what the fix actually achieves (measured: 18
    // ticks in 68 ms against a floor of 8): this asserts "the runtime kept
    // running", not a scheduling rate, and the number it has to separate
    // itself from is ZERO — on one worker a `std::fs` copy inside a task is
    // the only thing that thread can be doing.
    let expected = (elapsed.as_millis() as u64 / TICK.as_millis() as u64) / 4;
    assert!(
        during >= expected.max(3),
        "the runtime made {during} ticks while a {elapsed:?} transcript carry \
         ran; at least {} were expected. A carry that blocks the worker it is \
         on takes that share of the daemon's capacity to answer ANYTHING out \
         of service for its whole duration.",
        expected.max(3),
    );
}
