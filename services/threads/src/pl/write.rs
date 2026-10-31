//! # The writers: what cauldron itself records (§0.4.5)
//!
//! Each fact cauldron knows and no harness transcript holds is one record type
//! here, and one PURE builder turning it into a `protoledger::Event` (no I/O,
//! no clock: the caller passes `ts`, and [`super::PlStore`] assigns the id and
//! appends). Keeping the builders pure is what lets the tests pin each event's
//! exact shape apart from the store.
//!
//! | fact | kind | body | `meta["cauldron.threads"]` |
//! |---|---|---|---|
//! | the composed prompt | `text` (role `user`) | the text the model was SHOWN | `raw_text`, `message_id`, `queue_id`, `headers`, `_bare` |
//! | settings | `settings`, `complete: true` | `system_prompt`, `model`, `cwd` (+ `effort`, `permission_mode`, `agent` when set) | `inject`: every inject flag |
//! | a network delivery | `peer_message` | `direction`, `peer`, `network`, `text` | `message_id`, `queue_id`, `mail` |
//! | a network join | `app` `cauldron.threads/network_joined` | — | `data`: `network`, `at_spawn` |
//! | the summary | `app` `cauldron.threads/summary_updated` | — | `data`: `summary`, `by` |
//! | the title | `title` (SPEC §3's neutral kind; the index reads it) | `text` | `by` |
//!
//! **Settings are FULL snapshots, never patches** (rev 3): every snapshot
//! carries every field, with `complete: true`, so SPEC §3's rule "absent fields
//! are unset" holds and a reader never has to fold earlier snapshots to know
//! what governed a turn. [`SettingsSnapshot`] has no optional "unchanged"
//! state to express a patch with.
//!
//! **A network join is its own event, the join at spawn included** (rev 3).
//! The `spawned-by` link is in the child's header (SPEC §2.1 — links are
//! header-only), but its networks are NOT folded into that header: each one is
//! a `network_joined` event after it, exactly as a later join would be, so a
//! reader finds every membership the same way.

use serde_json::{json, Map, Value};

use super::{Event, Kind, CT};

/// The composed prompt of one delivery: what the model was shown, and what it
/// was composed from.
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    /// The text as sent to the model (with `[#id date time]`, headers and any
    /// injected context): the event's `body.text` (SPEC §5).
    pub composed: String,
    /// The message before the daemon wrapped it.
    pub raw_text: String,
    pub message_id: Option<i64>,
    pub queue_id: Option<i64>,
    /// The application headers, as an object.
    pub headers: Map<String, Value>,
    /// The `_bare` flag: delivered without the daemon's wrapping.
    pub bare: bool,
    pub turn: Option<String>,
}

/// One FULL settings snapshot. Every field is written every time.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsSnapshot {
    /// The system prompt the model runs under (the composed one; `None` only
    /// when there is none, written as JSON `null`, never omitted).
    pub system_prompt: Option<String>,
    pub model: String,
    pub cwd: String,
    /// Every inject flag, as cauldron's `Injection` serialises it
    /// (`inject_first_turn_preamble`, …). Must be an object.
    pub inject: Map<String, Value>,
    pub effort: Option<String>,
    pub permission_mode: Option<String>,
    pub agent: Option<String>,
}

impl SettingsSnapshot {
    /// The snapshot of a thread's live settings. `system_prompt` is passed in
    /// because the one the model runs under is COMPOSED at spawn (the
    /// settings blob holds only the appended part).
    pub fn of(settings: &crate::settings::ThreadSettings, system_prompt: Option<String>) -> Self {
        let inject = match serde_json::to_value(settings.injection) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        };
        SettingsSnapshot {
            system_prompt,
            model: settings.model.clone(),
            cwd: settings.cwd.clone(),
            inject,
            effort: settings.effort.clone(),
            permission_mode: settings.permission_mode.clone(),
            agent: settings.agent.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerDirection {
    In,
    Out,
}

impl PeerDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            PeerDirection::In => "in",
            PeerDirection::Out => "out",
        }
    }
}

/// A message delivered over a network, in the ledger of the side named by
/// `direction` (`in`: the recipient's; `out`: the sender's).
#[derive(Debug, Clone, PartialEq)]
pub struct PeerMessage {
    pub direction: PeerDirection,
    /// The other agent's thread id.
    pub peer: String,
    pub network: String,
    pub text: String,
    pub message_id: Option<i64>,
    pub queue_id: Option<i64>,
    /// The mail kind (`direct`, `broadcast`, …), when known.
    pub mail_kind: Option<String>,
    pub turn: Option<String>,
}

/// A thread joined a network.
#[derive(Debug, Clone, PartialEq)]
pub struct NetworkJoin {
    pub network: String,
    /// The join happened as part of the spawn (still its own event).
    pub at_spawn: bool,
}

/// A namespaced application fact.
#[derive(Debug, Clone, PartialEq)]
pub enum AppFact {
    /// `cauldron.threads/summary_updated {summary, by}`.
    Summary { summary: String, by: Option<String> },
    /// The thread's title: SPEC §3's neutral `title` kind (`{text}`), with
    /// `by` in CT meta.
    Title { text: String, by: Option<String> },
}

