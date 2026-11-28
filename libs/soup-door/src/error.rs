//! The one error type: a stable [`Code`] a caller can branch on (and an MCP
//! door can hand back as a refusal code), and the message a person reads.

use std::fmt;

/// What kind of refusal or failure an [`Error`] is. `as_str` is the stable
/// spelling, the form a refusal carries over the MCP door.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Code {
    /// An argument is malformed: a pool that is not a pool name, an unknown
    /// node type, a search with no words, a count that is not a number.
    Usage,
    /// The environment is not named, cannot be resolved, or does not exist.
    NoEnvironment,
    /// The named cauldron does not exist in the environment.
    NoCauldron,
    /// The cauldron exists but has no soup (no `spine.db`).
    NoSoup,
    /// A project marker cannot be used: unreadable, a symbolic link, naming
    /// another environment, or (for a write) tracked by git or another
    /// user's.
    Marker,
    /// A named node or region is not in the pool.
    NotFound,
    /// A soup daemon is running in the environment and may hold the store.
    LiveSoup,
    /// The environment's seam table cannot be read, so no proof can be made.
    SeamTable,
    /// Another writer holds the soup's writer lock.
    Locked,
    /// The soup is not at rest (an interrupted import awaits `recover`).
    NotAtRest,
    /// Reading or writing the store failed.
    Store,
    /// A cauldron in the environment has no grant, or not the scope a call
    /// needs.
    NotGranted,
    /// No grant's project contains the working directory.
    NoCauldronForProject,
    /// Two grants match the working directory equally well.
    AmbiguousProject,
    /// A grant names a cauldron that no longer exists.
    CauldronGone,
    /// The grant store cannot be read or written.
    GrantStore,
    /// An act that only a person at a terminal may perform was run without
    /// one.
    NotATerminal,
    /// A dump is larger than one dump may be.
    TooLarge,
    /// A write names a region another writer holds (`region_lock`).
    RegionLocked,
    /// A change cursor that is not this cauldron's (`changes`).
    CursorUnknown,
    /// A change cursor ahead of the cauldron's (`changes`).
    CursorStale,
    /// A merge its plan blocks (`crate::merge`): each reason is listed with
    /// what to do about it.
    MergeBlocked,
    /// Everything this source holds is already in this target, found by
    /// where each record came from (`crate::merge`).
    AlreadyMerged,
    /// An undo of a merge refused because the target was written since
    /// (`crate::merge::undo`).
    ChangedSince,
    /// A link or unlink of which at least one edge was refused, so none
    /// was written (`edges`).
    LinksRefused,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::Usage => "usage",
            Code::NoEnvironment => "no_environment",
            Code::NoCauldron => "no_cauldron",
            Code::NoSoup => "no_soup",
            Code::Marker => "marker",
            Code::NotFound => "not_found",
            Code::LiveSoup => "live_soup",
            Code::SeamTable => "seam_table",
            Code::Locked => "locked",
            Code::NotAtRest => "not_at_rest",
            Code::Store => "store",
            Code::NotGranted => "not_granted",
            Code::NoCauldronForProject => "no_cauldron_for_project",
            Code::AmbiguousProject => "ambiguous_project",
            Code::CauldronGone => "cauldron_gone",
            Code::GrantStore => "grant_store",
            Code::NotATerminal => "not_a_terminal",
            Code::TooLarge => "too_large",
            Code::RegionLocked => "region_locked",
            Code::CursorUnknown => "cursor_unknown",
            Code::CursorStale => "cursor_stale",
            Code::MergeBlocked => "merge_blocked",
            Code::AlreadyMerged => "already_merged",
            Code::ChangedSince => "changed_since",
            Code::LinksRefused => "links_refused",
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A refusal or a failure, with its whole message (any cause already
/// appended as `: <cause>`, the way `anyhow` prints a chain with `{:#}`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: Code,
    pub message: String,
    /// The refusal's facts for a caller that reads JSON (who holds a
    /// region, until when), when it has any.
    pub detail: Option<serde_json::Value>,
}

impl Error {
    pub fn new(code: Code, message: impl Into<String>) -> Error {
        Error { code, message: message.into(), detail: None }
    }

    /// The same error with `detail`.
    pub fn with_detail(mut self, detail: serde_json::Value) -> Error {
        self.detail = Some(detail);
        self
    }

    /// `context: <cause and its sources>`, as `anyhow`'s `with_context`
    /// followed by `{:#}` prints it.
    pub fn caused(code: Code, context: impl fmt::Display, cause: &dyn std::error::Error) -> Error {
        Error::new(code, format!("{context}: {}", chain(cause)))
    }

    /// An `anyhow` error from a library this crate calls, kept whole.
    pub fn from_anyhow(code: Code, err: anyhow::Error) -> Error {
        Error::new(code, format!("{err:#}"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// An error and every source under it, joined with `: `.
pub fn chain(err: &dyn std::error::Error) -> String {
    let mut s = err.to_string();
    let mut next = err.source();
    while let Some(e) = next {
        s.push_str(": ");
        s.push_str(&e.to_string());
        next = e.source();
    }
    s
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// `return Err(Error::new(code, format!(...)))`.
#[macro_export]
macro_rules! refuse {
    ($code:expr, $($fmt:tt)+) => {
        return Err($crate::Error::new($code, format!($($fmt)+)))
    };
}
