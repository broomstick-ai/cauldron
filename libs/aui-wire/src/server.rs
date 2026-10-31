//! The typed exhaustive `Mutation` vocabulary and the one project-name
//! normalizer, moved from harness `libs/aui-state/src/server/mod.rs` lines
//! 1-406 (see `Cargo.toml` provenance). The rest of that module (the read-side
//! `StoreView`, the views, the kind registry) is the ui-server's and did not
//! move: a face mints mutations, it never applies them.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::Model;
use crate::node::{EventAnchor, MutationEnvelope};
use crate::proto::RingTarget;

/// Special project name for general-purpose tracked threads not tied to a
/// project. Mirrors `aui_nav::state::MISCELLANEOUS_PROJECT` — the two must
/// stay identical. (History: the key was misspelled "_mythcellaneous" until
/// 2026-09-14; ui-server migrates the on-disk directory at startup.)
pub const MISC_PROJECT: &str = "_miscellaneous";

// ──────────────────────────────────────────────────────────────────────────
//  Typed, exhaustive mutation vocabulary (server-only)
// ──────────────────────────────────────────────────────────────────────────

/// Envelopes are deserialized into this at the server boundary; an unknown kind
/// is a nack. The client never sees this type.
///
/// ## Which of these a client mints BY HAND
///
/// Most mutations are baked into an `ActionDescriptor::Mutate` at projection
/// build time and relayed verbatim — the client never constructs them. Five
/// cannot work that way, because their payload only exists at the moment the
/// operator acts, and the client is the only one holding it:
///
/// - [`Mutation::SubmitTextTurn`] — he typed the words (§3, Compose).
/// - [`Mutation::MarkDelivered`] — this device just played it (§1 D3).
/// - [`Mutation::OpenCapture`] — this device just opened its microphone (§7).
/// - [`Mutation::AdminRestartServer`] / [`Mutation::AdminShutdownServer`] —
///   already hand-built by the existing client.
///
/// The iOS feature set (D19–D25) adds nine more to that set, for the same
/// reason in every case — the payload does not exist until his finger or his
/// voice produces it:
///
/// - [`Mutation::MovePin`] — a drag's destination does not exist until he
///   drops it (D19a). Still ANCHOR-based, never an index, so a list that
///   shifted under the drag cannot misfile it.
/// - [`Mutation::CreateProject`] — he just said or typed the name (D21).
/// - [`Mutation::ArchiveProject`] / [`Mutation::UnarchiveProject`] — a
///   long-press context menu is a client-side gesture (D22d).
/// - The five Human Actions writes (D23): [`Mutation::HumanActionDraft`],
///   [`Mutation::HumanActionSubmit`], [`Mutation::HumanActionMarkPhase`],
///   [`Mutation::HumanActionSetStatus`], [`Mutation::HumanActionPush`].
///
/// All of them are therefore kept MINIMAL and every optional field defaulted,
/// so a hand-assembled JSON object parses. `client_minted_envelopes_round_trip`
/// in the conformance test is what holds that promise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mutation {
    SubmitTurn {
        project: String,
        thread_id: Uuid,
        text: String,
        #[serde(default)]
        anchor: Option<EventAnchor>,
        #[serde(default)]
        model: Option<Model>,
        #[serde(default)]
        retry_failed: bool,
    },
    /// **A turn he TYPED (§3).** Client-minted: the text does not exist until he
    /// finishes typing, so this cannot be a descriptor baked at build time.
    ///
    /// It is a separate kind from `SubmitTurn` for exactly one reason — the
    /// minimal hand-buildable shape — and NOT a second implementation: the
    /// handler delegates to the same submit path, so numbering, thread binding
    /// and account placement are the daemon's, identically to a spoken turn.
    SubmitTextTurn {
        project: String,
        thread_id: Uuid,
        text: String,
    },
    /// **This device played it, so it is heard (§1 D3).** The one piece of state
    /// the desk and the phone must share: a thread he finished on the phone is
    /// not unread — and not read back to him — at the desk.
    ///
    /// `event_idx` is the thread log's own stable 0-based line position. It is
    /// NOT a `msg=#N`, and no arithmetic converts between the two spaces; the
    /// client copies the idx it was served and sends it back unchanged.
    MarkDelivered {
        project: String,
        thread_id: Uuid,
        /// Everything at or before this idx has been heard on `device_id`.
        event_idx: u64,
        /// Which device heard it. Empty = the connection's own device.
        #[serde(default)]
        device_id: String,
    },
    /// **The microphone just opened (§7).** Writes the `UnprocessedInput`
    /// placeholder AT MIC OPEN, not at stop — so a crash, a force-quit, or an
    /// update push landing mid-utterance leaves the moment attributable and the
    /// capture recoverable. This is the desktop's own crash posture, moved to
    /// the device that now owns the microphone.
    ///
    /// `capture_id` is the client's own id for the take; it rides onto the
    /// placeholder so the later STT fulfilment resolves exactly the placeholder
    /// it created and no other.
    OpenCapture {
        project: String,
        thread_id: Uuid,
        capture_id: Uuid,
    },
    SetThreadModel {
        project: String,
        thread_id: Uuid,
        model: Option<Model>,
    },
    SetProjectModel {
        project: String,
        model: Option<Model>,
    },
    MarkFailed {
        project: String,
        thread_id: Uuid,
        text: String,
        error: String,
        #[serde(default)]
        error_kind: String,
    },
    RemoveProject {
        project: String,
    },
    /// **A new project is a new DIRECTORY under `~/minds/` (D21).**
    ///
    /// `~/minds/` is the discovery root — a directory that exists is a project
    /// that exists — so his whole sentence is the architecture. The server
    /// normalizes the name through [`normalize_project_name`] (the ONE
    /// normalizer), refuses a collision rather than repairing it, and creates a
    /// real directory, not a symlink: a phone-born project has no `~/code`
    /// checkout to point at, so `~/minds/<name>` IS its root.
    ///
    /// It does NOT create `.mind/` (D21f). Registration is by existence, and a
    /// half-shell minted from a phone is a downstream corrector the real
    /// initializer would then have to reconcile.
    CreateProject {
        name: String,
    },
    /// **"I don't want to delete it, but I don't want to see it all the
    /// time." (D22)**
    ///
    /// A statement about ATTENTION, not about the filesystem — so it touches
    /// neither his directories nor his repos. The archived set lives in the
    /// interface's own state directory, and "never deletes" is true by
    /// construction because archive never touches the project at all.
    ArchiveProject {
        name: String,
    },
    /// The inverse, one Select away inside the Archived menu (D22f). Bringing a
    /// project back is its own deliberate act, which is exactly why a name
    /// collision on [`Mutation::CreateProject`] never auto-unarchives.
    UnarchiveProject {
        name: String,
    },
    // ── Human Actions (D23) — five thin doors onto the engine's own verbs ────
    //
    // The Rust server NEVER parses a ticket. The engine (`~/code/human-actions`)
    // stays the one implementation and grows one machine face; each mutation
    // below maps to exactly one NDJSON verb, so `set_frontmatter_status` stays
    // the one status writer, the non-ticket guard still refuses node artifacts,
    // and a status the phone writes clears `status_by` exactly as the TUI's
    // writer does — because the phone is his hand, the same as the TUI is.
    /// Typing never writes. Every keystroke lands in a client-side draft
    /// immediately and a coalesced sync lands it in the ENGINE's draft store,
    /// so a value half-typed on the phone shows as `● draft` in his TUI and one
    /// abandoned in the TUI shows on the phone. One draft store, two faces.
    HumanActionDraft {
        project: String,
        action: String,
        field: String,
        value: String,
    },
    /// Route every drafted value through the git-ignore-gated env upsert. The
    /// engine's own refusals (no write target) surface in the engine's words.
    HumanActionSubmit {
        project: String,
        action: String,
    },
    /// `gh secret set` — a SECOND, separate confirmation, never folded into
    /// submit: pushing a secret to a remote is a different act from writing it
    /// to a local env file.
    HumanActionPush {
        project: String,
        action: String,
    },
    /// Mark one instructional phase done — the engine writes the same
    /// `section:<id>:<slug>` key the TUI writes, and auto-resolve fires by the
    /// same rule when the last phase closes. `phase` empty means ALL phases.
    HumanActionMarkPhase {
        project: String,
        action: String,
        #[serde(default)]
        phase: String,
    },
    /// Retire / un-retire / adopt. From the phone this is HIS hand, so no
    /// `status_by` is written and his TUI receives it as his own.
    HumanActionSetStatus {
        project: String,
        action: String,
        status: String,
    },
    // ── thread options (ported from the live ThreadOptionItem row set) ──────
    SetThreadPinned {
        project: String,
        thread_id: Uuid,
        pinned: bool,
    },
    SetThreadClosed {
        project: String,
        thread_id: Uuid,
        closed: bool,
    },
    /// Manually-set summary — LOCKED against the agent's auto-drifting updates,
    /// exactly as `ThreadHeader::set_summary_manual` does on the desktop.
    SetThreadSummary {
        project: String,
        thread_id: Uuid,
        summary: String,
    },
    /// Move `thread_id` in the pin order so it sits immediately before
    /// `before`; `None` places it last.
    MovePin {
        project: String,
        thread_id: Uuid,
        #[serde(default)]
        before: Option<Uuid>,
    },
    InterruptThread {
        project: String,
        thread_id: Uuid,
    },
    CompactThread {
        project: String,
        thread_id: Uuid,
        #[serde(default)]
        instructions: Option<String>,
    },
    // ── the pending-input sidecar (PendingInputs / PendingMessage) ──────────
    /// Send the queue now. `only` picks one entry **by its stable id** (the
    /// per-message submenu's "Send message N now."); `None` sends every
    /// eligible entry.
    ///
    /// An id and not an ordinal: the phone holds a rendered list that is
    /// already a moment old when he taps it, and a desk-side send or delete
    /// renumbers the queue underneath it. An ordinal from a stale list names
    /// the wrong message; an id names his message or nothing.
    SendPending {
        project: String,
        thread_id: Uuid,
        #[serde(default)]
        only: Option<Uuid>,
    },
    /// Arm the queue to flush when the agent finishes ("Send when done.").
    ArmPending {
        project: String,
        thread_id: Uuid,
    },
    /// Delete one queued message **by its stable id**, or all of them when
    /// `only` is `None`. See [`Mutation::SendPending`] for why it is an id.
    DeletePending {
        project: String,
        thread_id: Uuid,
        #[serde(default)]
        only: Option<Uuid>,
    },
    // ── TWO MUTATIONS ARE GONE: `RetryPlacement`, `UnpendThread` ────────────
    //
    // They were the phone's half of the `thread_pending_menu` node and the only
    // things that sent `thread.replace` and `thread.unpend` from this side. The
    // daemon answers `unknown method` to both names (probed on `:3651`,
    // 2026-10-02) and the node that offered them was gated on a status word no
    // producer has ever sent, so neither mutation has ever been delivered.
    //
    // **The wire can drop them.** `Mutation` is `#[serde(tag = …)]`-tagged and
    // the phone only ever sends a mutation the SERVER just handed it inside a
    // node projection, so there is no stored client that could still name
    // `retry_placement` or `unpend_thread` — and a phone that somehow did would
    // get the ordinary unknown-kind refusal rather than a silent success.
    // ── the failed-input sub-menu ───────────────────────────────────────────
    RetranscribeFailedInput {
        project: String,
        thread_id: Uuid,
        event_idx: usize,
    },
    CancelAutoRetry {
        project: String,
        thread_id: Uuid,
    },
    /// **Where the ring's five primitives go (§4 rung 2).** Server-held so the
    /// desk and the phone can never both act on one press.
    SetRingTarget {
        target: RingTarget,
    },
    AdminRestartServer,
    AdminShutdownServer,
}

