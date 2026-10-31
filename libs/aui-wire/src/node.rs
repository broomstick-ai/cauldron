//! Core object model shared by every binary: the open node identity, the
//! projected menu graph, and the client instruction set.
//!
//! The client is an *interpreter*. It compiles in only the small versioned
//! instruction set below (`ActionDescriptor` + `LocalCmd`); the entire state
//! machine — node kinds, structure, labels, edges, parameterized actions — is
//! server-served data. New scope kinds and new server actions are server-only
//! changes; the client never matches on `NodeKey.kind`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::spoken::Spoken;

/// Identity of a menu node — OPEN on the wire and in the client (the "URL" of
/// the UI). The client treats every node identically; new kinds are server-only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeKey {
    pub kind: String,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

impl NodeKey {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            params: BTreeMap::new(),
        }
    }

    pub fn with(kind: impl Into<String>, params: &[(&str, &str)]) -> Self {
        let mut map = BTreeMap::new();
        for (k, v) in params {
            map.insert((*k).to_string(), (*v).to_string());
        }
        Self {
            kind: kind.into(),
            params: map,
        }
    }

    pub fn param(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(|s| s.as_str())
    }

    /// Convenience: read a required uuid param.
    pub fn uuid_param(&self, key: &str) -> Option<Uuid> {
        self.param(key).and_then(|s| Uuid::parse_str(s).ok())
    }

    /// Stable string id for breadcrumb/debug.
    pub fn debug_id(&self) -> String {
        if self.params.is_empty() {
            self.kind.clone()
        } else {
            let parts: Vec<String> = self
                .params
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            format!("{}[{}]", self.kind, parts.join(" "))
        }
    }
}

/// The five universal nav primitives. Owned by the generic runtime, identical
/// for every node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavAction {
    Up,
    Down,
    Back,
    Select,
    Wildcard,
}

/// Which step of a device gesture binding sequence we're capturing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindStep {
    Back,
    Up,
    Select,
    Down,
    Wildcard,
}

impl BindStep {
    pub const SEQUENCE: [BindStep; 5] = [
        BindStep::Back,
        BindStep::Up,
        BindStep::Select,
        BindStep::Down,
        BindStep::Wildcard,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BindStep::Back => "left / back",
            BindStep::Up => "up",
            BindStep::Select => "right / select",
            BindStep::Down => "down",
            BindStep::Wildcard => "center / enter",
        }
    }

    /// TTS-friendly variant of `label` ("left, back" instead of "left / back").
    pub fn spoken_label(self) -> &'static str {
        match self {
            BindStep::Back => "left, back",
            BindStep::Up => "up",
            BindStep::Select => "right, select",
            BindStep::Down => "down",
            BindStep::Wildcard => "center, enter",
        }
    }

    /// Stable name used in `device_rebind` node params.
    pub fn name(self) -> &'static str {
        match self {
            BindStep::Back => "back",
            BindStep::Up => "up",
            BindStep::Select => "select",
            BindStep::Down => "down",
            BindStep::Wildcard => "wildcard",
        }
    }

    pub fn from_name(name: &str) -> Option<BindStep> {
        Some(match name {
            "back" => BindStep::Back,
            "up" => BindStep::Up,
            "select" => BindStep::Select,
            "down" => BindStep::Down,
            "wildcard" => BindStep::Wildcard,
            _ => return None,
        })
    }

    /// The nav action this step's captured gesture should fire in normal use.
    pub fn nav_action(self) -> NavAction {
        match self {
            BindStep::Back => NavAction::Back,
            BindStep::Up => NavAction::Up,
            BindStep::Select => NavAction::Select,
            BindStep::Down => NavAction::Down,
            BindStep::Wildcard => NavAction::Wildcard,
        }
    }

    pub fn next(self) -> Option<BindStep> {
        match self {
            BindStep::Back => Some(BindStep::Up),
            BindStep::Up => Some(BindStep::Select),
            BindStep::Select => Some(BindStep::Down),
            BindStep::Down => Some(BindStep::Wildcard),
            BindStep::Wildcard => None,
        }
    }
}

