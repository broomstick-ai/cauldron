//! **HAS HE RUN THIS BEFORE — answered from state that already exists, and
//! from no marker of its own.**
//!
//! `0040` §10 asks for one extra band on the very first launch: a single hint
//! line that teaches the grammar "and never appears again". `0053` declined to
//! build it and was right to at the time — it read the ask as needing durable
//! "has he run this before" state, which `0023`'s `what_the_tui_stores` refuses
//! ("any byte on disk beyond the log").
//!
//! **That refusal is intact, and this does not breach it.** Read it again with
//! its own rule attached: *"if a datum can be asked of the daemon in one RPC,
//! holding it beyond cache lifetime is a defect."* It is a rule about
//! DAEMON-OWNED truth — the queue, the unread cursor, thread state — and it
//! names the log as the one thing that legitimately lands on disk. Whether
//! this face has ever been started on this machine is not a fact the daemon
//! has, in one RPC or in any number of them: the daemon serves many clients
//! and has never heard of this one's terminal. So there is nothing here to
//! ask, and nothing being cached.
//!
//! The composer draft journal (`~/.tui/draft`, `crate::draft`) and the mic
//! captures (`~/.tui/captures`, `crate::mic`) are the same shape and were
//! settled the same way: client-owned state with no daemon to hold it.
//!
//! **And this writes nothing regardless.** The detector is a READ of a
//! directory that another part of the program creates for its own reasons:
//! `main::init_logging` calls `create_dir_all("~/.tui")` on every start,
//! before the face is drawn, because the log has to go somewhere. So the
//! directory's existence already records "this program has run here", and it
//! records it as a by-product rather than as a flag — which is the detector
//! the task asked for, in its own words, because it **cannot drift, cannot be
//! stale, and needs no migration**. There is no file here whose only purpose
//! is to remember him. Delete `~/.tui` and the face teaches again, which is
//! honest: it has genuinely forgotten.
//!
//! **The ordering is load-bearing and it is the whole mechanism.** The probe
//! must run BEFORE the log opens, or the log will have created the very
//! directory being asked about and every run would look like the first —
//! inverted, which is worse than either answer, since a face that teaches
//! forever is a face that never learned anything. `main` runs it first and
//! hands the answer down as a value; `tests::the_log_hides_the_answer_once_it_has_opened`
//! is the assertion that the order matters.

use std::path::Path;

/// The directory the face keeps its own things in — the log, the draft
/// journal, the mic captures. Named once.
pub const STATE_DIR: &str = ".tui";

/// **Has this face ever been started for this home directory?**
///
/// `home` is the resolved home; `None` (no home at all) answers `false`,
/// because a machine with nowhere to keep a log is not a machine we can
/// honestly say anything about, and a wrong "welcome" is louder than a missing
/// one.
///
/// Call it BEFORE the log is opened. See the module docs for why that is the
/// mechanism and not a detail.
pub fn is_first_run(home: Option<&Path>) -> bool {
    match home {
        Some(h) => !h.join(STATE_DIR).exists(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch home, deleted with the guard.
    struct Home(std::path::PathBuf);
    impl Home {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "tui-first-run-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_home_with_no_state_dir_is_a_first_run() {
        let h = Home::new("fresh");
        assert!(is_first_run(Some(&h.0)));
    }

    /// **THE SECOND RUN, AND THE REASON THE FIRST ONE CANNOT REPEAT.** The
    /// directory is not written by this module — it is written by the log, on
    /// every start — so one launch is all it takes for the answer to flip and
    /// stay flipped.
    #[test]
    fn the_log_hides_the_answer_once_it_has_opened() {
        let h = Home::new("second");
        assert!(is_first_run(Some(&h.0)), "the fixture was not fresh");
        // Exactly what `main::init_logging` does, and nothing more.
        std::fs::create_dir_all(h.0.join(STATE_DIR)).unwrap();
        assert!(
            !is_first_run(Some(&h.0)),
            "the hint would be taught on every launch forever"
        );
    }

    #[test]
    fn no_home_is_not_a_welcome() {
        assert!(!is_first_run(None));
    }
}
