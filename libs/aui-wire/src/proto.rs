//! The wire protocol, derived from the same crate as the object model — there
//! is no second definition to drift. JSON over a WebSocket; audio bytes are
//! base64-encoded inside JSON.
//!
//! ## The promise pattern (proto v2)
//!
//! Every read-side interaction is a *promise*: the client posts a
//! [`ClientMsg::Request`] stating exactly what it wants back (a projection,
//! audio for some labels in a given format, STT for a blob). The server
//! replies IMMEDIATELY with [`ServerMsg::PromiseAck`] and fulfils later with
//! [`ServerMsg::Fulfil`] pushed over the same socket.
//!
//! Promises survive disconnects: the server stores fulfilled results keyed by
//! promise id (on disk where large) with a TTL, and tracks NO per-client
//! delivery state. On reconnect the CLIENT re-asks "promise N done?" via
//! [`ClientMsg::QueryPromise`] for every promise it still wants; the server
//! answers `Fulfil` | `PromisePending` | `PromiseExpired`. Expired/unknown is
//! fine — every request is idempotently re-issuable from the same parameters.
//!
//! Mutations keep their dedicated ack semantics: a [`ServerMsg::MutAck`] is
//! effectively the promise ack + result in one frame.
//!
//! `proto_version` covers ONLY the instruction set (descriptor kinds,
//! LocalCmds, wire messages) — not the menu vocabulary. Menu/state-machine
//! edits ship with zero version churn; an older client hitting an `Unknown`
//! descriptor degrades gracefully.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::node::{CaptureSink, MutationEnvelope, NavAction, NodeKey, NodeProjection, TurnAddr};
use crate::spoken::Spoken;

/// Bumped only when the instruction set grows. v2: promise-based
/// request/response wire messages, per-request audio format negotiation, and
/// the `RefreshBtList`/`ConnectBtDevice` LocalCmds.
pub const PROTO_VERSION: u32 = 2;

/// Default WebSocket port for the ui-server (legacy aui keeps 9001).
pub const DEFAULT_PORT: u16 = 9010;

/// Audio container format for synthesized speech. Every audio request
/// declares the format the requesting client can play; the server
/// synthesizes/transcodes accordingly and caches per format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
    /// RIFF WAVE, 16-bit LE PCM — playable by `aplay` on Linux/Pi.
    Wav,
    /// AIFF — the native macOS `say` output, playable by `afplay`.
    Aiff,
}

impl AudioFormat {
    /// File extension (doubles as the format component of cache filenames).
    pub fn ext(self) -> &'static str {
        match self {
            AudioFormat::Wav => "wav",
            AudioFormat::Aiff => "aiff",
        }
    }

    /// The format this platform's shell-out player can handle natively:
    /// AIFF via `afplay` on macOS, WAV via `aplay` everywhere else.
    pub fn native() -> Self {
        if cfg!(target_os = "macos") {
            AudioFormat::Aiff
        } else {
            AudioFormat::Wav
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "wav" => Some(AudioFormat::Wav),
            "aiff" => Some(AudioFormat::Aiff),
            _ => None,
        }
    }
}

/// Identifies a connecting client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    /// "aui" (audio) | "tui" (terminal) | "ios".
    pub kind: String,
    /// Free-form client build/version string for logging.
    #[serde(default)]
    pub version: String,
    /// Stable per-DEVICE id, chosen once by the client and persisted.
    ///
    /// This is NOT the connection id: a phone that reconnects forty times a day
    /// is still one device, and "has he heard this?" (`Mutation::MarkDelivered`)
    /// is a fact about the device, not about the socket. Empty for clients that
    /// do not care (the desk TUI); the server then treats each connection as its
    /// own anonymous device.
    #[serde(default)]
    pub device_id: String,
}

// ──────────────────────────────────────────────────────────────────────────
//  The tone vocabulary on the wire (D8)
// ──────────────────────────────────────────────────────────────────────────

/// One beep of a tone pattern: `(frequency_hz, duration_ms, amplitude)`.
///
/// A beep with `amp == 0.0` is a SILENT segment and is part of the pattern —
/// `NoInput` and `ThreadPending` are rhythms, and dropping their gaps would
/// turn two knocks into one longer one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ToneBeep {
    pub freq_hz: f32,
    pub ms: u64,
    pub amp: f32,
}

/// One tone: its stable name and the beeps that make it.
///
/// **This is the vocabulary, and the laptop owns it.** The phone synthesizes
/// tones on-device (a 134 ms round trip would make every press feel dead) but it
/// must not FORK the table, or the two carriers drift and a beep stops meaning
/// the same thing on the two devices. So the server ships the table and the
/// client caches the last-seen copy on disk.
///
/// `name` is the `Tone` variant name as the server spells it (`"Tap"`,
/// `"MicOn"`, …). A client that does not know a name still has the pattern and
/// can play it; a client that knows a name it did not receive has no pattern for
/// it and must stay silent rather than invent one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToneSpec {
    pub name: String,
    pub beeps: Vec<ToneBeep>,
}

