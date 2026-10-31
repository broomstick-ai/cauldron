//! Warming is a PUT. "Every time we do this put operation… that's like
//! setting the timestamp of the… hash… And it decides on the fly, should we
//! warm this one up now or is it already warm or is it already being warmed
//! up? And then it can decide to discard the text."

mod common;

use common::{calls_of, eventually, manager_with, say_calls, warm};
use tts::queue::Lane;

/// A keep-warm key under a global backstop that is easy to fill; nothing in
/// it is evictable, so once past 500 bytes the cache STAYS full.
const PINNED: &str = r#"
[global]
max_bytes        = 500
max_concurrent   = 4
synth_timeout_ms = 20000
reap_orphans     = false

[keys.pinned]
mode          = "keep_warm"
queueing      = "fifo"
module        = "system"
priority      = 100
min_age_secs  = 0
[keys.pinned.module_config]
voice = "Samantha"
"#;

/// One LRU key with a roomy per-key budget, so only the GLOBAL backstop bites.
fn window(global_max: u64) -> String {
    format!(
        "[global]\nmax_bytes = {global_max}\nmax_concurrent = 4\nsynth_timeout_ms = 20000\nreap_orphans = false\n\n\
         [keys.content]\nmode = \"lru\"\nmax_bytes = \"1MiB\"\nqueueing = \"fifo\"\nmodule = \"system\"\n\
         priority = 50\nmin_age_secs = 0\n[keys.content.module_config]\nvoice = \"Samantha\"\n"
    )
}

fn last_used(m: &tts::TtsManager, key: &str, hash: &str) -> String {
    m.store.entry(key, hash).unwrap().expect("membership").last_used_at
}

#[tokio::test]
async fn a_put_on_a_warm_entry_moves_recency_and_synthesizes_nothing() {
    let (m, tmp) = manager_with(&window(1 << 20));
    let h = warm(&m, "content", "alpha").await;
    let before = last_used(&m, "content", &h);
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    let out = m.register("content", vec!["alpha".into()], Lane::Warm).await.unwrap();
    assert_eq!(out["items"][0]["action"], "warm", "{out}");
    assert_eq!(out["items"][0]["text_used"], false);
    assert_eq!(out["items"][0]["hash"], h);
    assert_eq!((out["warm"].as_u64(), out["enqueued"].as_u64(), out["joined"].as_u64()), (Some(1), Some(0), Some(0)));
    assert_eq!((out["discarded"].as_u64(), out["touched"].as_u64()), (Some(1), Some(1)), "{out}");
    assert!(last_used(&m, "content", &h) > before, "recency did not move");
    assert_eq!(say_calls(tmp.path()).len(), 1);
}

#[tokio::test]
async fn a_put_on_a_rendering_entry_joins_instead_of_spawning() {
    let (m, tmp) = manager_with(&window(1 << 20));
    common::say_takes_ms(tmp.path(), 1200);
    let first = m.register("content", vec!["the slow one".into()], Lane::Warm).await.unwrap();
    assert_eq!((first["items"][0]["action"].as_str(), first["items"][0]["text_used"].as_bool()), (Some("enqueued"), Some(true)));
    assert!(eventually(5_000, || !say_calls(tmp.path()).is_empty()).await, "the render never started");
    for _ in 0..3 {
        let out = m.register("content", vec!["the slow one".into()], Lane::Warm).await.unwrap();
        assert_eq!(out["items"][0]["action"], "joined", "{out}");
        assert_eq!((out["joined"].as_u64(), out["enqueued"].as_u64(), out["discarded"].as_u64()), (Some(1), Some(0), Some(1)));
    }
    assert!(eventually(20_000, || m.store.key_stats("content").unwrap().warm == 1).await);
    assert_eq!(calls_of(tmp.path(), "the slow one"), 1, "joining spawned a second synthesis");
}

