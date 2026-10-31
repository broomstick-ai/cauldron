//! Eviction, keep-warm, recency, and the sweep that refuses to be the 33 GB
//! incident in reverse.

mod common;

use common::manager_with;

/// **THE RACE IS BOUNDED, NOT REMOVED** (R5.S1b, on the R5.S1 inspection's
/// F-5; corrected by R5.S1c, whose inspector found the first wording, "no
/// wall-clock race", overclaimed). Nothing here tests a timeout, yet every
/// render runs under two: the synthesis timeout below and the request wait.
/// Each is a wall-clock bound on a REAL subprocess, so a slow-enough render
/// always loses; no event can replace it, because the event being awaited is
/// the very exit the bound limits. The only race-free form is no bound at all,
/// which turns a slow render into a hung suite, and the production config has
/// no "no timeout" value to set, so these tests keep a bound.
///
/// What was done instead is to make the margin large, and MEASURE it (36
/// concurrent copies of this binary, every render's `synth_ms` logged, R5.S1c
/// report):
/// - the fixture used to exec about eight helpers per render, so under load a
///   render took up to 81.5 s (mean 31.6 s), against 5 s here before R5.S1b
///   and 120 s after it: a 1.5x margin.
/// - `tests/fixtures/fake-say` is now builtin-only bar one `rm`: the slowest
///   render at 36x is 4.7 s (mean 0.53 s), so 120 s is a ~25x margin.
///
/// The one remaining sleep, [`next_second`], is a LOWER bound (it crosses a
/// second boundary), so a slow machine can only make it more true. Timeouts
/// themselves are tested in `failures.rs`, which keeps its own bounds.
const NO_RACE_MS: u64 = 120_000;

async fn warm(m: &TtsManager, key: &str, text: &str) -> String {
    common::warm_within(m, key, text, NO_RACE_MS).await
}
use tts::queue::Lane;
use tts::TtsManager;

/// `interface` keep-warm with an 8 KiB ceiling; `content` LRU at 6 KiB, which
/// is three 2 KiB fixture renders. Plus two count-bounded keys at the
/// operator's own numbers.
const TINY: &str = r#"
[global]
max_bytes        = "1MiB"
max_concurrent   = 4
synth_timeout_ms = 120000
reap_orphans     = false

[keys.interface]
mode          = "keep_warm"
queueing      = "fifo"
module        = "system"
priority      = 100
min_age_secs  = 0
refuse_bytes  = "8KiB"
[keys.interface.module_config]
voice = "Samantha"

[keys.content]
mode          = "lru"
max_bytes     = "6KiB"
queueing      = "fifo"
module        = "system"
priority      = 50
min_age_secs  = 0
[keys.content.module_config]
voice = "Samantha"

[keys.pinned-count]
mode          = "lru_count"
max_entries   = 1
queueing      = "fifo"
module        = "system"
priority      = 100
min_age_secs  = 0
[keys.pinned-count.module_config]
voice = "Samantha"

[keys.active-count]
mode          = "lru_count"
max_entries   = 10
queueing      = "fifo"
module        = "system"
priority      = 100
min_age_secs  = 0
[keys.active-count.module_config]
voice = "Samantha"
"#;

fn entries(m: &TtsManager, key: &str) -> u64 {
    m.store.key_stats(key).unwrap().entries
}

fn holds(m: &TtsManager, key: &str, hash: &str) -> bool {
    m.store.has_entry(key, hash).unwrap()
}

/// `last_used_at` has second granularity; cross a second boundary.
async fn next_second() {
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
}

#[tokio::test]
async fn the_lru_evicts_the_least_recently_used_first() {
    let (m, _t) = manager_with(TINY);
    let hs = [warm(&m, "content", "alpha").await, warm(&m, "content", "bravo").await, warm(&m, "content", "charlie").await];
    let d = warm(&m, "content", "delta").await;
    assert_eq!(entries(&m, "content"), 3, "the budget was not enforced");
    assert!(!holds(&m, "content", &hs[0]), "the oldest entry survived");
    assert!([&hs[1], &hs[2], &d].iter().all(|h| holds(&m, "content", h)));
}