impl ToneSpec {
    /// Total pattern length in milliseconds — the per-tone speech hold (§5).
    pub fn duration_ms(&self) -> u64 {
        self.beeps.iter().map(|b| b.ms).sum()
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  Deferral signals (D12) — what a push must wait for
// ──────────────────────────────────────────────────────────────────────────

/// **The four things that make now a bad moment to replace the app.**
///
/// The laptop decides WHEN an update lands, but only the phone knows whether he
/// is mid-sentence. So the client reports these four facts and the push policy
/// reads them; a silent push that lands on an open microphone is the one way
/// this feature can cost him a thought.
///
/// Sent on `ClientMsg::ClientState`, unprompted, whenever any field changes and
/// at least every `CLIENT_STATE_MAX_INTERVAL_SECS`. Staleness is the server's
/// problem to judge (see `CLIENT_STATE_STALE_SECS`): a client that has gone
/// quiet is NOT a client that is idle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DeferralSignals {
    /// A capture is open right now — he is talking into it.
    #[serde(default)]
    pub capture_open: bool,
    /// The utterance queue is non-empty — the interface is mid-speech.
    #[serde(default)]
    pub utterances_queued: usize,
    /// A turn is pending — an answer is owed.
    #[serde(default)]
    pub turn_pending: bool,
    /// Milliseconds since the last input of any kind (gesture, tap, button).
    /// The policy's window is `RECENT_INPUT_WINDOW_MS`.
    #[serde(default)]
    pub since_input_ms: u64,
}

/// "Input within 30 seconds" (§10) in milliseconds.
pub const RECENT_INPUT_WINDOW_MS: u64 = 30_000;

/// How often a client re-sends `ClientState` even when nothing changed.
pub const CLIENT_STATE_MAX_INTERVAL_SECS: u64 = 10;

/// A `ClientState` older than this is not evidence of anything. The push policy
/// treats a stale report as "defer" — never as "he is idle, go ahead".
pub const CLIENT_STATE_STALE_SECS: u64 = 45;

impl DeferralSignals {
    /// True when at least one of the four says "not now".
    pub fn defers(&self) -> bool {
        self.capture_open
            || self.utterances_queued > 0
            || self.turn_pending
            || self.since_input_ms < RECENT_INPUT_WINDOW_MS
    }

    /// The reason to defer, for the push log. `None` when nothing defers.
    pub fn defer_reason(&self) -> Option<&'static str> {
        if self.capture_open {
            Some("a capture is open")
        } else if self.utterances_queued > 0 {
            Some("the utterance queue is not empty")
        } else if self.turn_pending {
            Some("a turn is pending")
        } else if self.since_input_ms < RECENT_INPUT_WINDOW_MS {
            Some("he pressed something in the last 30 seconds")
        } else {
            None
        }
    }
}

/// Where the JX-05's five primitives are delivered (§4 rung 2).
///
/// The ring has two possible targets the moment the relay exists, and guessing
/// which one has focus is exactly the ambiguity the routing row removes. One
/// setting, server-held, so both devices agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RingTarget {
    /// The Mac's own interface consumes the ring. The desktop's behaviour today.
    #[default]
    Desk,
    /// The Mac decodes and RELAYS the five primitives to the phone; the desk
    /// interface ignores them.
    Phone,
}

impl RingTarget {
    pub fn label(self) -> &'static str {
        match self {
            RingTarget::Desk => "Ring drives: desk",
            RingTarget::Phone => "Ring drives: phone",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            RingTarget::Desk => RingTarget::Phone,
            RingTarget::Phone => RingTarget::Desk,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "desk" => Some(RingTarget::Desk),
            "phone" => Some(RingTarget::Phone),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            RingTarget::Desk => "desk",
            RingTarget::Phone => "phone",
        }
    }
}

/// What a [`ClientMsg::Request`] asks for. Fully parameterized — fulfilment
/// is idempotently regenerable from these fields alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "want", rename_all = "snake_case")]
pub enum RequestBody {
    /// The projection for one node. `have_version` enables delta sync: if it
    /// matches what the server already served, fulfilment is
    /// `PromiseResult::ProjectionCurrent` instead of the full projection.
    Projection {
        node: NodeKey,
        #[serde(default)]
        have_version: Option<u64>,
    },
    /// Synthesized audio for these texts, in this format.
    Tts { texts: Vec<String>, format: AudioFormat },
    /// Speech-to-text for a WAV blob; when `addr` is set the server also lands
    /// the transcription at `sink` in that thread.
    Stt {
        wav_b64: String,
        #[serde(default)]
        addr: Option<TurnAddr>,
        /// Where the transcript goes (§7). Defaults to a turn, so an older
        /// client's requests behave exactly as they did.
        #[serde(default)]
        sink: CaptureSink,
        /// The client's id for this take, matching the `OpenCapture` mutation
        /// that opened it. Lets the server resolve exactly the placeholder this
        /// capture created — and no other, which is the bug a positional
        /// "replace the last one" search reintroduces every time.
        #[serde(default)]
        capture_id: Option<Uuid>,
    },
}

