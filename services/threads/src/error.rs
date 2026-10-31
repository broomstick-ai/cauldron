//! Local error type.
//!
//! `threads` deliberately does not use `substrate_types::SubstrateError` — the
//! system-of-record crate must not inherit branch drift from the rest of the
//! workspace (CCW's coupling to `substrate-types` is one reason it only exists
//! on the V1 branch).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// Bad configuration, bad request params, unknown ids.
    #[error("{0}")]
    Config(String),
    /// SQLite / persistence failures.
    #[error("store: {0}")]
    Store(String),
    /// Internal invariants (poisoned locks, join errors).
    #[error("internal: {0}")]
    Internal(String),
    /// I/O around spawn / turn-output files / imports.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// **A TYPED REFUSAL** — one of `shapewire::envelope::codes`, chosen at the
    /// site that knows which. The one variant the [`shapewire::envelope::Coded`]
    /// impl below types; everything else stays `error`, per that impl's own
    /// argument. Added for the `agents/*` façade (R4.S1), whose refusals
    /// (`not_available_yet`, `unknown_param`, `unknown`, `malformed`) an agent
    /// has to branch on and must not have to parse English for.
    #[error("{message}")]
    Refused {
        code: &'static str,
        message: String,
    },
}

/// **THE UNTYPED CODE, DECLARED RATHER THAN DEFAULTED INTO.**
///
/// R1 put a `code` beside every refusal on the wire. The chassis types its own
/// refusals by construction — an unknown shape, a dead fold, an absurd `at`, an
/// unknown method, an unauthenticated frame — and a producer's errors are typed
/// when the producer implements this trait. This impl takes the default,
/// `codes::ERROR`, and that is a statement rather than an omission:
///
/// **`Error::Config` is three different codes wearing one variant.** Its
/// docstring says so — "bad configuration, bad request params, unknown ids" —
/// which under the closed set is `malformed` for the middle one and `unknown`
/// for the last. Choosing either wholesale would put a code on the wire that is
/// wrong half the time, and a consumer branching on a code that lies is worse
/// off than one that knows it has only prose. Deciding it needs splitting
/// `Config`, per call site, which is its own change.
///
/// The remaining variants (`Store`, `Internal`, `Io`, `Json`) are producer-side
/// defects a consumer cannot repair. The closed set has no word for that —
/// `integrity` means a digest mismatch specifically — so inventing one here
/// would fork the vocabulary the chassis owns.
///
/// **`Error::Refused` is the one exception, since R4.S1**: it carries the code
/// its site chose, and nothing is defaulted into. The `agents/*` arms are the
/// first users.
impl shapewire::envelope::Coded for Error {
    fn code(&self) -> &'static str {
        match self {
            Error::Refused { code, .. } => code,
            _ => shapewire::envelope::codes::ERROR,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Store(e.to_string())
    }
}