/// A touch — explicit, or a request served from cache — moves recency, so
/// the touched entry stops being the victim.
#[tokio::test]
async fn a_touch_or_a_request_takes_an_entry_out_of_the_firing_line() {
    for via_request in [false, true] {
        let (m, _t) = manager_with(TINY);
        let a = warm(&m, "content", "alpha").await;
        let b = warm(&m, "content", "bravo").await;
        warm(&m, "content", "charlie").await;
        next_second().await;
        if via_request {
            let r = m.request("content", Some(a.clone()), None, NO_RACE_MS, "path").await.unwrap();
            assert_eq!(r["from_cache"], true);
            assert!(m.store.entry("content", &a).unwrap().unwrap().use_count >= 1, "a request is a use");
        } else {
            m.touch("content", &[a.clone()]).unwrap();
        }
        warm(&m, "content", "delta").await;
        assert!(holds(&m, "content", &a), "the touched entry was evicted (via_request={via_request})");
        assert!(!holds(&m, "content", &b), "the true LRU victim survived (via_request={via_request})");
    }
}

/// "keep everything warm": past its ceiling a keep-warm key REFUSES, and
/// nothing in it is evicted to make room.
#[tokio::test]
async fn a_keep_warm_key_refuses_rather_than_evicting() {
    let (m, _t) = manager_with(TINY);
    let mut kept = Vec::new();
    for i in 0..4 {
        kept.push(warm(&m, "interface", &format!("menu item {i}")).await);
    }
    let refused = m.register("interface", vec!["one too many".into()], Lane::Warm).await.unwrap();
    assert_eq!((refused["refused"].as_u64(), refused["items"][0]["code"].as_str()), (Some(1), Some("key_full")));
    assert_eq!(entries(&m, "interface"), 4);
    assert!(kept.iter().all(|h| holds(&m, "interface", h)), "a keep-warm entry was evicted");
}

#[tokio::test]
async fn nothing_younger_than_min_age_is_a_victim() {
    let young = TINY.replacen("min_age_secs  = 0\n[keys.content.module_config]", "min_age_secs  = 600\n[keys.content.module_config]", 1);
    assert_ne!(young, TINY, "the fixture edit did not apply");
    let (m, _t) = manager_with(&young);
    for i in 0..4 {
        warm(&m, "content", &format!("fresh {i}")).await;
    }
    assert_eq!(entries(&m, "content"), 4);
    assert!(m.store.key_stats("content").unwrap().bytes > 6 * 1024);
}

/// The global backstop takes from the bounded keys — byte and count alike —
/// and never from a keep-warm key.
#[tokio::test]
async fn the_global_backstop_never_takes_a_keep_warm_entry() {
    let cfg = TINY.replace("max_bytes        = \"1MiB\"", "max_bytes        = \"9KiB\"").replace("max_bytes     = \"6KiB\"", "max_bytes     = \"64KiB\"");
    let (m, _t) = manager_with(&cfg);
    let pinned = [warm(&m, "interface", "permanent 0").await, warm(&m, "interface", "permanent 1").await];
    for i in 0..3 {
        warm(&m, "content", &format!("disposable {i}")).await;
        warm(&m, "active-count", &format!("restart aui. {i} active")).await;
    }
    m.enforce_global().unwrap();
    assert!(pinned.iter().all(|h| holds(&m, "interface", h)), "the backstop evicted a keep-warm entry");
    assert!(m.store.total_warm_bytes().unwrap() <= 9 * 1024, "the backstop held nothing");
}

/// Rows first, bytes second: between the two, the failure is an orphaned
/// file, never a row naming a deleted one.
#[tokio::test]
async fn eviction_commits_rows_before_it_unlinks_bytes() {
    let (m, _t) = manager_with(TINY);
    let h = warm(&m, "content", "doomed").await;
    let rel = m.store.get_render(&h).unwrap().unwrap().rel_path.unwrap();
    assert!(m.store.drop_entry("content", &h).unwrap() > 0);
    assert!(m.store.get_render(&h).unwrap().is_none() && m.abs_path(&rel).exists());
    assert_eq!(m.store.take_pending_unlink().unwrap().len(), 1);
    m.drain_unlinks();
    assert!(!m.abs_path(&rel).exists() && m.store.take_pending_unlink().unwrap().is_empty());
}

fn orphan(m: &TtsManager, rel: &str) -> std::path::PathBuf {
    let p = m.abs_path(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, b"orphan").unwrap();
    p
}

