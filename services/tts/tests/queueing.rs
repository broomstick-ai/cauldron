//! The queue's order, end to end, through the real scheduler with ONE
//! permit, so every decision is visible in the order the backend was called.
//! (The orderings themselves are unit-tested in `src/queue.rs`.)

mod common;

use common::{eventually, manager_with, say_calls};
use tts::manager::RegisterItem;
use tts::priority::look_ahead;
use tts::queue::Lane;
use tts::TtsManager;

/// One permit; `interface` and `content` at `(i, c)` default priorities.
fn serial(i: i64, c: i64) -> String {
    format!(
        "[global]\nmax_bytes = \"1MiB\"\nmax_concurrent = 1\nsynth_timeout_ms = 20000\nreap_orphans = false\n\n\
         [keys.interface]\nmode = \"keep_warm\"\nqueueing = \"fifo\"\nmodule = \"system\"\npriority = {i}\n\
         min_age_secs = 0\nrefuse_bytes = \"1MiB\"\n[keys.interface.module_config]\nvoice = \"Samantha\"\n\n\
         [keys.content]\nmode = \"lru\"\nmax_bytes = \"1MiB\"\nqueueing = \"fifo\"\nmodule = \"system\"\n\
         priority = {c}\nmin_age_secs = 0\n[keys.content.module_config]\nvoice = \"Samantha\"\n"
    )
}

/// The old numbers (100/50), kept to show equal-priority behaviour is what
/// it was; and the published scale (1/0) for anything using the ladder.
fn old_scale() -> String {
    serial(100, 50)
}
fn on_scale() -> String {
    serial(1, 0)
}

/// Occupy the single permit, so everything registered next is genuinely
/// queued before anything of it is popped.
async fn block(m: &TtsManager, tmp: &std::path::Path) {
    common::say_takes_ms(tmp, 250);
    m.register("content", vec!["blocker".into()], Lane::Warm).await.unwrap();
    assert!(eventually(5_000, || !say_calls(tmp).is_empty()).await);
}

async fn drained(tmp: &std::path::Path, n: usize) -> Vec<String> {
    assert!(eventually(60_000, || say_calls(tmp).len() >= n).await, "the queue never drained");
    say_calls(tmp)
}

#[tokio::test]
async fn the_interactive_lane_jumps_a_warm_backlog_that_stays_in_order() {
    let (m, tmp) = manager_with(&old_scale());
    block(&m, tmp.path()).await;
    m.register("content", vec!["warm one".into(), "warm two".into()], Lane::Warm).await.unwrap();
    m.register("content", vec!["urgent".into()], Lane::Interactive).await.unwrap();
    assert_eq!(drained(tmp.path(), 4).await, ["blocker", "urgent", "warm one", "warm two"]);
}

#[tokio::test]
async fn a_content_backlog_does_not_starve_the_menu() {
    let (m, tmp) = manager_with(&old_scale());
    block(&m, tmp.path()).await;
    m.register("content", vec!["message one".into(), "message two".into()], Lane::Warm).await.unwrap();
    m.register("interface", vec!["Settings".into()], Lane::Warm).await.unwrap();
    assert_eq!(drained(tmp.path(), 4).await, ["blocker", "Settings", "message one", "message two"]);
}

#[tokio::test]
async fn a_lifo_key_serves_newest_first_through_the_scheduler() {
    let cfg = format!(
        "{}\n[keys.newest-first]\nmode = \"lru\"\nmax_bytes = \"1MiB\"\nqueueing = \"lifo\"\nmodule = \"system\"\n\
         priority = 50\nmin_age_secs = 0\n[keys.newest-first.module_config]\nvoice = \"Samantha\"\n",
        old_scale()
    );
    let (m, tmp) = manager_with(&cfg);
    block(&m, tmp.path()).await;
    m.register("newest-first", vec!["a".into(), "b".into(), "c".into(), "d".into()], Lane::Warm).await.unwrap();
    assert_eq!(drained(tmp.path(), 5).await, ["blocker", "d", "c", "b", "a"]);
}

