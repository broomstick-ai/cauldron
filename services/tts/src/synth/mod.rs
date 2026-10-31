//! The backend seam.
//!
//! > "the specific module that you use to call the text-to-speech will have
//! > different backends swapped out… right now there's only one module, and
//! > it's just called system." — the operator
//!
//! A backend is one file here and one arm in [`module`]. `system` (macOS
//! `say`) is the only implemented one; `kokoro` is declared and refuses.
//! Switching a key's module changes its fingerprint, which mints new hashes,
//! so no cache-invalidation code is ever written: old entries age out under
//! the key's own policy.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::watch;

use crate::error::Result;

pub mod kokoro;
pub mod system;

/// Owned by the module. For `system`: `{"voice": "Samantha"}`.
pub type ModuleConfig = Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Aiff,
}

impl AudioFormat {
    pub fn ext(&self) -> &'static str {
        match self {
            AudioFormat::Aiff => "aiff",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SynthOutput {
    pub bytes: u64,
    pub format: AudioFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleHealth {
    Ok,
    Degraded(String),
}

/// "I no longer want this": permission for a synthesis to stop early.
///
/// Latching: once fired it stays fired and [`Cancel::cancelled`] resolves at
/// once forever after, so a firing just before the backend starts listening
/// is never lost. Clones observe the same firing.
#[derive(Debug, Clone)]
pub struct Cancel(watch::Receiver<bool>);

/// The firing end, held by the manager for the life of the claim.
#[derive(Debug, Clone)]
pub struct CancelHandle(Arc<watch::Sender<bool>>);

impl CancelHandle {
    pub fn new() -> (CancelHandle, Cancel) {
        let (tx, rx) = watch::channel(false);
        (CancelHandle(Arc::new(tx)), Cancel(rx))
    }

    /// Fire it; true only for the call that actually fired, so a double
    /// cancel counts once.
    pub fn cancel(&self) -> bool {
        !self.0.send_replace(true)
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
}

impl Cancel {
    /// A token that never fires: warm work with nobody to withdraw it.
    pub fn never() -> Cancel {
        CancelHandle::new().1
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves when fired. A handle dropped WITHOUT firing means nobody is
    /// left to withdraw this, which must not read as a withdrawal, so then
    /// this never resolves.
    pub async fn cancelled(&mut self) {
        loop {
            if *self.0.borrow_and_update() {
                return;
            }
            if self.0.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

#[async_trait]
pub trait SynthModule: Send + Sync {
    /// The `module` value in `tts.toml`.
    fn name(&self) -> &'static str;

    /// Refuse a bad config at LOAD, not at the first synthesis.
    fn validate(&self, cfg: &ModuleConfig) -> Result<()>;

    /// Everything that changes the audio, as one string: the second half of
    /// the hash. For `system`, the voice — which is what the face's cache key
    /// already uses, and why the operator's cache adopts unchanged.
    fn fingerprint(&self, cfg: &ModuleConfig) -> String;

    fn format(&self, cfg: &ModuleConfig) -> AudioFormat;

    /// Synthesize to `out`, atomically: on `Ok`, `out` exists and is not empty.
    ///
    /// **A cancelled synthesis writes nothing and is NOT a failure.** The
    /// operator: *"even if I scroll past it to another item, I have to wait
    /// for it to fail before any other menu items play."* A row he has moved
    /// off is work nobody will hear, holding one of four permits. Cancelling
    /// it removes its `.part`, never becomes warm, and never becomes a
    /// `failed` row, a breaker mark or a failure count. Barge-in on audio he
    /// WANTED never fires this: that synthesis still completes and writes.
    /// [`crate::error::Error::Interrupted`] is the same rule arriving from the
    /// machine instead of the client. Ignoring `cancel` is correct, merely
    /// slower.
    async fn synthesize(&self, text: &str, cfg: &ModuleConfig, out: &Path, timeout_ms: u64, cancel: Cancel)
        -> Result<SynthOutput>;

    async fn health(&self) -> ModuleHealth;
}

/// The signal a child died of, if it died of one. Asked before any non-zero
/// exit is called a synthesis failure: a signal says the machine took the
/// process away and nothing about the text.
pub fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

/// THE registry. Nothing else in the crate enumerates modules.
pub fn module(name: &str) -> Option<Arc<dyn SynthModule>> {
    match name {
        "system" => Some(system::instance()),
        // Declared, not implemented: the name resolves so a stanza naming it
        // gets a refusal that names the missing backend, not `unknown module`.
        "kokoro" => Some(kokoro::instance()),
        _ => None,
    }
}

/// Every name [`module`] answers, for `tts.status`.
pub const MODULES: &[&str] = &["system", "kokoro"];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Two names resolve and disagree about the fingerprint of one config, so
    /// a `say` render and a Kokoro render can never share a hash.
    #[test]
    fn the_modules_cannot_share_a_cache_identity() {
        let cfg = json!({"voice": "Samantha", "kokoro_voice": "af_heart"});
        let (sys, kok) = (module("system").unwrap(), module("kokoro").unwrap());
        assert_eq!(sys.fingerprint(&cfg), "Samantha");
        assert_eq!(kok.fingerprint(&cfg), "kokoro:af_heart");
        assert!(module("nope").is_none());
        for m in MODULES {
            assert_eq!(module(m).unwrap().name(), *m, "MODULES and the registry disagree");
        }
    }

    #[test]
    fn a_bad_voice_is_refused_at_load() {
        let sys = module("system").unwrap();
        assert!(sys.validate(&json!({"voice": "Samantha"})).is_ok());
        for bad in [json!({}), json!({"voice": ""}), json!({"voice": 7})] {
            assert!(sys.validate(&bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn a_cancel_latches_counts_once_and_a_dropped_handle_is_not_a_cancel() {
        let (h, mut c) = CancelHandle::new();
        assert!(h.cancel() && !h.cancel());
        c.cancelled().await;
        assert!(c.is_cancelled() && h.is_cancelled());
        let (h2, mut c2) = CancelHandle::new();
        drop(h2);
        let waited = tokio::time::timeout(std::time::Duration::from_millis(50), c2.cancelled()).await;
        assert!(waited.is_err(), "dropping the handle read as a withdrawal");
    }
}
