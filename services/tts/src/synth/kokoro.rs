//! `kokoro`: DECLARED, and deliberately NOT IMPLEMENTED.
//!
//! The Kokoro-82M hop to a local Python service was removed on 2026-10-08 by
//! the operator's ruling: *"I'm not even using it. when we want better tts we
//! can update the rust implementation… put a not implemented where it calls
//! kokoro."* No configured key ever used it.
//!
//! A refusing module rather than a deleted arm, because the name and its
//! fingerprint namespace (`kokoro:<voice>`) are a cache identity a future
//! backend will want to inherit, and `unknown module` would be misleading
//! about a name that was configurable yesterday. Every entry point refuses
//! with `not_implemented`, which a caller can tell apart from the three
//! answers it might otherwise be confused with:
//!
//! | code | means |
//! |---|---|
//! | `not_implemented` | there is no backend; permanent; nothing was written |
//! | `backend_unavailable` | a real backend is degraded; retry later |
//! | `synthesis_failed` | a real backend ran and could not speak this text |
//!
//! — and above all from a warm render that happens to be silent: a backend
//! with nothing behind it refuses, it does not quietly produce nothing.

use std::path::Path;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde_json::Value;

use super::{AudioFormat, Cancel, ModuleConfig, ModuleHealth, SynthModule, SynthOutput};
use crate::error::{Error, Result};

pub struct KokoroModule;

/// The refusal, in the words every entry point uses.
pub const NOT_IMPLEMENTED: &str = "the `kokoro` module has no synthesis backend in this service. \
     The Kokoro-82M hop to the local Python service at http://127.0.0.1:8787 was removed on \
     2026-10-08 and nothing replaced it: `tts` manages requests and the cache, and the only \
     implemented backend is `module = \"system\"`. This is a MISSING BACKEND, not a backend \
     outage and not a silent render — no audio was produced, nothing was written to disk, and \
     retrying cannot help. Either configure `module = \"system\"`, or implement a Rust backend \
     under services/tts/src/synth/ and register it in synth::module.";

pub fn instance() -> Arc<dyn SynthModule> {
    static I: OnceLock<Arc<dyn SynthModule>> = OnceLock::new();
    I.get_or_init(|| Arc::new(KokoroModule)).clone()
}

fn refuse<T>() -> Result<T> {
    Err(Error::wire("not_implemented", NOT_IMPLEMENTED))
}

#[async_trait]
impl SynthModule for KokoroModule {
    fn name(&self) -> &'static str {
        "kokoro"
    }

    /// Refused at load — boot, `tts.reload`, `tts.upsert_key` — the earliest
    /// point anyone can be told.
    fn validate(&self, _cfg: &ModuleConfig) -> Result<()> {
        refuse()
    }

    /// Kept: a cache IDENTITY, not a capability.
    fn fingerprint(&self, cfg: &ModuleConfig) -> String {
        format!("kokoro:{}", cfg.get("kokoro_voice").and_then(Value::as_str).unwrap_or("af_heart"))
    }

    fn format(&self, _cfg: &ModuleConfig) -> AudioFormat {
        AudioFormat::Aiff
    }

    /// Refuses before touching the filesystem: no directory, no `.part`.
    async fn synthesize(&self, _: &str, _: &ModuleConfig, _: &Path, _: u64, _: Cancel) -> Result<SynthOutput> {
        refuse()
    }

    async fn health(&self) -> ModuleHealth {
        ModuleHealth::Degraded(NOT_IMPLEMENTED.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_key_can_be_configured_onto_it_whatever_the_config() {
        let a = KokoroModule.validate(&json!({"kokoro_voice": "af_heart"})).unwrap_err();
        let b = KokoroModule.validate(&json!({})).unwrap_err();
        assert_eq!(a.code(), Some("not_implemented"));
        assert_eq!(a.to_string(), b.to_string(), "nothing about the config is what is wrong");
        assert!(a.to_string().contains("has no synthesis backend"));
    }

    /// The property the file exists for: a coded refusal that is neither
    /// retryable code, and NOTHING on disk — not the render, not a `.part`,
    /// not even the directory.
    #[tokio::test]
    async fn synthesize_refuses_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("ab/cd/deadbeef.aiff");
        let e = KokoroModule.synthesize("hello", &json!({}), &out, 1_000, Cancel::never()).await.unwrap_err();
        assert_eq!(e.code(), Some("not_implemented"));
        assert!(e.to_string().contains("MISSING BACKEND") && e.to_string().contains("module = \"system\""));
        assert!(!out.exists() && !super::super::system::part_path(&out).exists());
        assert!(!dir.path().join("ab").exists());
    }

    #[tokio::test]
    async fn it_is_never_healthy_and_its_namespace_survives() {
        assert_eq!(KokoroModule.health().await, ModuleHealth::Degraded(NOT_IMPLEMENTED.into()));
        assert_eq!(KokoroModule.fingerprint(&json!({"voice": "Samantha"})), "kokoro:af_heart");
    }
}
