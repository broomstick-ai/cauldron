//! # tts — the text-to-speech cache and synthesis daemon
//!
//! > "we should externalize the service for text-to-speech into a text-to-speech
//! > dedicated service like threads… It needs to have a cache key and the cache
//! > key corresponds to some settings." — the operator
//!
//! Rewritten into cauldron from substrate `lib/tts` (trunk 5a17b66). The wire
//! ([`protocol`]), the store schema and the hash are unchanged; what changed is
//! what kind of process this is. It was a binary cauldron HOSTED — forked,
//! watched, written a seam for. It is now a cauldron SERVICE: it reads its
//! environment through `env::seams::ServiceEnv`, registers its own seam, and
//! serves the warnings shape the contract requires.
//!
//! ## Module map
//!
//! - [`protocol`] — the websocket envelope and pushes. Unchanged.
//! - [`error`] — the error type and the pinned wire error codes.
//! - [`config`] — where state lives (handed in by cauldron) and the WS address.
//! - [`hash`] — the render identity, byte-identical to the face's cache key.
//! - [`priority`] — the priority scale, as numbers.
//! - [`keys`] — the cache-key model (`store/tts.toml`).
//! - [`store`] — SQLite: keys, renders, memberships, locks, counters.
//! - [`queue`] — one consumer over every key, priority first.
//! - [`synth`] — the backend seam: `system` (macOS `say`) and `kokoro` (refuses).
//! - [`manager`] — single-flight, the gate, the scheduler, eviction, the breaker.
//! - [`server`] — the axum websocket and `/health`.
//! - [`import`] — `tts.import`: adopt a cache directory by hash, by hardlink.
//! - [`seam`] — the cauldron seam and the warnings shape the contract requires.
//! - [`daemon`] — bind, open, sweep, serve, register, drain on stop.
pub mod config;
pub mod daemon;
pub mod error;
pub mod hash;
pub mod import;
pub mod keys;
pub mod manager;
pub mod priority;
pub mod protocol;
pub mod queue;
pub mod seam;
pub mod server;
pub mod store;
pub mod synth;

pub use config::Config;
pub use error::{Error, Result, WS_ERROR_CODES};
pub use keys::{KeysFile, Mode, Queueing};
pub use daemon::build_manager;
pub use manager::TtsManager;
pub use store::Store;
