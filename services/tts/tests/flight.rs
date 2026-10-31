//! Single-flight, the gate, the reserve, and letting go.
//!
//! The incidents these pin: one utterance spawning `say` three times at once,
//! 59 processes against one shared speech service; a backlog of 3,173 warm
//! items with every keypress waiting 1.4–2.5 s for a permit; and a row reading
//! "thread compacted" that sounded an error tone forever because an evicted
//! job left its claim behind. Each is a property of the service now.

mod common;

use std::time::{Duration, Instant};

use common::{calls_of, eventually, hash_of, manager, manager_with, say_calls, state_of};
use tts::manager::RegisterItem;
use tts::queue::Lane;

/// A gate of four against a slow backend, on the published scale.
const GATE_OF_FOUR: &str = r#"
[global]
max_bytes        = "64MiB"
max_concurrent   = 4
synth_timeout_ms = 20000
reap_orphans     = false

[keys.interface]
mode          = "keep_warm"
queueing      = "fifo"
module        = "system"
priority      = 1
min_age_secs  = 0
refuse_bytes  = "64MiB"
[keys.interface.module_config]
voice = "Samantha"

[keys.content]
mode          = "lru"
max_bytes     = "64MiB"
queueing      = "fifo"
module        = "system"
priority      = 0
min_age_secs  = 0
[keys.content.module_config]
voice = "Samantha"
"#;

fn texts(prefix: &str, n: usize) -> Vec<RegisterItem> {
    (0..n).map(|i| RegisterItem::new(format!("{prefix} {i}"))).collect()
}

/// Twenty concurrent requests for one utterance: ONE backend call, one path.
#[tokio::test]
async fn twenty_concurrent_requests_for_one_utterance_synthesize_once() {
    let (m, tmp) = manager();
    common::say_takes_ms(tmp.path(), 400);
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let m = m.clone();
            tokio::spawn(async move { m.request("content", None, Some("the same sentence".into()), 10_000, "path").await })
        })
        .collect();
    let mut paths = Vec::new();
    for t in tasks {
        paths.push(t.await.unwrap().expect("request")["path"].as_str().unwrap().to_string());
    }
    assert_eq!(say_calls(tmp.path()).len(), 1, "the same utterance was synthesized more than once");
    assert!(paths.windows(2).all(|w| w[0] == w[1]));
}

/// "So let's do four." Four, the interactive lane included.
#[tokio::test]
async fn the_gate_is_four_wide_and_nothing_synthesizes_without_a_permit() {
    let (m, tmp) = manager();
    common::say_takes_ms(tmp.path(), 200);
    m.register("content", texts("utterance", 40), Lane::Warm).await.unwrap();
    m.register("content", texts("urgent", 8), Lane::Interactive).await.unwrap();
    assert!(eventually(30_000, || say_calls(tmp.path()).len() >= 48).await, "the queue never drained");
    let peak = common::peak_concurrency(tmp.path());
    assert!((2..=4).contains(&peak), "peak concurrency {peak}: the gate never ran in parallel, or was exceeded");
}

/// The re-probe after the permit: a warm entry is never synthesized again.
#[tokio::test]
async fn registering_an_already_warm_utterance_costs_no_synthesis() {
    let (m, tmp) = manager();
    common::warm(&m, "content", "warm me").await;
    for _ in 0..5 {
        let out = m.register("content", vec!["warm me".into()], Lane::Warm).await.unwrap();
        assert_eq!((out["warm"].as_u64(), out["queued"].as_u64()), (Some(1), Some(0)), "{out}");
    }
    assert_eq!(say_calls(tmp.path()).len(), 1);
}

/// Two keys on one fingerprint share the BYTES; forgetting from one key does
/// not take them from the other.
#[tokio::test]
async fn two_keys_on_one_fingerprint_share_one_render() {
    let (m, tmp) = manager();
    let a = m.request("content", None, Some("shared beat".into()), 5000, "path").await.unwrap();
    let b = m.request("interface", None, Some("shared beat".into()), 5000, "path").await.unwrap();
    assert_eq!((&a["hash"], &a["path"]), (&b["hash"], &b["path"]));
    assert_eq!(b["from_cache"], true);
    assert_eq!(say_calls(tmp.path()).len(), 1);
    let out = m.forget("content", &[a["hash"].as_str().unwrap().into()]).unwrap();
    assert_eq!(out["freed_bytes"], 0);
    assert!(std::path::Path::new(a["path"].as_str().unwrap()).exists());
}