/// Client-local settings that `LocalCmd::CycleSetting` can toggle. Never leave
/// the device.
///
/// These are the Options rows whose whole effect is on the device holding them:
/// how loud, how fast, how long a gap. A phone and a desk can disagree about
/// every one of them without either being wrong, which is exactly why they are
/// not server state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingKey {
    Tts,
    Ptt,
    Model,
    /// Playback speed percentage.
    PlaybackSpeed,
    /// Gap between spoken fragments, in milliseconds.
    FragmentGap,
    /// Play Forward, the FEATURE switch (not the per-session arm).
    PlayForward,
    /// Gap between messages while Play Forward is running, in milliseconds.
    PlayForwardGap,
    /// A key this build does not know. A newer server offering a setting an
    /// older client has never heard of degrades to "nothing happens" rather
    /// than to a wrong toggle.
    #[serde(other)]
    Unknown,
}

/// **Where a capture's transcript LANDS.**
///
/// The desktop already routes captures to three different places — a turn, a
/// compaction's focus instructions, a thread's summary — and it decides by which
/// scope the operator was in when he pressed. On the phone the microphone is the
/// device's, so the sink has to ride ON the capture instead, from the moment it
/// opens through to the STT request that resolves it.
///
/// Defaulted everywhere it appears, so an older client that never sets it keeps
/// its existing behaviour exactly: every capture is a turn.
/// **On `Copy`, and why it is gone.** Every variant used to be a unit variant,
/// so the enum was `Copy` and callers passed it around freely. `ReportAnswer`
/// carries the ticket and the question it answers — the transcript has to know
/// which answer block it lands in, and that is two strings — so the enum is
/// `Clone` now. Nothing about a sink was ever performance-sensitive; the clone
/// happens once per microphone press.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSink {
    /// Submit it as a turn to `addr`'s thread. The ordinary case.
    #[default]
    Turn,
    /// The focus text for a manual compaction of `addr`'s thread.
    CompactionInstructions,
    /// A new, manually-set (and therefore LOCKED) summary for `addr`'s thread.
    ThreadSummary,
    /// **A name for a new project (D21d).** He has no keyboard in his pocket,
    /// so the audio face names a project by voice: the server transcribes,
    /// NORMALIZES, and hands the slug back as the fulfilment text, and the
    /// client navigates to `new_project[name=<slug>]` to confirm it.
    ///
    /// It carries no `addr` — a project does not exist yet, so there is no
    /// thread for the words to land in. The transcript is the only product.
    ///
    /// **The visual face reads the same descriptor differently, and that is
    /// D21c**: holding a keyboard, it raises the naming sheet instead of the
    /// microphone and mints `CreateProject` itself. One row, one descriptor,
    /// two honest doors — the `+ Compose` precedent wearing project clothes.
    ProjectName,
    /// **A dictated answer to one question of a report ticket (D23e).**
    ///
    /// `action` is the ticket id and `question` is the answer block's own id.
    /// Both ride ON the capture rather than being inferred from where the
    /// cursor happens to be at mic-close, for the same reason the sink itself
    /// does: by the time the words come back, the descriptor that started them
    /// is long gone.
    ReportAnswer { action: String, question: String },
    /// A sink this build does not know. The server must nack rather than guess:
    /// guessing routes his words into the wrong thread.
    #[serde(other)]
    Unknown,
}

/// When a `Confirm` descriptor should actually prompt before firing `then`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmGate {
    /// Always prompt.
    Always,
    /// Prompt only when there are pending turns (the client tracks the count
    /// from `ServerInfo`/`TurnEvent` deltas).
    PendingTurns,
}

/// Where a mutation is addressed — the project/thread it lands in. Carried by
/// outbox entries so an offline mutation lands in the right thread on drain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnAddr {
    pub project: String,
    pub thread_id: Uuid,
}

/// Anchor pointing at a specific event (and optionally an option within it)
/// the operator is responding to. Mirrors the legacy EventAnchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventAnchor {
    pub event_idx: usize,
    #[serde(default)]
    pub option_idx: Option<usize>,
}

/// Opaque mutation envelope on the wire. The client relays or outboxes it
/// verbatim — it has no typed view. The server deserializes `payload` into the
/// typed `Mutation` enum at its boundary (unknown kind = nack).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationEnvelope {
    pub kind: String,
    pub payload: serde_json::Value,
}

