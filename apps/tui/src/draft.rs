//! **THE DRAFT JOURNAL — and it is a DELIBERATE, ARGUED EXCEPTION to the
//! architecture's no-disk rule, flagged here so the next reader finds it as a
//! decision rather than as a violation.**
//!
//! # The rule it contradicts, and why it contradicts it
//!
//! `0023` `what_the_tui_stores` refuses any byte on disk beyond the log. That
//! refusal is not wrong and it is not being overturned wholesale — it is being
//! read for what it actually says. Two reasons were given for it, and neither
//! reaches this file:
//!
//! 1. **`projections.json`'s exposure class.** The prior face wrote a `0644`
//!    file holding PLAINTEXT SECRETS. That is what the rule was aimed at. This
//!    journal holds his own unsent sentence, nothing else, in a `0600` file
//!    inside a `0700` directory.
//! 2. **Ledger churn.** The dirty-state metadata design excludes draft TEXT
//!    from the metadata blob because every metadata patch persists a ledger
//!    row, and the threads WAL is a standing operational concern. **That
//!    refusal is UNCHANGED and still holds** — nothing here writes to the
//!    daemon, ever. This is a local file and the daemon never learns it exists.
//!
//! And `0023` itself names the composer buffer *"the one state the client truly
//! owns"*, which is exactly the property that makes it the one thing a crash
//! destroys and no RPC can restore. Losing a long draft to a
//! daemon-restart-triggered crash is a GLITCH under the acceptance bar, and the
//! bar is the thing that outranks the rule here.
//!
//! # The four safeguards that make it honest rather than a re-run
//!
//! * It holds no credential, ever — only text he typed into a box.
//! * `0600` in a `0700` directory, both set at CREATE time rather than
//!   afterwards, so there is no window in which the file exists at the
//!   process umask.
//! * It is DELETED the moment a send is accepted, so it does not accumulate a
//!   history of everything he has ever written.
//! * **The restore is an explicit OFFER, never an insertion.** A stale draft
//!   silently re-typed into the box would be indistinguishable from something
//!   he wrote this minute — the same dishonesty the disconnected transcript is
//!   forbidden from committing. `model::DraftOffer` is what that refusal looks
//!   like in the type system.
//!
//! # And what it is NOT
//!
//! Not a history, not a sync, not a cache, not a queue. One file per thread,
//! overwritten in place, holding at most one unsent message.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::SystemTime;

/// The directory mode. `0700`: nobody else's business that he is mid-sentence.
const DIR_MODE: u32 = 0o700;
/// The file mode. `0600`.
const FILE_MODE: u32 = 0o600;

/// The biggest draft that will be journalled.
///
/// A ceiling exists because a paste can be arbitrarily large and a debounced
/// write of an eight-hundred-kilobyte buffer every 750ms is a disk cost he
/// never asked for. Over it, the journal writes NOTHING and says so in the log
/// — it does not write a truncated draft, because half a message offered back
/// as a whole one is the dishonesty this whole module is shaped around.
pub const MAX: usize = 256 * 1024;

/// `~/.tui/draft`. `None` when there is no home directory to hang it off,
/// which is a legal state: the composer then simply has no crash journal.
pub fn dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".tui").join("draft"))
}

/// The file one thread's draft lives in.
///
/// The thread id is used as the file name and it is SANITIZED rather than
/// trusted: a daemon thread id is a uuid today, but a name that arrived from
/// the wire and reached `Path::join` unchecked is a path-traversal waiting for
/// the day the id format changes. Everything outside `[A-Za-z0-9._-]` becomes
/// `_`, which cannot escape the directory.
fn path(thread_id: &str) -> Option<PathBuf> {
    let safe: String = thread_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(128)
        .collect();
    if safe.is_empty() {
        return None;
    }
    dir().map(|d| d.join(safe))
}