/// THE BAR. A deep warm backlog holds every permit at the top of the ladder;
/// he presses a key; his synthesis STARTS at once, because one permit is
/// always held back for a blocked caller. Measured to the start, not the end.
#[tokio::test]
async fn the_row_he_is_standing_on_does_not_wait_for_the_warm_backlog() {
    let (m, tmp) = manager_with(GATE_OF_FOUR);
    common::say_takes_ms(tmp.path(), 3_000);
    let backlog = (0..200).map(|i| RegisterItem::with_priority(format!("warm ahead {i}"), tts::priority::look_ahead(1))).collect();
    m.register("content", backlog, Lane::Warm).await.unwrap();
    assert!(eventually(5_000, || !say_calls(tmp.path()).is_empty()).await, "the backlog never started");
    let m2 = m.clone();
    let pressed = tokio::spawn(async move { m2.request("interface", None, Some("options".into()), 20_000, "path").await });
    let began = Instant::now();
    assert!(eventually(10_000, || say_calls(tmp.path()).iter().any(|t| t == "options")).await);
    let waited = began.elapsed();
    assert!(waited < Duration::from_millis(800), "he waited {waited:?} for a permit");
    assert!(pressed.await.unwrap().expect("his row")["path"].as_str().is_some_and(|p| !p.is_empty()));
}

/// The queue is bounded, asserted against a literal rather than the constant
/// under test (which would be a tautology).
#[tokio::test]
async fn a_flood_of_warm_ahead_cannot_grow_the_queue_without_limit() {
    let (m, tmp) = manager_with(GATE_OF_FOUR);
    common::say_takes_ms(tmp.path(), 5_000);
    for batch in 0..4 {
        m.register("content", texts(&format!("abandoned {batch}"), 1_000), Lane::Warm).await.unwrap();
    }
    let depth = m.queue_depth();
    assert!(depth < 1_000 && depth as usize <= tts::queue::MAX_QUEUED, "queue reached {depth} of 4,000 pushed");
}

/// THE BEEP. Overfill the queue so the newest warm item is evicted, then ask
/// for it again: it must be ENQUEUED afresh, not JOINED to a claim whose job
/// no longer exists — and no row may still claim a synthesis is coming.
#[tokio::test]
async fn an_evicted_job_does_not_strand_its_claim() {
    let (m, tmp) = manager_with(GATE_OF_FOUR);
    common::say_takes_ms(tmp.path(), 30_000);
    let all = texts("stranded row", 700);
    let victim = all.last().unwrap().text.clone();
    m.register("content", all, Lane::Warm).await.unwrap();
    assert_eq!(m.queue_depth() as usize, tts::queue::MAX_QUEUED, "precondition: the bound is enforced");
    assert_eq!(m.queue_position(&hash_of(&victim)), None, "precondition: the victim was evicted");
    assert_eq!(state_of(&m, &hash_of(&victim)), None, "an evicted job left a row claiming a synthesis is coming");
    let again = m.register("content", vec![victim.into()], Lane::Warm).await.unwrap();
    assert_eq!((again["items"][0]["action"].as_str(), again["joined"].as_u64()), (Some("enqueued"), Some(0)), "{again}");
}

#[tokio::test]
async fn a_queued_row_does_not_survive_a_boot() {
    let (m, tmp) = manager_with(GATE_OF_FOUR);
    common::say_takes_ms(tmp.path(), 30_000);
    let batch = texts("mid flight", 8);
    let names: Vec<String> = batch.iter().map(|i| i.text.clone()).collect();
    m.register("content", batch, Lane::Warm).await.unwrap();
    assert!(state_of(&m, &hash_of(&names[7])).is_some());
    let swept = m.boot_sweep().unwrap();
    assert!(swept["stranded_queued_cleared"].as_u64().unwrap() > 0, "{swept}");
    for t in &names {
        assert_ne!(state_of(&m, &hash_of(t)).as_deref(), Some("queued"), "{t} still claims a synthesis is coming");
    }
}