/// Client-only effects. Part of the compiled-in instruction set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "with", rename_all = "snake_case")]
pub enum LocalCmd {
    /// Toggle mic capture; the runtime injects the current-item anchor at exec
    /// time when `addr` is None.
    ///
    /// On OPEN the runtime fires `Mutation::OpenCapture` so the placeholder is
    /// written at mic-open, not at stop — the moment a crash has to stay
    /// attributable from. On CLOSE it posts an `Stt` request carrying the same
    /// `addr` and `sink`.
    ToggleCapture {
        #[serde(default)]
        addr: Option<TurnAddr>,
        /// Where this capture's transcript lands. Defaults to a turn.
        #[serde(default)]
        sink: CaptureSink,
    },
    /// Cycle a client-local setting.
    CycleSetting(SettingKey),
    /// Re-enumerate bindable devices (shell executes `Effect::EnumerateDevices`).
    RefreshDeviceList,
    /// The operator picked a device in the picker (or a registered device in
    /// the binding menu); enters the rebind walk. Binding never pairs — it
    /// only sees already-connected devices.
    SelectBindCandidate { device_id: String },
    /// Scan for discoverable Bluetooth devices (shell executes
    /// `Effect::ScanBluetooth`) — the audio-device connect menu.
    RefreshBtList,
    /// The operator picked a Bluetooth device in the connect menu:
    /// pair + trust + connect it locally.
    ConnectBtDevice { mac: String },
    /// exit(7) — supervisor relaunches this client.
    ExitRestart,
    /// exit(0) — clean shutdown of this client.
    ExitShutdown,
}

/// Optimistic patches applied to the local projection store the instant a
/// mutation is fired, dropped when a newer base version arrives or on nack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "patch", rename_all = "snake_case")]
pub enum Patch {
    ReplaceItem {
        node: NodeKey,
        idx: usize,
        item: NavItem,
    },
    AppendItem {
        node: NodeKey,
        item: NavItem,
    },
    SetAnnouncement {
        node: NodeKey,
        announcement: Spoken,
    },
}

/// THE client instruction set — the only vocabulary compiled into clients.
/// Additions are deliberate, versioned via `proto_version`; serde tolerates
/// unknown variants so old clients degrade gracefully.
// Adjacently tagged (not internally tagged): newtype variants here wrap
// sequences (`Seq(Vec<_>)`) and bare types, which serde's internally-tagged
// representation cannot serialize. Adjacent tagging handles every variant shape
// and still supports the `#[serde(other)]` graceful-fallback catch-all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "do", content = "with", rename_all = "snake_case")]
pub enum ActionDescriptor {
    /// Push this node, announce, kick prefetch.
    Navigate(NodeKey),
    /// Replace the top of the stack (sibling move; Back skips it).
    SwapTop(NodeKey),
    /// Pop one scope.
    Pop,
    /// Server action — fully parameterized at build time. Opaque to the client.
    Mutate {
        m: MutationEnvelope,
        #[serde(default)]
        optimistic: Vec<Patch>,
        #[serde(default)]
        speak: Option<Spoken>,
    },
    /// Client-only effect.
    Local(LocalCmd),
    /// Wrap a descriptor in a confirmation gate.
    Confirm {
        prompt: Spoken,
        gate: ConfirmGate,
        then: Box<ActionDescriptor>,
    },
    /// Speak a line (no navigation).
    Say(Spoken),
    /// Run several descriptors in order.
    Seq(Vec<ActionDescriptor>),
    /// Explicit no-op.
    #[default]
    None,
    /// Newer-server descriptor an old client can't interpret. Graceful
    /// fallback (error tone + "needs a client update").
    #[serde(other)]
    Unknown,
}

/// **One entry of a row's long-press context menu (D25, ledger item 22).**
///
/// Server-declared, so a new context action is a server-only change exactly
/// like a new row: the client renders whatever labels arrive and fires whatever
/// descriptor rides with them. It never matches on the label.
///
/// Visual face only, and that asymmetry is deliberate rather than a gap — the
/// audio face has no long press, so every act reachable from a context menu is
/// also reachable as a ROW somewhere the ear can walk to. Archive is the worked
/// example: long-press on the root row for the thumb (D22d), and
/// project → Options → "archive project" for the ear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuEntry {
    pub label: String,
    pub action: ActionDescriptor,
}

impl MenuEntry {
    pub fn new(label: impl Into<String>, action: ActionDescriptor) -> Self {
        Self {
            label: label.into(),
            action,
        }
    }
}

