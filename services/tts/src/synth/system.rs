//! The `system` module: macOS `say`.
//!
//! `say -o <hash>.part.aiff -v <voice> -- <text>`, in its own process group,
//! timed out and killed BY GROUP, then renamed into place. Three details, each
//! of which cost the operator real silence:
//!
//! 1. **`--` ends option parsing.** Without it an utterance starting with a
//!    hyphen (`- **F1** FIXED`, a spoken `---`) is read as a flag, `say` exits 1
//!    having produced nothing, and the text misses forever.
//! 2. **The text never passes through a shell.** Direct argv only.
//! 3. **The `.part` file ends in `.aiff`**, because `say` picks its output
//!    format from the extension.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use serde_json::Value;

use super::{AudioFormat, Cancel, ModuleConfig, ModuleHealth, SynthModule, SynthOutput};
use crate::error::{Error, Result};

/// The `say` children this daemon is running right now.
///
/// The orphan reaper kills `say` processes writing into this service's audio
/// directory that it does not own. It once excluded only the daemon's own
/// pid — and a child has a different pid — so every sweep SIGTERMed every
/// synthesis in flight, and each came back as `say exited signal: 15`, a
/// durable failure on an innocent sentence. Ownership is a fact the reaper
/// reads now: a pid is here exactly while this process waits on that child.
static LIVE_CHILDREN: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

pub fn live_child_pids() -> Vec<u32> {
    LIVE_CHILDREN.lock().map(|s| s.iter().copied().collect()).unwrap_or_default()
}

/// Registers a child in [`LIVE_CHILDREN`] and removes it on drop, so every
/// early return below — cancel, timeout, bad status, missing output — gives
/// the pid back.
struct Owned(u32);

impl Owned {
    fn register(pid: u32) -> Self {
        if pid != 0 {
            if let Ok(mut s) = LIVE_CHILDREN.lock() {
                s.insert(pid);
            }
        }
        Owned(pid)
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        if let Ok(mut s) = LIVE_CHILDREN.lock() {
            s.remove(&self.0);
        }
    }
}

pub struct SystemModule;

pub fn instance() -> Arc<dyn SynthModule> {
    static I: OnceLock<Arc<dyn SynthModule>> = OnceLock::new();
    I.get_or_init(|| Arc::new(SystemModule)).clone()
}

/// `say`, or `$TTS_SAY_BIN` — how every test runs against a fixture and never
/// against the operator's speech service.
pub fn say_bin() -> String {
    std::env::var("TTS_SAY_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "say".into())
}

/// `<x>.aiff` becomes `<x>.part.aiff`.
pub fn part_path(out: &Path) -> PathBuf {
    let s = out.to_string_lossy();
    PathBuf::from(format!("{}.part.aiff", s.strip_suffix(".aiff").unwrap_or(&s)))
}

fn voice_of(cfg: &ModuleConfig) -> &str {
    cfg.get("voice").and_then(Value::as_str).unwrap_or("Samantha")
}

#[async_trait]
impl SynthModule for SystemModule {
    fn name(&self) -> &'static str {
        "system"
    }

    fn validate(&self, cfg: &ModuleConfig) -> Result<()> {
        match cfg.get("voice") {
            Some(Value::String(v)) if !v.is_empty() => Ok(()),
            Some(Value::String(_)) => Err(Error::Config("system module: voice must not be empty".into())),
            Some(other) => Err(Error::Config(format!("system module: voice must be a string, got {other}"))),
            None => Err(Error::Config(
                "system module: module_config.voice is required. The voice IS half the cache identity, \
                 so it is never defaulted silently — the operator's cache is keyed on \"Samantha\""
                    .into(),
            )),
        }
    }

    fn fingerprint(&self, cfg: &ModuleConfig) -> String {
        voice_of(cfg).to_string()
    }

    fn format(&self, _cfg: &ModuleConfig) -> AudioFormat {
        AudioFormat::Aiff
    }

    async fn synthesize(
        &self,
        text: &str,
        cfg: &ModuleConfig,
        out: &Path,
        timeout_ms: u64,
        mut cancel: Cancel,
    ) -> Result<SynthOutput> {
        if cancel.is_cancelled() {
            return Err(Error::wire("cancelled", "withdrawn before synthesis began"));
        }
        let part = part_path(out);
        if let Some(dir) = out.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let _ = tokio::fs::remove_file(&part).await;
        let child = tokio::process::Command::new(say_bin())
            .arg("-o")
            .arg(&part)
            .args(["-v", voice_of(cfg), "--", text])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| Error::wire("synthesis_failed", format!("spawn {}: {e}", say_bin())))?;
        let pid = child.id().unwrap_or(0);
        // Owned before the first await, so the reaper never sees this child
        // without also seeing that it is ours.
        let _owned = Owned::register(pid);

        // A wedged backend and a client that changed its mind are torn down
        // the same way — the whole GROUP, because `say` talks to a shared
        // speech service — and REPORTED differently: a timeout is a failure,
        // a cancellation is nothing at all.
        let waited = tokio::select! {
            r = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), child.wait_with_output()) => r,
            _ = cancel.cancelled() => {
                kill_group(pid);
                let _ = tokio::fs::remove_file(&part).await;
                return Err(Error::wire("cancelled", "withdrawn by client"));
            }
        };
        let output = match waited {
            Ok(r) => r.map_err(|e| Error::wire("synthesis_failed", format!("wait: {e}")))?,
            Err(_) => {
                kill_group(pid);
                let _ = tokio::fs::remove_file(&part).await;
                return Err(Error::wire("synthesis_failed", format!("timeout after {timeout_ms}ms")));
            }
        };
        if !output.status.success() {
            let _ = tokio::fs::remove_file(&part).await;
            // Both early returns above leave before the status is read, so a
            // signalled child here was killed by somebody else.
            if let Some(signal) = super::signal_of(&output.status) {
                return Err(Error::Interrupted { signal });
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::wire(
                "synthesis_failed",
                format!("{} exited {}: {}", say_bin(), output.status, stderr.lines().next().unwrap_or("").trim()),
            ));
        }
        let bytes = tokio::fs::metadata(&part)
            .await
            .map_err(|e| Error::wire("synthesis_failed", format!("output missing: {e}")))?
            .len();
        if bytes == 0 {
            // Never a warm entry pointing at an empty file.
            let _ = tokio::fs::remove_file(&part).await;
            return Err(Error::wire("synthesis_failed", "empty output"));
        }
        tokio::fs::rename(&part, out).await?;
        Ok(SynthOutput { bytes, format: AudioFormat::Aiff })
    }

    async fn health(&self) -> ModuleHealth {
        ModuleHealth::Ok
    }
}

/// SIGTERM the process GROUP, then SIGKILL it three seconds later.
pub fn kill_group(pid: u32) {
    if pid == 0 {
        return;
    }
    let pgid = pid as libc::pid_t;
    // SAFETY: killpg only sends a signal; the group is one this module made
    // with `process_group(0)`, or one the reaper selected.
    unsafe { libc::killpg(pgid, libc::SIGTERM) };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        // SAFETY: as above.
        unsafe { libc::killpg(pgid, libc::SIGKILL) };
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_part_file_keeps_the_aiff_extension() {
        assert_eq!(part_path(Path::new("/a/ab/cd/x.aiff")), Path::new("/a/ab/cd/x.part.aiff"));
        assert_eq!(part_path(Path::new("/a/x")), Path::new("/a/x.part.aiff"));
    }
}