/// A join creates no work, so a FULL cache still answers it — while a new
/// utterance is refused, and the refusal names its hash.
#[tokio::test]
async fn a_join_is_never_refused_by_a_full_cache_but_new_work_is() {
    let (m, tmp) = manager_with(PINNED);
    common::say_writes_bytes(tmp.path(), 600);
    common::say_takes_ms(tmp.path(), 4000);
    let started = m.register("pinned", vec!["the long one".into()], Lane::Warm).await.unwrap();
    assert_eq!(started["items"][0]["action"], "enqueued");
    assert!(eventually(5_000, || !say_calls(tmp.path()).is_empty()).await);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    common::say_takes_ms(tmp.path(), 0);
    m.register("pinned", vec!["the filler".into()], Lane::Warm).await.unwrap();
    assert!(eventually(20_000, || m.store.total_warm_bytes().unwrap() >= 500).await, "the cache never filled");

    let cold = m.register("pinned", vec!["something new".into()], Lane::Warm).await.unwrap();
    assert_eq!((cold["refused"].as_u64(), cold["items"][0]["code"].as_str()), (Some(1), Some("disk_full")), "{cold}");
    assert!(cold["items"][0]["hash"].is_string());
    let join = m.register("pinned", vec!["the long one".into()], Lane::Warm).await.unwrap();
    assert_eq!((join["refused"].as_u64(), join["items"][0]["action"].as_str()), (Some(0), Some("joined")), "{join}");
    assert_eq!(calls_of(tmp.path(), "the long one"), 1);
}

/// The touch comes first and unconditionally: even a refused PUT has moved
/// recency, because "I am using this" is true whether or not the bytes can
/// be made right now.
#[tokio::test]
async fn even_a_refused_put_has_already_moved_recency() {
    let (m, tmp) = manager_with(PINNED);
    common::say_fails(tmp.path(), true);
    m.register("pinned", vec!["the broken one".into()], Lane::Warm).await.unwrap();
    assert!(eventually(20_000, || m.store.key_stats("pinned").unwrap().failed == 1).await);
    let h = common::hash_of("the broken one");
    let before = last_used(&m, "pinned", &h);
    common::say_fails(tmp.path(), false);
    common::say_writes_bytes(tmp.path(), 600);
    m.register("pinned", vec!["the filler".into()], Lane::Warm).await.unwrap();
    assert!(eventually(20_000, || m.store.total_warm_bytes().unwrap() >= 500).await);
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    let out = m.register("pinned", vec!["the broken one".into()], Lane::Warm).await.unwrap();
    assert_eq!((out["refused"].as_u64(), out["items"][0]["code"].as_str(), out["touched"].as_u64()), (Some(1), Some("disk_full"), Some(1)), "{out}");
    assert!(last_used(&m, "pinned", &h) > before, "a refused PUT left recency where it was");
}

/// The window he is navigating is touched as a WHOLE before any item is
/// decided, so warming its cold member cannot evict its warm member.
#[tokio::test]
async fn warming_a_window_takes_its_members_out_of_the_firing_line() {
    let (m, tmp) = manager_with(&window(1 << 20));
    common::say_writes_bytes(tmp.path(), 300);
    let alpha = warm(&m, "content", "alpha").await;
    let beta = warm(&m, "content", "beta").await;
    warm(&m, "content", "gamma").await;
    assert_eq!(m.store.total_warm_bytes().unwrap(), 900);
    std::fs::write(tmp.path().join("tts.toml"), window(700)).unwrap();
    m.reload().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    let out = m.register("content", vec!["delta".into(), "alpha".into()], Lane::Warm).await.unwrap();
    assert_eq!((out["items"][0]["action"].as_str(), out["items"][1]["action"].as_str()), (Some("enqueued"), Some("warm")), "{out}");
    assert!(m.store.has_entry("content", &alpha).unwrap(), "the window evicted its own member");
    assert!(!m.store.has_entry("content", &beta).unwrap(), "the true LRU victim survived");
    assert!(eventually(20_000, || calls_of(tmp.path(), "delta") == 1).await);
    assert_eq!(calls_of(tmp.path(), "alpha"), 1);
}

