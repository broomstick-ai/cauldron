//! A technical failure never silences a sentence, and an outage costs the
//! wait once.
//!
//! > "Never let a sentence block outright, for a technical failure. There's
//! > nothing intrinsic about a sentence that should make it fail. It can fail
//! > transiently, but then if it's being requested again, then it needs to be
//! > fixed." — the operator

mod common;

use std::time::{Duration, Instant};

use common::{calls_of, eventually, hash_of, manager, manager_with, say_calls, state_of, TWO_KEYS};
use tts::queue::Lane;

const ONE_PERMIT: &str = r#"
[global]
max_bytes        = "1536MiB"
max_concurrent   = 1
synth_timeout_ms = 5000
reap_orphans     = false

[keys.content]
mode          = "lru"
max_bytes     = "1GiB"
queueing      = "fifo"
module        = "system"
priority      = 50
min_age_secs  = 0
[keys.content.module_config]
voice = "Samantha"
"#;

fn with_global(extra: &str) -> String {
    TWO_KEYS.replace("synth_timeout_ms = 5000", &format!("synth_timeout_ms = 5000\n{extra}"))
}

// ── a failure is a record, not a gate ─────────────────────────────────────

/// The assertion that matters is the SECOND BACKEND CALL: answering out of
/// the stored failure, or out of anything else, would also return quickly.
#[tokio::test]
async fn a_failed_render_is_synthesized_again_on_the_next_request() {
    let (m, tmp) = manager();
    let text = "the sentence he could not hear";
    common::say_fails(tmp.path(), true);
    assert_eq!(m.request("content", None, Some(text.into()), 5000, "path").await.unwrap_err().code(), Some("synthesis_failed"));
    assert_eq!(state_of(&m, &hash_of(text)).as_deref(), Some("failed"), "the record stays");
    common::say_fails(tmp.path(), false);
    let ok = m.request("content", None, Some(text.into()), 10_000, "path").await.expect("it must speak when asked again");
    assert_eq!((ok["state"].as_str(), ok["from_cache"].as_bool()), (Some("warm"), Some(false)));
    assert_eq!(calls_of(tmp.path(), text), 2, "the retry never reached the backend");
}

/// The same, through the warm sweep the face actually uses.
#[tokio::test]
async fn a_failed_render_is_retried_by_the_warm_sweep_too() {
    let (m, tmp) = manager();
    let text = "a message in his look-ahead window";
    common::say_fails(tmp.path(), true);
    m.register("content", vec![text.into()], Lane::Warm).await.unwrap();
    assert!(eventually(5000, || state_of(&m, &hash_of(text)).as_deref() == Some("failed")).await);
    common::say_fails(tmp.path(), false);
    m.register("content", vec![text.into()], Lane::Warm).await.unwrap();
    assert!(eventually(10_000, || state_of(&m, &hash_of(text)).as_deref() == Some("warm")).await);
    assert_eq!(calls_of(tmp.path(), text), 2);
}

/// Held in the queue by a permit hog so it can be read: the revived row is
/// `queued` with its failure history, and the hog is untouched.
#[tokio::test]
async fn a_failed_row_is_revived_to_queued_with_its_history() {
    let (m, tmp) = manager_with(ONE_PERMIT);
    let poisoned = "the sentence that was poisoned";
    common::say_fails(tmp.path(), true);
    for _ in 0..2 {
        let _ = m.request("content", None, Some(poisoned.into()), 5000, "path").await;
    }
    assert_eq!(m.store.get_render(&hash_of(poisoned)).unwrap().unwrap().fail_count, 2);
    common::say_fails(tmp.path(), false);
    common::say_takes_ms(tmp.path(), 10_000);
    m.register("content", vec!["the permit hog".into()], Lane::Warm).await.unwrap();
    assert!(eventually(5000, || state_of(&m, &hash_of("the permit hog")).as_deref() == Some("synthesizing")).await);
    m.register("content", vec![poisoned.into()], Lane::Warm).await.unwrap();
    let r = m.store.get_render(&hash_of(poisoned)).unwrap().unwrap();
    assert_eq!((r.state.as_str(), r.fail_count, r.fail_code.as_deref()), ("queued", 2, Some("synthesis_failed")));
    assert_eq!(state_of(&m, &hash_of("the permit hog")).as_deref(), Some("synthesizing"));
    common::say_takes_ms(tmp.path(), 0);
}

