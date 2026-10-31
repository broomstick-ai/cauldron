//! The `system` backend, driven directly against the fixture.

mod common;

use std::path::{Path, PathBuf};

use serde_json::json;
use tts::error::Error;
use tts::synth::system::{live_child_pids, part_path, SystemModule};
use tts::synth::{Cancel, CancelHandle, SynthModule};

/// An output path the fixture can read its data dir back off.
fn out_in(dir: &Path, name: &str) -> PathBuf {
    dir.join("audio/ab/cd").join(format!("{name}.aiff"))
}

async fn say(dir: &Path, text: &str, timeout_ms: u64) -> tts::Result<tts::synth::SynthOutput> {
    common::set_env();
    SystemModule.synthesize(text, &json!({"voice": "Samantha"}), &out_in(dir, "x"), timeout_ms, Cancel::never()).await
}

/// `--` ends option parsing: each of these was a permanent miss without it.
/// The fixture exits 1 on a leading-hyphen word before `--`, exactly as `say`.
#[tokio::test]
async fn a_leading_hyphen_utterance_reaches_the_backend_intact() {
    let dir = tempfile::tempdir().unwrap();
    for text in ["- **F1 (HIGH)** FIXED", "---", "-v is not a flag here", "--help"] {
        let o = say(dir.path(), text, 5000).await.unwrap_or_else(|e| panic!("{text:?}: {e}"));
        assert_eq!(o.bytes, 2048);
    }
    let calls = common::say_calls(dir.path());
    assert!(calls.iter().any(|c| c == "---") && calls.iter().any(|c| c == "--help"), "{calls:?}");
}

#[tokio::test]
async fn an_empty_output_is_a_failure_and_leaves_no_file() {
    let dir = tempfile::tempdir().unwrap();
    common::say_writes_nothing(dir.path());
    let e = say(dir.path(), "silence", 5000).await.unwrap_err();
    assert_eq!(e.code(), Some("synthesis_failed"));
    assert!(!out_in(dir.path(), "x").exists() && !part_path(&out_in(dir.path(), "x")).exists());
}

/// A wedged backend is bounded, killed by group, and reported as a failure.
#[tokio::test]
async fn a_wedged_synthesis_times_out_as_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    common::say_takes_ms(dir.path(), 5000);
    let started = std::time::Instant::now();
    let e = say(dir.path(), "this one hangs", 150).await.unwrap_err();
    assert_eq!(e.code(), Some("synthesis_failed"));
    assert!(e.to_string().contains("timeout"), "{e}");
    assert!(started.elapsed() < std::time::Duration::from_secs(3), "the timeout did not bound the wait");
    assert!(!out_in(dir.path(), "x").exists());
}

/// Killed from outside is INTERRUPTED, never `synthesis_failed`: the signal
/// says nothing about the text.
#[tokio::test]
async fn a_child_killed_by_a_signal_is_interrupted_not_failed() {
    let dir = tempfile::tempdir().unwrap();
    common::say_dies_on_signal(dir.path(), true);
    match say(dir.path(), "reaped", 5000).await {
        Err(Error::Interrupted { signal }) => assert_eq!(signal, libc_sigterm()),
        other => panic!("expected Interrupted, got {other:?}"),
    }
    common::say_dies_on_signal(dir.path(), false);
    common::say_fails(dir.path(), true);
    assert_eq!(say(dir.path(), "refused", 5000).await.unwrap_err().code(), Some("synthesis_failed"));
}

fn libc_sigterm() -> i32 {
    15
}

/// A withdrawal mid-synthesis tears the child down at once, reports
/// `cancelled`, and leaves neither the render nor its `.part`.
#[tokio::test]
async fn a_cancel_mid_synthesis_stops_it_and_writes_nothing() {
    common::set_env();
    let dir = tempfile::tempdir().unwrap();
    common::say_takes_ms(dir.path(), 5000);
    let (handle, cancel) = CancelHandle::new();
    let out = out_in(dir.path(), "x");
    let task = {
        let out = out.clone();
        tokio::spawn(async move {
            SystemModule.synthesize("long", &json!({"voice": "Samantha"}), &out, 60_000, cancel).await
        })
    };
    assert!(common::eventually(3000, || !common::say_calls(dir.path()).is_empty()).await);
    let started = std::time::Instant::now();
    assert!(handle.cancel());
    let e = task.await.unwrap().unwrap_err();
    assert_eq!(e.code(), Some("cancelled"));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(!out.exists() && !part_path(&out).exists());
}

/// While a child runs it is listed as OURS — the fact the orphan reaper reads
/// — and it is unlisted on the way out, however the synthesis ended.
#[tokio::test]
async fn a_running_child_is_owned_and_released() {
    common::set_env();
    let dir = tempfile::tempdir().unwrap();
    common::say_takes_ms(dir.path(), 600);
    let before = live_child_pids();
    let out = out_in(dir.path(), "owned");
    let task =
        tokio::spawn(async move { SystemModule.synthesize("t", &json!({"voice": "Samantha"}), &out, 5000, Cancel::never()).await });
    assert!(common::eventually(3000, || live_child_pids().len() > before.len()).await, "the child was never owned");
    let mine: Vec<u32> = live_child_pids().into_iter().filter(|p| !before.contains(p)).collect();
    task.await.unwrap().unwrap();
    assert!(live_child_pids().iter().all(|p| !mine.contains(p)), "a finished child is still owned");
}
