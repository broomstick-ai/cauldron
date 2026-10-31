//! The websocket wire: frame for frame the envelope `threads` speaks, so a
//! face's client for one ports to the other.
//!
//! Boot-id welcome, optional shared-secret auth, request/response correlation
//! with a success/error/promise outcome, subscribe with a key filter, a ping
//! every 30 s. Two supersets of the `threads` envelope:
//!
//! 1. `Outcome::Error` may carry `details` (see [`crate::error`]). A client
//!    reading only `error` is unaffected.
//! 2. There is no `Event` frame and no event ledger. A speech event is worth
//!    nothing ten seconds later, and a ledger at hundreds of warms a minute
//!    would grow without bound — the disk problem this service exists to
//!    prevent. Every push is transient; durable observability is the per-key
//!    counters and `tts.status`.
//!
//! THIS FILE IS THE WIRE. Moving the crate into cauldron did not change one
//! field of it, and the protocol tests that came with it are what say so.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bumped only on a breaking change to the envelope.
pub const PROTO: u16 = 1;

/// Client to daemon.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Auth {
        secret: String,
    },
    Request {
        id: String,
        method: String,
        #[serde(default)]
        params: Value,
    },
    /// `keys: None` subscribes to every key.
    Subscribe {
        #[serde(default)]
        keys: Option<Vec<String>>,
    },
    Unsubscribe {
        #[serde(default)]
        keys: Option<Vec<String>>,
    },
}

/// Daemon to client.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Welcome {
        proto: u16,
        daemon: &'static str,
        daemon_version: &'static str,
        boot_id: String,
        auth_required: bool,
    },
    AuthAck {
        ok: bool,
    },
    Response {
        correlate: String,
        outcome: Outcome,
    },
    /// Transient. Never persisted.
    Push {
        #[serde(flatten)]
        push: Push,
    },
    SubAck {
        firehose: bool,
        keys: Vec<String>,
    },
}

/// Kept verbatim from `threads` so client code ports mechanically. This
/// daemon never emits `Promise`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    Success {
        payload: Value,
    },
    Error {
        error: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<&'static str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
    },
    Promise {
        promise: String,
    },
}

impl Outcome {
    pub fn ok(payload: Value) -> Self {
        Outcome::Success { payload }
    }

    pub fn from_err(e: &crate::error::Error) -> Self {
        Outcome::Error { error: e.to_string(), code: e.code(), details: e.details() }
    }
}

/// The pushes. Filtered by `key` for a client that subscribed narrowly;
/// `module_health` has no key and reaches everyone.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Push {
    /// What a waiting client resolves on.
    Warmed { key: String, hash: String, bytes: u64, synth_ms: u64, module: String },
    /// An honest failure, never silence.
    SynthesisFailed { key: String, hash: String, code: String, reason: String, module: String },
    /// So a client can drop a path it is holding. `reason`: `lru`,
    /// `lru_count`, `forget`, `global`, `queue_pressure`.
    Evicted { key: String, hash: String, bytes: u64, reason: String },
    /// `kind`: `warn`, `refuse`, `global`.
    CachePressure { key: String, bytes: u64, limit: u64, kind: String },
    /// Lets a client back off registering.
    QueueDepth { key: String, depth: u64, in_flight: u64 },
    /// The circuit breaker. `state`: `ok` or `degraded`.
    ModuleHealth {
        module: String,
        state: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl Push {
    /// The key this push belongs to; `None` reaches every subscriber.
    pub fn key(&self) -> Option<&str> {
        match self {
            Push::Warmed { key, .. }
            | Push::SynthesisFailed { key, .. }
            | Push::Evicted { key, .. }
            | Push::CachePressure { key, .. }
            | Push::QueueDepth { key, .. } => Some(key),
            Push::ModuleHealth { .. } => None,
        }
    }
}