/// `too_large` refuses ONE attempt: after the client chunks differently the
/// same hash must be allowed to try again.
#[tokio::test]
async fn an_oversized_render_errors_and_gates_no_later_attempt() {
    let (m, tmp) = manager_with(&with_global("max_entry_bytes  = \"4KiB\""));
    let text = "a paragraph he had not chunked yet";
    common::say_writes_bytes(tmp.path(), 16 * 1024);
    let e = m.request("content", None, Some(text.into()), 5000, "path").await.unwrap_err();
    assert_eq!(e.code(), Some("too_large"));
    assert!(e.to_string().contains("max_entry_bytes"));
    assert_eq!(m.store.key_stats("content").unwrap().warm, 0, "a runaway render became warm");
    common::say_writes_bytes(tmp.path(), 1024);
    assert_eq!(m.request("content", None, Some(text.into()), 10_000, "path").await.unwrap()["state"], "warm");
    assert_eq!(calls_of(tmp.path(), text), 2);
}

#[tokio::test]
async fn failures_are_counted_and_an_empty_output_is_never_warm() {
    let (m, tmp) = manager();
    common::say_writes_nothing(tmp.path());
    assert_eq!(m.request("content", None, Some("silence".into()), 5000, "path").await.unwrap_err().code(), Some("synthesis_failed"));
    let s = m.store.key_stats("content").unwrap();
    assert_eq!((s.warm, s.failed), (0, 1));
    assert_eq!(m.store.counters("content").unwrap().fail_total, 1);
}

// ── a killed child is not a failure ───────────────────────────────────────

/// The live incident: the reaper SIGTERMed the daemon's own children and
/// each became `synthesis_failed`. A signal leaves no row, no breaker mark,
/// no failure count; the job is retried once (N >= 2 backend calls); and the
/// text speaks the moment the machine stops killing it.
#[tokio::test]
async fn a_synthesis_killed_by_a_signal_leaves_nothing_behind_and_is_retried_once() {
    let (m, tmp) = manager();
    let text = "he was in the middle of hearing this";
    common::say_dies_on_signal(tmp.path(), true);
    let began = Instant::now();
    let e = m.request("content", None, Some(text.into()), 20_000, "path").await.unwrap_err();
    assert_eq!(e.code(), Some("synthesis_failed"), "a code the client's retry ladder acts on");
    assert!(began.elapsed() < Duration::from_secs(10), "the interrupted job spun");
    assert_eq!(state_of(&m, &hash_of(text)), None, "a killed child left a durable row");
    assert_eq!(m.store.counters("content").unwrap().fail_total, 0);
    assert_eq!(m.module_health()["system"]["consecutive_failures"], 0, "a killed child moved the breaker");
    assert_eq!(calls_of(tmp.path(), text), 1 + tts::manager::INTERRUPTED_REQUEUES as usize);
    assert_eq!(m.queue_position(&hash_of(text)), None, "the claim outlived the job");
    common::say_dies_on_signal(tmp.path(), false);
    assert_eq!(m.request("content", None, Some(text.into()), 10_000, "path").await.unwrap()["state"], "warm");
}

/// A restart strands nothing: a new manager over the same data dir — what
/// the daemon is after cauldron restarts it — sweeps `failed` and
/// `synthesizing` cold, and the poisoned sentence speaks.
#[tokio::test]
async fn a_restart_leaves_every_unfinished_render_cold() {
    let (m, tmp) = manager();
    common::say_fails(tmp.path(), true);
    let poisoned = "cut off when the daemon went down";
    let _ = m.request("content", None, Some(poisoned.into()), 5000, "path").await;
    assert_eq!(state_of(&m, &hash_of(poisoned)).as_deref(), Some("failed"));
    let rebooted = tts::build_manager(tmp.path().to_path_buf(), tts::Config::default()).unwrap();
    let mid = hash_of("was being spoken when the process died");
    rebooted.store.ensure_render(&mid, "system", "Samantha", 10, "aiff").unwrap();
    rebooted.store.set_render_state(&mid, "synthesizing").unwrap();
    let swept = rebooted.boot_sweep().unwrap();
    assert_eq!(swept["stale_failed_cleared"], 1, "{swept}");
    assert_eq!((state_of(&rebooted, &hash_of(poisoned)), state_of(&rebooted, &mid)), (None, None));
    rebooted.start();
    common::say_fails(tmp.path(), false);
    assert_eq!(rebooted.request("content", None, Some(poisoned.into()), 10_000, "path").await.unwrap()["state"], "warm");
}