/// "It might've even deleted all of my WAV files." The boot sweep refuses
/// to delete most of the directory…
#[tokio::test]
async fn the_boot_sweep_refuses_to_delete_most_of_the_directory() {
    let (m, _t) = manager_with(TINY);
    warm(&m, "content", "known one").await;
    warm(&m, "content", "known two").await;
    let orphans: Vec<_> = (0..6).map(|i| orphan(&m, &format!("zz/zz/{}{i}.aiff", "f".repeat(63)))).collect();
    let report = m.boot_sweep().unwrap();
    assert_eq!((report["sweep_refused"].as_bool(), report["orphan_files"].as_u64(), report["orphan_files_unlinked"].as_u64()), (Some(true), Some(6), Some(0)), "{report}");
    assert!(orphans.iter().all(|p| p.exists()), "a refused sweep deleted anyway");
}

/// …but under the threshold it tidies, repairs a row whose file vanished,
/// and sweeps stale `.part` files without counting them as renders.
#[tokio::test]
async fn the_boot_sweep_tidies_small_orphans_missing_rows_and_stale_parts() {
    let cfg = TINY.replace("max_bytes     = \"6KiB\"", "max_bytes     = \"1MiB\"").replace("reap_orphans     = false", "reap_orphans     = false\nstale_part_secs  = 0");
    let (m, _t) = manager_with(&cfg);
    let mut known = Vec::new();
    for i in 0..10 {
        known.push(warm(&m, "content", &format!("known {i}")).await);
    }
    let lost = orphan(&m, &format!("zz/zz/{}.aiff", "f".repeat(64)));
    let part = orphan(&m, &format!("zz/zz/{}.part.aiff", "e".repeat(64)));
    let vrel = m.store.get_render(&known[0]).unwrap().unwrap().rel_path.unwrap();
    std::fs::remove_file(m.abs_path(&vrel)).unwrap();
    let report = m.boot_sweep().unwrap();
    for (k, v) in [("orphan_files", 1), ("orphan_files_unlinked", 1), ("missing_rows_dropped", 1), ("stale_parts_removed", 1)] {
        assert_eq!(report[k], v, "{k}: {report}");
    }
    assert_eq!(report["sweep_refused"], false);
    assert!(!lost.exists() && !part.exists());
    assert!(m.store.get_render(&known[0]).unwrap().is_none());
}

/// "since we set a count of one, it just evicts the old one." The new value
/// is never its own victim, and the steady state is one entry.
#[tokio::test]
async fn a_count_of_one_evicts_the_old_value_on_every_write() {
    let (m, _t) = manager_with(TINY);
    let sixteen = warm(&m, "pinned-count", "Pinned. 16 threads").await;
    let seventeen = warm(&m, "pinned-count", "Pinned. 17 threads").await;
    assert_eq!(entries(&m, "pinned-count"), 1);
    assert!(!holds(&m, "pinned-count", &sixteen) && holds(&m, "pinned-count", &seventeen));
    for n in 18..25 {
        let h = warm(&m, "pinned-count", &format!("Pinned. {n} threads")).await;
        assert_eq!(entries(&m, "pinned-count"), 1);
        assert!(holds(&m, "pinned-count", &h));
    }
}

/// "a count based least recently used cache with a count of, say, 10": the
/// eleventh takes the least recently USED, not the oldest created.
#[tokio::test]
async fn a_count_of_ten_evicts_by_recency() {
    let (m, _t) = manager_with(TINY);
    let mut hs = Vec::new();
    for n in 1..=10 {
        hs.push(warm(&m, "active-count", &format!("restart aui. {n} active")).await);
    }
    next_second().await;
    m.touch("active-count", &[hs[0].clone()]).unwrap();
    let eleventh = warm(&m, "active-count", "restart aui. 11 active").await;
    assert_eq!(entries(&m, "active-count"), 10);
    assert!(holds(&m, "active-count", &eleventh) && holds(&m, "active-count", &hs[0]));
    assert!(!holds(&m, "active-count", &hs[1]), "the true LRU entry survived");
}

/// The count is of WARM entries. A failed render beside the live value must
/// not push the key over budget and evict what he can hear.
#[tokio::test]
async fn a_failed_render_does_not_evict_the_warm_value_beside_it() {
    let (m, t) = manager_with(TINY);
    let current = warm(&m, "pinned-count", "Pinned. 3 threads").await;
    common::say_fails(t.path(), true);
    assert!(m.request("pinned-count", None, Some("Pinned. 4 threads".into()), NO_RACE_MS, "path").await.is_err());
    let s = m.store.key_stats("pinned-count").unwrap();
    assert_eq!((s.warm, s.entries), (1, 2));
    m.enforce_key(&m.key("pinned-count").unwrap()).unwrap();
    assert!(holds(&m, "pinned-count", &current), "a FAILED render evicted the live value");
}
