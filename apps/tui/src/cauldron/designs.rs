//! **The open questions of the three msg-165 designs, each in ONE place.**
//!
//! Designs 0138 (the dump), 0146 (the Artifacts tab) and 0142 (running,
//! parked and pending) each left questions with the operator. Each is built
//! here at its RECOMMENDED option, and each is one constant, so his answer
//! is a one-line change: every place that behaves differently by the answer
//! reads the constant, and nothing else decides it.

// ── 0138, the dump ─────────────────────────────────────────────────────────

/// 0138 Q2: Esc inside the dump hands the selection back to the page (Esc a
/// two-way switch between page and dump). Recommended: yes; ← at the start
/// and ↑ on the top line keep working.
pub const ESC_LEAVES_DUMP: bool = true;

/// 0138 Q3: a per-note send — ^S sends the note the caret is in as its own
/// dump and takes it out of the text. Recommended: yes.
pub const SEND_ONE_NOTE: bool = true;

// ── 0146, the Artifacts tab ────────────────────────────────────────────────

/// 0146 Q1: where the ARTIFACT node lives. ANSWERED (8e4fca10 msg 186,
/// his Q11): "We actually want artifacts to be considered like they are a
/// soup item. They are a node that wraps around like a file." So an artifact
/// is a SOUP node — it sits in regions like any node, and the Soup tab lists
/// it in its regions and in its ARTIFACTS band. The word is what its detail
/// rows say.
pub const ARTIFACT_HOME: &str = "soup";

/// 0146 Q2: the face SHOWS worktrees and never acts on one (no `remove ▸` on
/// merged or missing rows). Recommended: show only, for this build.
pub const WORKTREE_ACTIONS: bool = false;

/// 0146 Q3: what is open when the Artifacts tab is first drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactsOpen {
    /// A band is open iff something ACTIVE is inside it at any depth (a
    /// worktree a run holds, an artifact being written). Recommended.
    ActivePath,
    /// REPOS open down to the worktree status bands; the rest closed.
    ReposOpen,
    /// Everything closed.
    AllClosed,
}

pub const ARTIFACTS_OPEN: ArtifactsOpen = ArtifactsOpen::ActivePath;

// ── 0142, running · parked · pending ───────────────────────────────────────

/// 0142 Q1: PARKED counts only runs the admission gate refused. Work blocked
/// in the graph (■ Q-2, ← T-3) is none of the three words: it stays on the
/// engine with its own glyph. Recommended: gate-refused only. (False is the
/// old DEC-7 reading: parked = refused + blocked + waiting.)
pub const PARKED_IS_GATE_REFUSED_ONLY: bool = true;

/// 0142 Q2: a parked row carries the token sources it could run on, as a
/// label (`Claude Code · Codex`) — never as a per-source number.
/// Recommended: carry them.
pub const PARKED_ROW_CARRIES_ELIGIBLE: bool = true;

// ── msg 235 ────────────────────────────────────────────────────────────────

/// DEV ONLY — THIS MUST NOT MAKE IT INTO THE PRODUCTION VERSION (8e4fca10
/// msg 235: "it should say in the code, this should not make it into the
/// production version. This is just for the developer"). A double click on
/// a title — the sidebar's, a box's legend, a tab — renames it on screen
/// (`store::DevNames`, `app::Modal::DevRename`). Set it false, then remove
/// all three, before the face ships.
pub const DEV_RENAME: bool = true;
