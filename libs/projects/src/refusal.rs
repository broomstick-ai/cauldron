//! **EVERY REFUSAL SENTENCE IN THE SYSTEM, IN ONE PLACE.**
//!
//! `0071` §8 puts "the refusal sentences" in this crate's owns list, and the
//! reason is not tidiness. These sentences are SPOKEN — the AUI reads them out
//! loud and the TUI prints them verbatim in its notice area. If each face wrote
//! its own, the same refusal would be two different sentences depending on
//! which face he happened to be looking at, and the one he heard would not
//! match the one he could quote back.
//!
//! So: a refusal is a value (`Refusal`), it carries a finished sentence, and
//! the sentence is constructed HERE. A caller never formats one itself.
//!
//! House style, taken from the sentences already in `apps/aui`:
//! first person, no jargon, the name or path in backticks, and it says what is
//! true rather than what the code did. "I don't have a project called `x`." —
//! not "lookup failed" and not "ERR_NOT_FOUND".

use std::fmt;
use std::path::Path;

/// A refusal with a sentence a human can be told, as-is.
///
/// Deliberately NOT an enum. A caller that matched on a variant would be a
/// caller deciding what to say, and the whole point is that there is one
/// place that decides. If a caller ever genuinely needs to branch, it gets a
/// new constructor here and a predicate beside it — not a public tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(pub String);

impl Refusal {
    /// The sentence, for speaking or printing.
    pub fn sentence(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

/// The name is not in the registry. The AUI's reply to a mutation naming a
/// project it cannot resolve; the sentence `0071` §8 prescribes verbatim.
pub fn no_such_project(name: &str) -> Refusal {
    Refusal(format!("I don't have a project called `{name}`."))
}

/// Registering a second name onto a checkout that already has one. Two entries
/// on one checkout would draw every thread in it twice.
pub fn path_already_registered(path: &Path, as_name: &str) -> Refusal {
    Refusal(format!(
        "`{}` is already registered as `{as_name}`.",
        path.display()
    ))
}

/// The name is taken. **Never auto-suffixed** — D21's rule is that nothing
/// names things but him, so the refusal hands the naming back.
pub fn name_taken(name: &str) -> Refusal {
    Refusal(format!("`{name}` is taken — name it something else."))
}

/// `miscellaneous` is synthesized, never registered (`0071` §4).
pub fn name_reserved(name: &str) -> Refusal {
    Refusal(format!(
        "`{name}` is reserved — `{}` is the bucket, not a project.",
        crate::BUCKET
    ))
}

/// The bucket cannot be de-registered: it is not in the file to remove.
pub fn cannot_remove_bucket() -> Refusal {
    Refusal(format!(
        "`{}` is the bucket, not a project — it cannot be removed.",
        crate::BUCKET
    ))
}

/// Registering something that is not there.
pub fn nothing_at_path(path: &Path) -> Refusal {
    Refusal(format!("There is nothing at `{}`.", path.display()))
}

/// Registering a file, a socket, or anything else that is not a directory.
pub fn not_a_directory(path: &Path) -> Refusal {
    Refusal(format!(
        "`{}` is not a directory, so it cannot be a project.",
        path.display()
    ))
}

/// A name that cannot be worn by a settings key, a phone node key, or a
/// replica directory. See `crate::name_objection` for what is barred and why.
pub fn unusable_name(name: &str, why: &str) -> Refusal {
    Refusal(format!("`{name}` is not a name I can use: {why}."))
}

/// An empty (or whitespace-only) name.
pub fn empty_name() -> Refusal {
    Refusal("That is not a name I can use.".to_string())
}

/// **THE ANTI-`move_pin` SENTENCE.**
///
/// `0071` §5 and §15: `move_pin` (`apps/aui/src/v2/mutate.rs:711`) resolves a
/// stale `before` with `unwrap_or(next.len())` and silently drops the moved row
/// to the END of the list. The move looks like it worked and the row is not
/// where he put it. The registry REFUSES instead, and the face re-reads. This
/// sentence is the whole difference.
pub fn stale_before(before: &str) -> Refusal {
    Refusal(format!(
        "`{before}` is not in the list any more — the list changed under you; try again."
    ))
}

/// Moving a project to sit before itself. Not an error worth a crash and not
/// a silent no-op either: a face that sent it is computing its neighbour wrong.
pub fn move_before_self(name: &str) -> Refusal {
    Refusal(format!("`{name}` is already where it is."))
}

/// `UnarchiveProject` on a name that is no longer registered (`0071` §3 —
/// replaces today's "its directory is gone from your minds folder").
pub fn not_registered(name: &str) -> Refusal {
    Refusal(format!("`{name}` is not registered."))
}
