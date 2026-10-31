//! `aui-wire` — the slice of harness `libs/aui-state` a FACE links to speak the
//! AUI's proto-v2 mutation socket (9010): the wire protocol (`proto`), the
//! opaque `MutationEnvelope` and the node vocabulary it rides in (`node`,
//! `spoken`), the model enum (`model`), and the typed `Mutation` a face mints
//! by hand (`server`, the name kept so the moved face code reads unchanged).
//!
//! MOVED, not shared: the AUI itself stays in harness (CON-15) and keeps its
//! own `aui-state`. The two copies define ONE wire, so a change to either is a
//! change to the other — see `PROVENANCE.md`.

pub mod model;
pub mod node;
pub mod proto;
pub mod server;
pub mod spoken;

pub use model::Model;
pub use node::{
    ActionDescriptor, BindStep, CaptureSink, ConfirmGate, Edge, EventAnchor, FieldSpec, LocalCmd,
    MenuEntry, MutationEnvelope, NavAction, NavItem, NodeKey, NodeProjection, Patch, SettingKey,
    TurnAddr,
};
pub use proto::{
    AudioClip, AudioFormat, ClientInfo, ClientMsg, DeferralSignals, MutResult, PromiseResult,
    RequestBody, RingTarget, ServerMsg, ToneBeep, ToneSpec, TurnEventKind, DEFAULT_PORT,
    PROTO_VERSION, RECENT_INPUT_WINDOW_MS,
};
pub use spoken::{Spoken, SpokenSlot};