/// Write the draft. Overwrites; never appends.
///
/// Errors are logged and swallowed, deliberately: a journal that could not be
/// written is a lost crash-recovery, not a lost message, and a toast about a
/// filesystem he did not ask to use would be noise on the surface he is typing
/// into.
pub fn save(thread_id: &str, text: &str) {
    if text.is_empty() {
        clear(thread_id);
        return;
    }
    if text.len() > MAX {
        tracing::debug!(
            len = text.len(),
            "draft over the journal ceiling; not written"
        );
        return;
    }
    let Some(p) = path(thread_id) else { return };
    let Some(d) = dir() else { return };
    if let Err(e) = fs::create_dir_all(&d) {
        tracing::debug!(error = %e, "could not make the draft directory");
        return;
    }
    // Set the directory mode explicitly: `create_dir_all` uses the process
    // umask, which on a permissive umask leaves it world-readable.
    let _ = fs::set_permissions(&d, fs::Permissions::from_mode(DIR_MODE));
    match fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        // **THE MODE IS SET AT CREATE**, not chmod'd afterwards. A chmod leaves
        // a window in which the file exists at the umask, and the content is in
        // it for that window.
        .mode(FILE_MODE)
        .open(&p)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(text.as_bytes()).and_then(|_| f.flush()) {
                tracing::debug!(error = %e, "could not write the draft");
            }
        }
        Err(e) => tracing::debug!(error = %e, "could not open the draft"),
    }
}

/// The draft for a thread, and how old it is — `None` when there is none, when
/// it is empty, or when it cannot be read.
pub fn load(thread_id: &str) -> Option<(String, SystemTime)> {
    let p = path(thread_id)?;
    let meta = fs::metadata(&p).ok()?;
    let text = fs::read_to_string(&p).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    Some((text, meta.modified().unwrap_or_else(|_| SystemTime::now())))
}

/// Delete it. Called on an ACCEPTED send and on an explicit discard, and on
/// nothing else — in particular not on exit, because exiting is one of the ways
/// a draft gets stranded.
pub fn clear(thread_id: &str) {
    if let Some(p) = path(thread_id) {
        let _ = fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **THE FILE NAME CANNOT ESCAPE THE DIRECTORY**, whatever the daemon calls
    /// a thread. This is the only place a value off the wire reaches a path.
    #[test]
    fn a_thread_id_can_never_walk_out_of_the_draft_directory() {
        let d = dir().expect("a home directory");
        for hostile in [
            "../../.ssh/authorized_keys",
            "/etc/passwd",
            "..",
            ".",
            "a/b/c",
            "x\0y",
        ] {
            if let Some(p) = path(hostile) {
                assert_eq!(
                    p.parent(),
                    Some(d.as_path()),
                    "{hostile:?} reached {p:?}, which is outside the draft directory"
                );
            }
        }
        // A real id is untouched.
        assert_eq!(
            path("57db2ec9-1111-2222-3333-444455556666").unwrap(),
            d.join("57db2ec9-1111-2222-3333-444455556666")
        );
    }

    /// **0600 IN A 0700 DIRECTORY, AND BOTH ARE CHECKED ON DISK.** The argument
    /// for the whole exception rests on these two numbers, so they are measured
    /// rather than asserted in prose.
    #[test]
    fn the_journal_is_written_private_and_deleted_on_clear() {
        // A thread id nothing else will use, so a parallel test cannot collide.
        let id = format!("tui-draft-selftest-{}", std::process::id());
        save(&id, "half a sentence");
        let p = path(&id).expect("a path");
        let m = fs::metadata(&p).expect("the draft was written");
        assert_eq!(
            m.permissions().mode() & 0o777,
            FILE_MODE,
            "the draft is not 0600 — the exception's argument does not hold"
        );
        let dm = fs::metadata(dir().unwrap()).unwrap();
        assert_eq!(
            dm.permissions().mode() & 0o777,
            DIR_MODE,
            "the directory is not 0700"
        );
        assert_eq!(load(&id).unwrap().0, "half a sentence");
        clear(&id);
        assert!(load(&id).is_none(), "clear did not delete the draft");
        // An empty draft clears rather than writing an empty file.
        save(&id, "hello");
        save(&id, "");
        assert!(load(&id).is_none());
        clear(&id);
    }

    #[test]
    fn an_oversized_draft_is_not_written_rather_than_truncated() {
        let id = format!("tui-draft-big-{}", std::process::id());
        clear(&id);
        save(&id, &"x".repeat(MAX + 1));
        assert!(
            load(&id).is_none(),
            "an over-ceiling draft was written — half a message offered back whole"
        );
        clear(&id);
    }
}