/// client -> server
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        client: ClientInfo,
        proto_version: u32,
    },
    /// Post a promise: the server acks immediately and fulfils via push.
    Request { id: Uuid, body: RequestBody },
    /// Reconnect re-query: "is promise `id` done?" Answered with `Fulfil`,
    /// `PromisePending`, or `PromiseExpired`.
    QueryPromise { id: Uuid },
    /// Opaque relay; also the outbox drain path. Acked with `MutAck`.
    Mutate { id: Uuid, m: MutationEnvelope },
    /// The four deferral signals (§10). Unacked and lossy on purpose — this is
    /// a report about NOW, and a stale one is worthless, so it is never queued,
    /// never retried, and never outboxed.
    ClientState { signals: DeferralSignals },
}

/// One synthesized clip inside an audio fulfilment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioClip {
    /// Content-addressed cache key (`cache_key(text, voice)`).
    pub key: String,
    pub format: AudioFormat,
    pub b64: String,
}

/// The fulfilment payload of a promise.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromiseResult {
    Projection { p: Box<NodeProjection> },
    /// The client's `have_version` matched — it is already current.
    ProjectionCurrent { node: NodeKey, version: u64 },
    Audio { clips: Vec<AudioClip> },
    Stt { text: String },
    Error { message: String },
}

/// server -> client
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Immediate answer to `Request`: the promise is registered.
    PromiseAck { id: Uuid },
    /// A promise resolved — pushed when ready, or replayed for `QueryPromise`.
    Fulfil { id: Uuid, result: PromiseResult },
    /// `QueryPromise` answer: still being computed.
    PromisePending { id: Uuid },
    /// `QueryPromise` answer: expired or never seen — re-request if wanted.
    PromiseExpired { id: Uuid },
    /// "re-fetch if you care."
    Invalidate { node: NodeKey, version: u64 },
    MutAck { id: Uuid, result: MutResult },
    /// An inbound thread event the client routes (speak-if-focused else unread).
    TurnEvent {
        project: String,
        thread_id: Uuid,
        seq: u64,
        spoken: Spoken,
        #[serde(default)]
        audio_key: Option<String>,
        kind: TurnEventKind,
    },
    AlertsCount { count: usize },
    ServerInfo {
        voice: String,
        version: String,
        proto_version: u32,
        /// Lets the client gate `Confirm { gate: PendingTurns }` without server
        /// round-trips.
        #[serde(default)]
        pending_turn_count: usize,
        /// **The tone vocabulary (D8).** A defaulted field, so this is a
        /// compatible extension: `proto_version` covers only the instruction
        /// set, and an older client that never reads it is unaffected. Empty
        /// means "this server did not ship a table" — the client then keeps
        /// whatever it last cached, and never invents a pattern.
        #[serde(default)]
        tones: Vec<ToneSpec>,
        /// Where the ring's primitives currently go (§4 rung 2). Server-held so
        /// both devices agree on one answer.
        #[serde(default)]
        ring_target: RingTarget,
        /// The build the laptop currently has staged for this client, if any.
        /// The client's update module compares it with its own and takes §10's
        /// path; `None` means nothing is staged.
        #[serde(default)]
        staged_build: Option<String>,
    },
    /// **A ring primitive, relayed (§4 rung 2).** The Mac decoded it from the
    /// JX-05's raw HID reports and forwarded it because `ring_target` is
    /// `Phone`. Identical in shape and meaning to a locally-generated gesture —
    /// the client feeds it into the same `InputSource` path, which is what stops
    /// the relay from becoming a second input vocabulary.
    ///
    /// Sent ONLY to clients whose device is the current ring target. It is not
    /// broadcast: two devices acting on one press is the failure this whole
    /// routing row exists to prevent.
    RingGesture { action: NavAction },
    /// **A build is staged and the client should take §10's path.** Sent when
    /// the laptop finishes staging, so the phone does not have to poll the
    /// manifest to find out.
    UpdateStaged {
        build: String,
        /// True when the laptop can push it with zero taps (LAN + `devicectl`);
        /// false when the away path applies and it costs one tap.
        zero_tap: bool,
    },
    /// **Play Shutdown; the process is about to be replaced (§10).** The last
    /// thing the app hears before the update lands. The client plays the tone,
    /// persists scope and row, and stops — it does NOT exit itself; the laptop
    /// replaces the process.
    PrepareForUpdate { build: String },
}

/// Result of a mutation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "ok", rename_all = "snake_case")]
pub enum MutResult {
    Ok,
    Err { message: String },
}

impl MutResult {
    pub fn err(message: impl Into<String>) -> Self {
        MutResult::Err {
            message: message.into(),
        }
    }
    pub fn is_ok(&self) -> bool {
        matches!(self, MutResult::Ok)
    }
}

/// What a `TurnEvent` represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEventKind {
    /// Assistant speech paragraph (speak it).
    Speech,
    /// Thread summary updated (refresh labels; usually not spoken).
    Summary,
    /// Turn ended.
    TurnEnd,
    /// Turn failed.
    Error,
}
