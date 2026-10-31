//! **Holding an environment — singleton-ness the KERNEL enforces.**
//!
//! One process per environment hosts that environment's services. This module
//! is how "one" is established, and the whole of its design is a response to a
//! measured failure.
//!
//! # The failure this replaces, in full, because a replacement that does not
//! name it will re-import it
//!
//! `~/.threads-supervisor/supervise-service.sh` established its singleton with
//! a **pidfile and `kill -0`**: the holder wrote its pid into a file, and a new
//! holder read that file and exited if the recorded pid was alive.
//!
//! On 2026-09-22 the machine rebooted at 18:35:11. The `tts` supervisor from
//! the *previous* boot (pid 1115, started Sep 18) never received a SIGTERM at
//! shutdown, so its `EXIT` trap never ran and its pidfile survived the reboot.
//! At 18:36:20 launchd started the new supervisor; it read `1115`; `kill -0
//! 1115` **succeeded, because the pid had been reused by an unrelated process
//! on the new boot**; it logged "another supervisor is already running (pid
//! 1115)" and exited **0**. launchd honoured the clean exit. `tts` ran
//! unsupervised for sixteen days — in fact it was AUI that had spawned the
//! running `tts`, 74 s after that boot and 5 s after the supervisor quit.
//!
//! Three separate properties of that mechanism each failed:
//!
//! 1. **The evidence was wrong.** A pid is a small recycled integer. "Is pid N
//!    alive" and "is the process that wrote this file alive" are different
//!    questions, and the first is not evidence about the second.
//! 2. **The record outlived the process.** A pidfile is released by a *tidy
//!    shutdown*. A reboot, a `SIGKILL` and a panic are exactly the cases a
//!    singleton has to survive, and none of them is tidy.
//! 3. **The failure was indistinguishable from success.** Exit 0 meant "do not
//!    revive me", so the one mechanism below the supervisor was told to stand
//!    down by the bug.
//!
//! Cauldron's own seam contract already rejects this evidence class in so many
//! words ("staleness is proven by a handshake carrying a boot token, never pid
//! liveness"). The hold had to as well.
//!
//! # What this does instead
//!
//! An **advisory file lock taken by the kernel** —
//! [`std::fs::File::try_lock`], stable since Rust 1.89 — on
//! `<data_root>/supervisor.lock`.
//!
//! Every one of the three failures above is answered by a property of the
//! mechanism rather than by care on the caller's part:
//!
//! 1. **Nothing is read in order to decide.** The decision is the kernel's
//!    answer to "may I take this lock", about *this open file description*.
//!    There is no number to compare and no number to be wrong.
//! 2. **The kernel releases it when the holder dies, by any means.** A
//!    `SIGKILL`, a panic, a reboot and a power cut all close the file
//!    descriptor, and closing it drops the lock. There is no state left behind
//!    to go stale, which is why this module has no concept of a stale hold and
//!    no breaker — see the note on [`crate::lock`] below.
//! 3. **A refused hold is a REFUSAL, with its own exit code**, and the daemon
//!    exits non-zero on it. Nothing downstream may read "somebody else holds
//!    this" as "all is well".
//!
//! # Why not `crate::lock`
//!
//! [`crate::lock::with_lock`] is right for what it does and wrong for this.
//! It is a `create_new` file carrying a pid and a timestamp, with a breaker
//! that removes any lock older than
//! [`crate::lock::STALE_AFTER`] (30 s) — correct for a sub-second
//! read-modify-write of `seams.json`, where a lock still held after half a
//! minute really does mean the holder is gone.
//!
//! A hold on an environment lasts for **days**. A stale-by-age breaker would
//! break a live holder's hold every thirty seconds, so the two cannot share a
//! mechanism, and it carries a pid-and-time record, which is the evidence class
//! above. Both files exist, side by side, with different names and different
//! lifetimes, deliberately.
//!
//! # The note beside the lock, and why it is ADVISORY
//!
//! A kernel lock cannot tell a refused caller who holds it. So a holder writes
//! one line of JSON into the locked file *after* it has the lock, and a refused
//! caller reads that line *for its error message only*. The ordering matters
//! twice: the note is written after the lock, so an unlocked file cannot carry
//! one that is current, and the note is read after the refusal, so nothing in
//! the decision path ever reads it. [`Attempt::Taken::note`] is `Option` for
//! that reason — "I could not tell you who" is a legal answer and does not
//! soften the refusal.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The file, inside the environment's data root. Named for what holds it, not
/// for what it protects: `seams.lock` is already the seam table's
/// read-modify-write lock and the two must never be confused.
pub const HOLD_FILE: &str = "supervisor.lock";

/// **The live holder's own account of itself. Advisory, never evidence.**
///
/// Everything here is written by the holder after it took the lock and is read
/// only to compose an error message. None of it is consulted in order to
/// decide whether the hold is available — that decision is the kernel's, and a
/// `Note` that disagrees with the kernel loses, every time, without a vote.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    /// The environment being held.
    pub env: String,
    /// The holder's pid. **Written for a human to read.** It is not compared,
    /// not probed, and not signalled on the strength of this file.
    pub pid: u32,
    /// The process name the holder runs under — `cauldron` in prod,
    /// `cauldron-<env>` elsewhere — so a refused caller's message names the
    /// launchd job to look at.
    pub process_name: String,
    /// The holder's own executable path.
    pub binary: String,
    /// Epoch seconds at which the hold was taken.
    pub since: u64,
    /// The commit the holder is running, so two holders of two versions are
    /// distinguishable in the message.
    #[serde(default)]
    pub commit: String,
}