/// Cancelling a queued item takes it out before it costs anything.
#[tokio::test]
async fn cancelling_a_queued_utterance_takes_it_out_before_it_costs_anything() {
    let (m, tmp) = manager_with(GATE_OF_FOUR);
    common::say_takes_ms(tmp.path(), 30_000);
    m.register("content", texts("queued line", 12), Lane::Warm).await.unwrap();
    let h = hash_of("queued line 11");
    assert!(m.queue_position(&h).is_some(), "precondition: still waiting");
    let out = m.cancel("content", &[h.clone()]).unwrap();
    assert_eq!((out["dequeued"].as_u64(), out["aborted"].as_u64()), (Some(1), Some(0)), "{out}");
    assert_eq!((m.queue_position(&h), state_of(&m, &h)), (None, None));
    assert_eq!(calls_of(tmp.path(), "queued line 11"), 0);
}

/// THE CLOG. One permit, held by a huge row he scrolled past; he cancels it;
/// the row he moved TO speaks at once, and the abandoned request is told
/// `cancelled` — not a failure, so asking again later just works.
#[tokio::test]
async fn cancelling_the_row_he_left_frees_the_permit_and_records_nothing() {
    let one = GATE_OF_FOUR.replace("max_concurrent   = 4", "max_concurrent   = 1");
    let (m, tmp) = manager_with(&one);
    common::say_takes_ms(tmp.path(), 20_000);
    let huge = "a very long user message he scrolled past";
    let h = hash_of(huge);
    let m2 = m.clone();
    let in_flight = tokio::spawn(async move { m2.request("content", None, Some(huge.into()), 30_000, "path").await });
    assert!(eventually(5_000, || state_of(&m, &h).as_deref() == Some("synthesizing")).await, "precondition");
    common::say_takes_ms(tmp.path(), 0);
    assert_eq!(m.cancel("content", &[h.clone()]).unwrap()["aborted"], 1);
    let began = Instant::now();
    let next = m.request("content", None, Some("the next menu item".into()), 10_000, "path").await.expect("the row he moved to");
    assert!(next["path"].is_string() && began.elapsed() < Duration::from_secs(5), "waited {:?}", began.elapsed());
    assert_eq!(in_flight.await.unwrap().unwrap_err().code(), Some("cancelled"));
    assert_eq!(state_of(&m, &h), None, "a cancelled synthesis left a row");
    assert_eq!(m.store.counters("content").unwrap().fail_total, 0, "a cancel was counted as a failure");
    let again = m.request("content", None, Some(huge.into()), 10_000, "path").await.expect("cold again, not broken");
    assert_eq!(again["from_cache"], false);
}

/// Cancel is not forget: warm audio is kept, repeats are fine, an unknown
/// hash is not an error, and a malformed one or an unknown key is.
#[tokio::test]
async fn cancelling_warm_or_unknown_audio_keeps_it() {
    let (m, _tmp) = manager_with(GATE_OF_FOUR);
    let h = common::warm(&m, "content", "already warm").await;
    let out = m.cancel("content", &[h.clone()]).unwrap();
    assert_eq!((out["dequeued"].as_u64(), out["aborted"].as_u64(), out["unknown"].as_u64()), (Some(0), Some(0), Some(1)));
    let after = m.request("content", Some(h.clone()), None, 1_000, "path").await.unwrap();
    assert_eq!(after["from_cache"], true);
    m.cancel("content", &[h, format!("{:064x}", 1)]).unwrap();
    assert!(m.cancel("content", &["nope".into()]).is_err());
    assert!(m.cancel("no-such-key", &[]).is_err());
}

/// What survived of the substrate daemon's self-update protocol: on its stop
/// signal the daemon starts nothing new and lets a running synthesis LAND
/// before it exits — the quiet moment, without the execv.
#[tokio::test]
async fn a_stop_lets_the_running_synthesis_land_and_starts_nothing_new() {
    let one = GATE_OF_FOUR.replace("max_concurrent   = 4", "max_concurrent   = 1");
    let (m, tmp) = manager_with(&one);
    common::say_takes_ms(tmp.path(), 600);
    m.register("content", vec!["being spoken".into(), "still queued".into()], Lane::Warm).await.unwrap();
    assert!(eventually(5_000, || say_calls(tmp.path()).len() == 1).await, "precondition: one in flight");
    assert_eq!(m.drain(Duration::from_secs(5)).await, 0, "the stop did not wait for the synthesis");
    assert_eq!(state_of(&m, &hash_of("being spoken")).as_deref(), Some("warm"), "the running sentence was cut");
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(say_calls(tmp.path()), ["being spoken"], "a draining daemon started new work");
}