#[tokio::test]
async fn warming_one_window_twenty_times_enqueues_it_once() {
    let (m, tmp) = manager_with(&window(1 << 20));
    let win: Vec<String> = (0..5).map(|i| format!("message {i}")).collect();
    let mut enqueued = 0;
    for _ in 0..20 {
        let out = m.register("content", win.iter().cloned().map(Into::into).collect(), Lane::Warm).await.unwrap();
        enqueued += out["enqueued"].as_u64().unwrap();
        assert_eq!(out["refused"], 0, "{out}");
    }
    assert_eq!(enqueued, 5);
    assert!(eventually(20_000, || m.store.key_stats("content").unwrap().warm == 5).await);
    let out = m.register("content", win.into_iter().map(Into::into).collect(), Lane::Warm).await.unwrap();
    for (k, v) in [("warm", 5), ("queued", 0), ("enqueued", 0), ("joined", 0), ("touched", 5), ("discarded", 5)] {
        assert_eq!(out[k], v, "{k}: {out}");
    }
    assert_eq!(say_calls(tmp.path()).len(), 5);
}

#[tokio::test]
async fn a_duplicate_inside_one_batch_is_enqueued_once_and_joined_once() {
    let (m, tmp) = manager_with(&window(1 << 20));
    common::say_takes_ms(tmp.path(), 400);
    let out = m.register("content", vec!["twice over".into(), "twice over".into()], Lane::Warm).await.unwrap();
    assert_eq!((out["enqueued"].as_u64(), out["joined"].as_u64()), (Some(1), Some(1)), "{out}");
    assert_eq!(out["items"][0]["hash"], out["items"][1]["hash"]);
    assert!(eventually(20_000, || m.store.key_stats("content").unwrap().warm == 1).await);
    assert_eq!(calls_of(tmp.path(), "twice over"), 1);
}

/// One permit, one job, one at a time, in FIFO order.
#[tokio::test]
async fn the_scheduler_pulls_one_job_per_permit_in_fifo_order() {
    let (m, tmp) = manager_with(&window(1 << 20).replace("max_concurrent = 4", "max_concurrent = 1"));
    common::say_takes_ms(tmp.path(), 120);
    let batch: Vec<String> = (0..6).map(|i| format!("item {i}")).collect();
    let out = m.register("content", batch.iter().cloned().map(Into::into).collect(), Lane::Warm).await.unwrap();
    assert_eq!(out["enqueued"], 6);
    assert!(eventually(30_000, || say_calls(tmp.path()).len() >= 6).await);
    assert_eq!(say_calls(tmp.path()), batch, "FIFO was not honoured");
    assert_eq!(common::peak_concurrency(tmp.path()), 1);
}

/// An empty text is refused per item, not for the batch, and an oversize
/// batch is rejected whole rather than truncated.
#[tokio::test]
async fn empty_items_are_refused_singly_and_oversize_batches_whole() {
    let (m, tmp) = manager_with(&window(1 << 20).replace("reap_orphans = false", "reap_orphans = false\nmax_batch_items = 3"));
    let out = m.register("content", vec!["   ".into(), "fine".into()], Lane::Warm).await.unwrap();
    assert_eq!((out["refused"].as_u64(), out["items"][0]["code"].as_str()), (Some(1), Some("bad_params")), "{out}");
    let e = m.register("content", (0..10).map(|i| format!("item {i}").into()).collect(), Lane::Warm).await.unwrap_err();
    assert_eq!(e.code(), Some("bad_params"));
    assert!(e.to_string().contains("max_batch_items"));
    assert!(eventually(5_000, || say_calls(tmp.path()).len() == 1).await);
    assert!(say_calls(tmp.path()).iter().all(|c| c == "fine"), "a rejected batch was half accepted");
}