/// A live hold. **Dropping it releases the hold**, and so does dying.
///
/// The `File` is the hold: it is kept for its open file description and
/// nothing else reads it. It is deliberately private — a caller that could
/// reach the handle could close it, which is the one way to release the hold
/// while continuing to act as the holder.
pub struct Hold {
    file: File,
    path: PathBuf,
    note: Note,
}

impl Hold {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn note(&self) -> &Note {
        &self.note
    }

    /// Re-stamp the note with a fresh `since`/`commit` after the holder learns
    /// something about itself it did not know at acquisition time.
    ///
    /// **This does not re-take anything.** The hold is already held; this
    /// rewrites the advisory line inside it. It cannot fail the caller's
    /// liveness in any way, so its error is reported and swallowed by the
    /// daemon rather than ending a supervision loop over a cosmetic write.
    pub fn restamp(&mut self, note: Note) -> Result<()> {
        self.note = note;
        write_note(&mut self.file, &self.note)
    }
}

/// What came of asking for the hold. **Not a `Result`**, because "somebody
/// else holds this environment" is not an error in the mechanism — it is the
/// mechanism working, and it carries different information than a failure to
/// open a file does.
pub enum Attempt {
    /// It is ours until the `Hold` drops or this process dies.
    Held(Hold),
    /// Another process holds it. The kernel said so.
    Taken {
        path: PathBuf,
        /// The holder's advisory note, if there was a readable one. `None` is
        /// a legal answer and changes nothing about the refusal.
        note: Option<Note>,
    },
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn write_note(file: &mut File, note: &Note) -> Result<()> {
    let mut text = serde_json::to_string(note)?;
    text.push('\n');
    // Truncate only now, with the lock already held. Truncating at open time
    // would destroy a live holder's note before the kernel had been asked
    // whether we may have the lock at all.
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    Ok(())
}

fn read_note(path: &Path) -> Option<Note> {
    let mut f = File::open(path).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    serde_json::from_str(s.trim()).ok()
}

/// **Try to hold `env`'s data root. One call, one syscall's worth of
/// decision.**
///
/// The file is created if absent and never truncated before the lock is taken.
/// `O_RDWR` rather than `O_WRONLY` because the note has to be readable by the
/// holder itself for [`Hold::note`] round-trips, and because a write-only
/// handle buys nothing here.
pub fn try_hold(data_root: &Path, note: Note) -> Result<Attempt> {
    let path = crate::guard::require_inside(data_root, &data_root.join(HOLD_FILE))?;
    std::fs::create_dir_all(data_root)
        .with_context(|| format!("creating {}", data_root.display()))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening the environment hold {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            // READ THE NOTE ONLY NOW, AFTER THE KERNEL HAS ALREADY DECIDED.
            // The ordering is the point: nothing in the decision path above
            // this line reads a byte of the file.
            return Ok(Attempt::Taken { note: read_note(&path), path });
        }
        Err(std::fs::TryLockError::Error(e)) => {
            return Err(e).with_context(|| {
                format!(
                    "the kernel refused to answer whether {} can be locked. That is NOT read as \
                     `the hold is free`: a supervisor that cannot establish singleton-ness and \
                     proceeds anyway is the second launcher this mechanism exists to make \
                     impossible.",
                    path.display()
                )
            });
        }
    }
    // THE LOCK IS ON THIS OPEN FILE DESCRIPTION, so the `File` is MOVED into
    // the `Hold` and never cloned. A `try_clone` here would duplicate the
    // descriptor, and closing either copy on some platforms drops the lock —
    // which would make the hold releasable by a path nobody was looking at.
    let note_copy = note.clone();
    let mut hold = Hold { file, path, note };
    write_note(&mut hold.file, &note_copy)?;
    Ok(Attempt::Held(hold))
}

/// The note a holder stamps, assembled from what a holder knows about itself.
pub fn note_for(env: &str, process_name: &str, commit: &str) -> Note {
    Note {
        env: env.to_string(),
        pid: std::process::id(),
        process_name: process_name.to_string(),
        binary: std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        since: now(),
        commit: commit.to_string(),
    }
}

/// **The sentence a refused holder prints.** One place, so the daemon and
/// `cauldron start` cannot describe the same refusal two different ways.
pub fn refusal(env: &str, path: &Path, note: Option<&Note>) -> String {
    let who = match note {
        Some(n) => format!(
            "The holder's own note says: pid {}, process `{}`, binary {}, holding since epoch \
             {}{}. THAT NOTE IS ADVISORY — it was read only to write this sentence. The refusal \
             above came from the kernel, which was asked whether this process may take the lock \
             and said no.",
            n.pid,
            n.process_name,
            n.binary,
            n.since,
            if n.commit.is_empty() {
                String::new()
            } else {
                format!(", commit {}", n.commit.chars().take(12).collect::<String>())
            }
        ),
        None => "There is no readable note beside the lock, which changes nothing: the kernel \
                 holds the answer and the answer was no. A holder writes its note after taking \
                 the lock, so a missing note means only that it has not got that far."
            .to_string(),
    };
    format!(
        "REFUSED: environment `{env}` is already held. {}\n{who}\n\
         This is a KERNEL lock on an open file description, not a pidfile: it is released when \
         the holder dies by any means, including SIGKILL and a power cut, and it is never \
         released by a file surviving a reboot. The predecessor of this mechanism read a pid \
         out of a file, found the number alive because an unrelated process had been given it \
         on a new boot, and exited 0 — after which the service it was supposed to keep alive \
         went unwatched for sixteen days. Nothing here is decided by reading anything.",
        path.display()
    )
}