/// **An inline form input carried by a row (D25, ledger item 23).**
///
/// The visual face mounts a widget under the row — `SecureField` with a reveal
/// toggle when `secret`, a multiline editor when `multiline`, a plain
/// `TextField` otherwise. The audio face reads the row as an ordinary spoken
/// line and offers no dictation into it, which is ledger item 24: a
/// Whisper-mangled API key is not a degraded answer, it is a wrong secret
/// written into an env file that fails silently three systems downstream.
///
/// An older client that has never heard of this field ignores it and still
/// walks the menu, which is why it is a defaulted `Option` and not a
/// `proto_version` bump.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FieldSpec {
    /// The engine's own key for this value — what a submit writes it under.
    pub name: String,
    /// The label shown above the widget.
    pub label: String,
    /// Mask it. **Defaults to TRUE**, matching the engine's own default: a
    /// field whose secrecy nobody declared is treated as a secret, because the
    /// failure of guessing wrong in the other direction is a key on a screen.
    #[serde(default = "default_true")]
    pub secret: bool,
    /// A report answer or a prose field; the widget is a text editor.
    #[serde(default)]
    pub multiline: bool,
    /// The engine's note under the field, verbatim.
    #[serde(default)]
    pub note: String,
    /// What is in the DRAFT store for this field right now — typed here, or
    /// typed in his TUI and synced. Empty when nothing is drafted.
    #[serde(default)]
    pub draft: String,
    /// A value has already been submitted for this field.
    #[serde(default)]
    pub submitted: bool,
    /// **A dim placeholder only — never a value.**
    ///
    /// The engine shows a pre-existing destination value as a hint and refuses
    /// to pre-load it into the input, because a pre-loaded value counts itself
    /// as filled and he submits something he never looked at. The hint travels
    /// so the phone can be honest in the same way; the widget must render it as
    /// a placeholder and never as content.
    #[serde(default)]
    pub hint: String,
}

fn default_true() -> bool {
    true
}

/// A single navigable row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavItem {
    pub spoken: Spoken,
    pub display: String,
    #[serde(default)]
    pub detail: Option<String>,
    pub on_select: ActionDescriptor,
    #[serde(default)]
    pub on_wildcard: Option<ActionDescriptor>,
    /// The row's long-press context menu (D25). Empty on every row that has
    /// none, which is nearly all of them.
    #[serde(default)]
    pub menu: Vec<MenuEntry>,
    /// An inline form input mounted under this row (D25). `None` on every row
    /// that is not a field.
    #[serde(default)]
    pub input: Option<FieldSpec>,
}

impl NavItem {
    /// A row whose spoken and display text are the same and whose only action
    /// is Select.
    pub fn simple(label: impl Into<String>, on_select: ActionDescriptor) -> Self {
        let label = label.into();
        Self {
            spoken: Spoken::plain(label.clone()),
            display: label,
            detail: None,
            on_select,
            on_wildcard: None,
            menu: Vec::new(),
            input: None,
        }
    }

    pub fn with_spoken(spoken: Spoken, display: impl Into<String>, on_select: ActionDescriptor) -> Self {
        Self {
            spoken,
            display: display.into(),
            detail: None,
            on_select,
            on_wildcard: None,
            menu: Vec::new(),
            input: None,
        }
    }

    pub fn wildcard(mut self, action: ActionDescriptor) -> Self {
        self.on_wildcard = Some(action);
        self
    }

    /// Attach a long-press context menu (D25).
    pub fn with_menu(mut self, menu: Vec<MenuEntry>) -> Self {
        self.menu = menu;
        self
    }

    /// Attach an inline form input (D25).
    pub fn with_input(mut self, spec: FieldSpec) -> Self {
        self.input = Some(spec);
        self
    }

    pub fn detail(mut self, text: impl Into<String>) -> Self {
        self.detail = Some(text.into());
        self
    }
}

/// A reachable node — drives the prefetch graph and supplies a title spoken
/// instantly on Navigate even on a cache miss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub to: NodeKey,
    pub title: Spoken,
}

impl Edge {
    pub fn new(to: NodeKey, title: impl Into<Spoken>) -> Self {
        Self {
            to,
            title: title.into(),
        }
    }
}

/// A materialized, traversable view of ONE node. Pure data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeProjection {
    pub node: NodeKey,
    pub announcement: Spoken,
    pub items: Vec<NavItem>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub on_wildcard: ActionDescriptor,
    pub version: u64,
}

impl NodeProjection {
    /// Collect every cacheable spoken fragment in this projection (announcement
    /// + item labels + edge titles). The prefetch walker batches the misses.
    pub fn spoken_fragments(&self) -> Vec<String> {
        let mut out = self.announcement.fragments();
        for item in &self.items {
            out.extend(item.spoken.fragments());
        }
        for edge in &self.edges {
            out.extend(edge.title.fragments());
        }
        out
    }
}