/// A managed agent spawned by another thread: a ROOT ledger with a
/// `spawned-by` link (never a structural `parent`, SPEC §2.1), its first
/// settings snapshot, and one join event per network.
#[derive(Debug, Clone, PartialEq)]
pub struct Spawn {
    pub thread_id: String,
    pub created_at: String,
    /// The spawning thread.
    pub parent: String,
    /// The parent's event that spawned it (the `agents_spawn` call), if known.
    pub parent_event: Option<String>,
    pub spawn_request_id: Option<String>,
    pub budget_key: Option<String>,
    pub settings: SettingsSnapshot,
    pub networks: Vec<String>,
}

// ---- the pure builders --------------------------------------------------

fn opt<T: Into<Value>>(v: Option<T>) -> Value {
    v.map(Into::into).unwrap_or(Value::Null)
}

/// `text` (role `user`), body = what the model was shown, CT meta = what it
/// was composed from.
pub fn prompt_event(id: &str, ts: &str, p: &Prompt) -> Event {
    Event::new(id, Kind::Text)
        .ts(Some(ts))
        .turn(p.turn.clone())
        .with("role", "user")
        .with("text", p.composed.clone())
        .meta_ns(
            CT,
            json!({
                "raw_text": p.raw_text,
                "message_id": opt(p.message_id),
                "queue_id": opt(p.queue_id),
                "headers": Value::Object(p.headers.clone()),
                "_bare": p.bare,
            }),
        )
}

/// `settings`, `complete: true`, every field present.
pub fn settings_event(id: &str, ts: &str, s: &SettingsSnapshot) -> Event {
    let mut e = Event::new(id, Kind::Settings).ts(Some(ts));
    // Inserted directly, not through `with`, which skips nulls: a full
    // snapshot says "no system prompt" out loud.
    e.body.insert("complete".into(), Value::Bool(true));
    e.body.insert("system_prompt".into(), opt(s.system_prompt.clone()));
    e.body.insert("model".into(), Value::String(s.model.clone()));
    e.body.insert("cwd".into(), Value::String(s.cwd.clone()));
    let e = e
        .with("effort", opt(s.effort.clone()))
        .with("permission_mode", opt(s.permission_mode.clone()))
        .with("agent", opt(s.agent.clone()));
    e.meta_ns(CT, json!({ "inject": Value::Object(s.inject.clone()) }))
}

/// `peer_message`, the SPEC §3 body, CT meta for the queue bookkeeping.
pub fn peer_message_event(id: &str, ts: &str, m: &PeerMessage) -> Event {
    let mut mail = Map::new();
    mail.insert("network".into(), Value::String(m.network.clone()));
    if let Some(k) = &m.mail_kind {
        mail.insert("kind".into(), Value::String(k.clone()));
    }
    let who = if m.direction == PeerDirection::In { "from" } else { "to" };
    mail.insert(who.into(), Value::String(m.peer.clone()));
    Event::new(id, Kind::PeerMessage)
        .ts(Some(ts))
        .turn(m.turn.clone())
        .with("direction", m.direction.as_str())
        .with("peer", m.peer.clone())
        .with("network", m.network.clone())
        .with("text", m.text.clone())
        .meta_ns(
            CT,
            json!({ "message_id": opt(m.message_id), "queue_id": opt(m.queue_id), "mail": Value::Object(mail) }),
        )
}

/// The `app` type of a network join.
pub fn network_joined_type() -> String {
    format!("{CT}/network_joined")
}

/// The `app` type of a summary.
pub fn summary_type() -> String {
    format!("{CT}/summary_updated")
}

fn app_event(id: &str, ts: &str, ty: String, data: Value) -> Event {
    Event::new(id, Kind::App).ts(Some(ts)).with("type", ty).with("data", data)
}

/// `app cauldron.threads/network_joined {network, at_spawn}`.
pub fn network_join_event(id: &str, ts: &str, j: &NetworkJoin) -> Event {
    app_event(id, ts, network_joined_type(), json!({ "network": j.network, "at_spawn": j.at_spawn }))
}

/// A summary as namespaced `app`; a title as the neutral `title`.
pub fn app_fact_event(id: &str, ts: &str, f: &AppFact) -> Event {
    match f {
        AppFact::Summary { summary, by } => {
            app_event(id, ts, summary_type(), json!({ "summary": summary, "by": opt(by.clone()) }))
        }
        AppFact::Title { text, by } => Event::new(id, Kind::Title)
            .ts(Some(ts))
            .with("text", text.clone())
            .meta_ns(CT, json!({ "by": opt(by.clone()) })),
    }
}

/// The short word in an event id for each writer: `ct:<word>:<seq>`.
pub fn id_word(e: &Event) -> &'static str {
    match e.kind {
        Kind::Text => "prompt",
        Kind::Settings => "settings",
        Kind::PeerMessage => "peer",
        Kind::Title => "title",
        Kind::App => "app",
        _ => "event",
    }
}