/// "the next message has a priority of six, and then two-ahead has a
/// priority of five…" — registered LAST and out of order, served nearest
/// first, ahead of a backlog it does not reorder.
#[tokio::test]
async fn the_look_ahead_ladder_comes_out_nearest_first_ahead_of_the_backlog() {
    let (m, tmp) = manager_with(&on_scale());
    block(&m, tmp.path()).await;
    m.register("content", (0..4).map(|i| RegisterItem::new(format!("bulk {i}"))).collect(), Lane::Warm).await.unwrap();
    let window = [3u32, 5, 1, 4, 2].into_iter().map(|d| RegisterItem::with_priority(format!("ahead {d}"), look_ahead(d))).collect();
    m.register("content", window, Lane::Warm).await.unwrap();
    let calls = drained(tmp.path(), 10).await;
    assert_eq!(&calls[1..6], ["ahead 1", "ahead 2", "ahead 3", "ahead 4", "ahead 5"], "{calls:?}");
    assert_eq!(&calls[6..], ["bulk 0", "bulk 1", "bulk 2", "bulk 3"], "the ladder reordered the backlog");
}

/// THE MAX RULE end to end: warmed ahead cheaply, then asked for at the top
/// of the ladder — one item, raised, never two, never demoted after.
#[tokio::test]
async fn a_duplicate_raises_the_queued_item_once_and_never_demotes_it() {
    let (m, tmp) = manager_with(&on_scale());
    block(&m, tmp.path()).await;
    let first = m.register("content", ["queued one", "queued two", "warmed ahead"].map(RegisterItem::new).to_vec(), Lane::Warm).await.unwrap();
    assert_eq!(first["enqueued"], 3);
    let raised = m.register("content", vec![RegisterItem::with_priority("warmed ahead", look_ahead(1))], Lane::Warm).await.unwrap();
    assert_eq!((raised["enqueued"].as_u64(), raised["joined"].as_u64(), raised["items"][0]["priority"].as_i64()), (Some(0), Some(1), Some(look_ahead(1))), "{raised}");
    let swept = m.register("content", vec!["warmed ahead".into()], Lane::Warm).await.unwrap();
    assert_eq!(swept["items"][0]["priority"], look_ahead(1), "a lower duplicate demoted it");
    assert_eq!(drained(tmp.path(), 4).await, ["blocker", "warmed ahead", "queued one", "queued two"]);
}

/// "You should just have a default priority associated with each cache." The
/// registration answers with the number the caller did not send.
#[tokio::test]
async fn a_caller_that_names_no_priority_gets_its_cache_default() {
    let (m, tmp) = manager_with(&on_scale());
    block(&m, tmp.path()).await;
    let c = m.register("content", vec!["a message body".into()], Lane::Warm).await.unwrap();
    let i = m.register("interface", vec!["Settings".into()], Lane::Warm).await.unwrap();
    assert_eq!((c["items"][0]["priority"].as_i64(), i["items"][0]["priority"].as_i64()), (Some(0), Some(1)));
    assert_eq!(drained(tmp.path(), 3).await, ["blocker", "Settings", "a message body"]);
}

/// A miss carries the queue depth and the gate it was stuck behind; a hit's
/// payload carries none of it.
#[tokio::test]
async fn a_miss_carries_the_backlog_and_the_gate_and_a_hit_does_not() {
    let (m, tmp) = manager_with(&old_scale());
    common::say_takes_ms(tmp.path(), 400);
    m.register("content", vec!["backlog one".into(), "backlog two".into(), "backlog three".into()], Lane::Warm).await.unwrap();
    assert!(eventually(5_000, || !say_calls(tmp.path()).is_empty()).await);
    let v = m.request("content", None, Some("the menu row".into()), 30_000, "path").await.unwrap();
    assert_eq!((v["from_cache"].as_bool(), v["gate_max"].as_u64()), (Some(false), Some(1)), "{v}");
    assert!(v["gate_busy"].as_u64().unwrap() >= 1 && v["queued_behind"].as_u64().unwrap() >= 1, "{v}");
    assert_eq!(v["queued_behind_key"], v["queued_behind"]);
    let hit = m.request("content", None, Some("the menu row".into()), 30_000, "path").await.unwrap();
    assert!(hit["from_cache"] == true && hit.get("queued_behind").is_none(), "{hit}");
}