/// An in-flight synthesis IS a pid the daemon knows it owns — the half of the
/// reaper fix the pure selector test cannot reach — and is released after.
#[tokio::test]
async fn a_synthesis_in_flight_is_a_pid_the_daemon_owns() {
    let (m, tmp) = manager_with(ONE_PERMIT);
    common::say_takes_ms(tmp.path(), 3000);
    let m2 = m.clone();
    let job = tokio::spawn(async move { m2.request("content", None, Some("a long one".into()), 10_000, "path").await });
    assert!(eventually(5000, || !tts::synth::system::live_child_pids().is_empty()).await, "the reaper would have killed it");
    assert!(tts::synth::system::live_child_pids().iter().all(|p| *p != 0 && *p != std::process::id()));
    job.await.unwrap().unwrap();
    assert!(eventually(5000, || tts::synth::system::live_child_pids().is_empty()).await);
}

// ── the circuit breaker: the outage is paid once ──────────────────────────

/// Five consecutive failures open it; the sixth request fails FAST and never
/// reaches the backend.
#[tokio::test]
async fn five_failures_open_the_breaker_and_the_next_request_fails_fast() {
    let (m, tmp) = manager_with(&with_global("breaker_probe_secs = 3600"));
    let mut pushes = m.subscribe();
    common::say_fails(tmp.path(), true);
    for i in 0..5 {
        assert_eq!(m.request("content", None, Some(format!("doomed {i}")), 5000, "path").await.unwrap_err().code(), Some("synthesis_failed"));
    }
    assert_eq!(m.module_health()["system"]["state"], "degraded");
    let calls = say_calls(tmp.path()).len();
    let began = Instant::now();
    assert_eq!(m.request("content", None, Some("the sixth".into()), 30_000, "path").await.unwrap_err().code(), Some("backend_unavailable"));
    assert!(began.elapsed() < Duration::from_secs(2));
    assert_eq!(say_calls(tmp.path()).len(), calls, "a degraded module was still invoked");
    // …and the outage is ANNOUNCED, never silent.
    let announced = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(tts::protocol::Push::ModuleHealth { state, module, .. }) = pushes.recv().await {
                if state == "degraded" {
                    return module;
                }
            }
        }
    })
    .await;
    assert_eq!(announced.ok().as_deref(), Some("system"));
}

#[tokio::test]
async fn one_successful_probe_closes_the_breaker() {
    let (m, tmp) = manager_with(&with_global("breaker_probe_secs = 0"));
    common::say_fails(tmp.path(), true);
    for i in 0..5 {
        let _ = m.request("content", None, Some(format!("doomed {i}")), 5000, "path").await;
    }
    assert_eq!(m.module_health()["system"]["state"], "degraded");
    common::say_fails(tmp.path(), false);
    assert_eq!(m.request("content", None, Some("hello again".into()), 5000, "path").await.unwrap()["state"], "warm");
    assert_eq!((m.module_health()["system"]["state"].as_str(), m.module_health()["system"]["consecutive_failures"].as_u64()), (Some("ok"), Some(0)));
}

#[tokio::test]
async fn interleaved_successes_never_trip_the_breaker() {
    let (m, tmp) = manager_with(&with_global("breaker_probe_secs = 3600"));
    for i in 0..8 {
        common::say_fails(tmp.path(), true);
        let _ = m.request("content", None, Some(format!("bad {i}")), 5000, "path").await;
        common::say_fails(tmp.path(), false);
        let _ = m.request("content", None, Some(format!("good {i}")), 5000, "path").await;
    }
    assert_eq!(m.module_health()["system"]["state"], "ok");
}

/// THE ONE REVERSIBLE FLAG: `breaker_enabled = false` is the per-utterance
/// reading — every request pays its own wait and never `backend_unavailable`.
#[tokio::test]
async fn the_per_utterance_reading_is_one_config_word_away() {
    let (m, tmp) = manager_with(&with_global("breaker_enabled  = false"));
    common::say_fails(tmp.path(), true);
    for i in 0..7 {
        assert_eq!(m.request("content", None, Some(format!("doomed {i}")), 5000, "path").await.unwrap_err().code(), Some("synthesis_failed"));
    }
    assert_eq!(say_calls(tmp.path()).len(), 7);
    assert_eq!(m.module_health()["system"]["state"], "ok");
}
