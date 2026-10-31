//! The crate's error type, and the wire error codes it is allowed to emit.
//!
//! Every error that reaches a client is `"<code>: <detail>"`. The CODE is the
//! contract — it is drawn from [`WS_ERROR_CODES`] and a client may match on
//! it; the detail after the colon is prose and may change at any time. That is
//! the one place this wire is stricter than `threads`', whose clients match
//! error TEXT that nothing pins. `tests/ws_protocol.rs` reads this crate's
//! source and fails if any `Error::wire(..)` names a code not listed here.

use thiserror::Error;

/// Every code this daemon may put on the wire, sorted. Adding one is an edit
/// here plus the data contract, deliberately.
pub const WS_ERROR_CODES: &[&str] = &[
    "backend_unavailable",
    "bad_params",
    // Withdrawn by the client. Never recorded durably; the next request for
    // the same text synthesizes from cold.
    "cancelled",
    "disk_full",
    "hash_mismatch",
    "key_full",
    // A module that is declared and has no backend. Permanent: distinct from
    // `backend_unavailable` (a real backend, degraded — retry) and from
    // `synthesis_failed` (a backend ran and could not speak this text).
    "not_implemented",
    "not_ready",
    "synthesis_failed",
    "too_large",
    "unknown_hash",
    "unknown_key",
    "unknown_method",
];

#[derive(Debug, Error)]
pub enum Error {
    /// A coded error for the wire. `details` travels in its own field of the
    /// response envelope and is never parsed out of the text: `not_ready` uses
    /// it for `{hash, state, queue_position}` so a client can tell "no" from
    /// "not yet" without string matching.
    #[error("{code}: {detail}")]
    Wire {
        code: &'static str,
        detail: String,
        details: Option<serde_json::Value>,
    },
    /// A backend child was killed by a signal this service did not send.
    ///
    /// Not a wire code on purpose: it is a fact about the MACHINE, not about
    /// the text. Recording it as `synthesis_failed` would mark an innocent
    /// sentence and count against the circuit breaker. The cancel and timeout
    /// paths return before a child's status is read, so anything arriving here
    /// was killed from outside; the manager re-queues it once and otherwise
    /// releases it cold.
    #[error("interrupted: backend killed by signal {signal}")]
    Interrupted { signal: i32 },
    /// Bad configuration. A boot or load failure, never a wire code.
    #[error("config: {0}")]
    Config(String),
    #[error("store: {0}")]
    Store(String),
    /// A broken internal invariant: a poisoned lock, a lost join.
    #[error("internal: {0}")]
    Internal(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// A coded wire error. `code` must be in [`WS_ERROR_CODES`].
    pub fn wire(code: &'static str, detail: impl Into<String>) -> Self {
        Error::Wire { code, detail: detail.into(), details: None }
    }

    /// A coded wire error with structured detail beside it.
    pub fn wire_with(code: &'static str, detail: impl Into<String>, details: serde_json::Value) -> Self {
        Error::Wire { code, detail: detail.into(), details: Some(details) }
    }

    pub fn code(&self) -> Option<&'static str> {
        match self {
            Error::Wire { code, .. } => Some(code),
            _ => None,
        }
    }

    pub fn details(&self) -> Option<serde_json::Value> {
        match self {
            Error::Wire { details, .. } => details.clone(),
            _ => None,
        }
    }

    pub fn bad_params(detail: impl Into<String>) -> Self {
        Error::wire("bad_params", detail)
    }

    pub fn unknown_key(key: &str) -> Self {
        Error::wire("unknown_key", format!("no such cache key: {key}"))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Store(e.to_string())
    }
}