impl Mutation {
    /// The envelope kind string for this mutation.
    pub fn kind(&self) -> &'static str {
        match self {
            Mutation::SubmitTurn { .. } => "submit_turn",
            Mutation::SubmitTextTurn { .. } => "submit_text_turn",
            Mutation::MarkDelivered { .. } => "mark_delivered",
            Mutation::OpenCapture { .. } => "open_capture",
            Mutation::SetThreadModel { .. } => "set_thread_model",
            Mutation::SetProjectModel { .. } => "set_project_model",
            Mutation::MarkFailed { .. } => "mark_failed",
            Mutation::RemoveProject { .. } => "remove_project",
            Mutation::CreateProject { .. } => "create_project",
            Mutation::ArchiveProject { .. } => "archive_project",
            Mutation::UnarchiveProject { .. } => "unarchive_project",
            Mutation::HumanActionDraft { .. } => "human_action_draft",
            Mutation::HumanActionSubmit { .. } => "human_action_submit",
            Mutation::HumanActionPush { .. } => "human_action_push",
            Mutation::HumanActionMarkPhase { .. } => "human_action_mark_phase",
            Mutation::HumanActionSetStatus { .. } => "human_action_set_status",
            Mutation::SetThreadPinned { .. } => "set_thread_pinned",
            Mutation::SetThreadClosed { .. } => "set_thread_closed",
            Mutation::SetThreadSummary { .. } => "set_thread_summary",
            Mutation::MovePin { .. } => "move_pin",
            Mutation::InterruptThread { .. } => "interrupt_thread",
            Mutation::CompactThread { .. } => "compact_thread",
            Mutation::SendPending { .. } => "send_pending",
            Mutation::ArmPending { .. } => "arm_pending",
            Mutation::DeletePending { .. } => "delete_pending",
            Mutation::RetranscribeFailedInput { .. } => "retranscribe_failed_input",
            Mutation::CancelAutoRetry { .. } => "cancel_auto_retry",
            Mutation::SetRingTarget { .. } => "set_ring_target",
            Mutation::AdminRestartServer => "admin_restart_server",
            Mutation::AdminShutdownServer => "admin_shutdown_server",
        }
    }

    /// Build the opaque wire envelope. The payload is the tagged enum value, so
    /// it always round-trips back through `from_envelope`.
    pub fn into_envelope(self) -> MutationEnvelope {
        let kind = self.kind().to_string();
        let payload = serde_json::to_value(&self).unwrap_or(serde_json::Value::Null);
        MutationEnvelope { kind, payload }
    }

    /// Parse an opaque envelope into the typed enum at the server boundary.
    pub fn from_envelope(env: &MutationEnvelope) -> anyhow::Result<Mutation> {
        serde_json::from_value(env.payload.clone())
            .map_err(|e| anyhow::anyhow!("unknown/invalid mutation '{}': {e}", env.kind))
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  Project-name normalization (D21a) — ONE definition site
// ──────────────────────────────────────────────────────────────────────────

/// **Turn what he said into the name a directory can wear.**
///
/// Trim, lowercase, keep alphanumerics, collapse every other run to a single
/// hyphen — the same shape every project name in his root menu already wears.
///
/// There is exactly one of these and every caller goes through it: the STT
/// path that hands the slug to the confirm node, the confirm node that reads it
/// back to him, and the handler that makes the directory. If the normalizer
/// lived in two places, the name he heard confirmed and the name on disk could
/// differ — which is precisely the difference D21d exists to let him hear
/// BEFORE it exists.
///
/// An empty result is a refusal, not an empty directory name: the caller nacks.
pub fn normalize_project_name(raw: &str) -> String {
    let mut out = String::new();
    let mut pending_sep = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('-');
            }
            pending_sep = false;
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_alphanumeric() {
            // Non-ASCII letters and digits are kept as themselves, lowercased.
            // Dropping them would silently rename a project he named in his own
            // words; a directory name is allowed to hold them.
            if pending_sep && !out.is_empty() {
                out.push('-');
            }
            pending_sep = false;
            out.extend(ch.to_lowercase());
        } else {
            pending_sep = true;
        }
    }
    out
}
