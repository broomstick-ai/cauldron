//! **PL-2b — the per-thread SOURCE CENSUS, read-only.**
//!
//! `threads/{thread_id}/source_census` answers, for one thread: where its
//! Claude Code transcript is and whether it exists, how many files sit in its
//! `<data_dir>/turns/<thread_id>/` capture directory, its account, and its
//! project dir (cwd). It exists so PL-2's census can be measured through the
//! door instead of by re-deriving paths outside the daemon.
//!
//! **Read-only by construction.** Nothing in this module writes, creates a
//! directory, or opens a file for writing; no promise is opened and no event is
//! emitted. It is a child module of `runtime` only so it can reuse the
//! manager's own private lookups (`transcript_store_of`, `data_dir`) rather
//! than re-deriving them; it edits no function there.

use serde_json::{json, Value};

use super::ThreadManager;
use crate::error::{Error, Result};
use crate::settings::ThreadSettings;

impl ThreadManager {
    /// How many entries `<data_dir>/turns/<thread_id>/` holds — the raw count.
    ///
    /// v1 does NOT distinguish a turn's durable `.jsonl` stdout capture from
    /// its `.mcp.json` sidecar (both are joined at `data_dir.join("turns")
    /// .join(thread_id)` in `runtime.rs`); every directory entry counts once.
    /// An absent directory is `0`, never an error: a thread that has not run a
    /// turn on this daemon has no captures, and that is the answer.
    ///
    /// **PL-2c: anything else that stops the read is an ERROR, never `0`.** A
    /// `turns/<id>/` that exists but cannot be listed (permissions, not a
    /// directory, an entry that fails mid-listing) used to count `0` — the same
    /// answer as "no captures", which is the one a census must never fake. Only
    /// `NotFound` on the directory itself means "none".
    pub fn turn_capture_count(&self, thread_id: &str) -> Result<usize> {
        let dir = self.data_dir.join("turns").join(thread_id);
        let unreadable = |e: std::io::Error| {
            Error::Config(format!("turns dir {} is unreadable: {e}", dir.display()))
        };
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(unreadable(e)),
        };
        let mut count = 0;
        for entry in entries {
            entry.map_err(unreadable)?;
            count += 1;
        }
        Ok(count)
    }

    /// **`threads/{thread_id}/source_census`** — transcript `{path, exists}`,
    /// the `turns/` capture count, account and cwd, for one thread.
    ///
    /// The account and cwd are the thread row's (`store.get_thread`) and its
    /// stored settings' (`ThreadSettings::from_stored(..).cwd`), exactly as the
    /// `budget_get` arm resolves a thread. An unknown thread is an ERROR, never
    /// a zero-filled row.
    ///
    /// The transcript is located with `crate::accounts::locate_transcript`
    /// (through the thin `transcript_presence` wrapper) under the thread's OWN
    /// account's store. PL-2's census finding, verbatim:
    ///
    /// > `locate_transcript` searches only the thread's current account store,
    /// > so **a PL-5 backfill would report it as having no source.** PL-5 should
    /// > search every store.
    ///
    /// This verb does not fix that: `exists: false` here means "not in the
    /// current account's store", not "nowhere". A thread whose account is
    /// unrecorded, or no longer registered, reports `path: null,
    /// exists: false` and says why in `transcript.store_error`, as does a
    /// store that exists but cannot be read (PL-2d).
    pub fn source_census(&self, thread_id: &str) -> Result<Value> {
        let row = self
            .store
            .get_thread(thread_id)?
            .ok_or_else(|| Error::Config(format!("no such thread: {thread_id}")))?;
        let settings = ThreadSettings::from_stored(&row.settings_json)?;

        let (path, exists, store_error) = match row.account.as_deref() {
            None => (None, false, Some("the thread has no recorded account".to_string())),
            Some(account) => match self.transcript_store_of(account) {
                Err(e) => (None, false, Some(e.to_string())),
                Ok(store) => match crate::accounts::transcript_home() {
                    None => (None, false, Some("HOME is not set: no transcript store".to_string())),
                    Some(home) => match crate::accounts::transcript_presence(
                        store.as_deref(),
                        &home,
                        &row.id,
                    ) {
                        Ok((p, ex)) => (p, ex, None),
                        // PL-2d (PL-2c-1): a store that exists but cannot be
                        // read is "could not look", never "not there".
                        Err(e) => (None, false, Some(e.to_string())),
                    },
                },
            },
        };

        Ok(json!({
            "thread_id": row.id,
            "account": row.account,
            "cwd": settings.cwd,
            "transcript": {
                "path": path.map(|p| p.to_string_lossy().to_string()),
                "exists": exists,
                "store_error": store_error,
            },
            "turns": {
                "count": self.turn_capture_count(&row.id)?,
            },
        }))
    }
}
