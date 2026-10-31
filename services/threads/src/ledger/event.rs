//! # The event shape, its identity, and the append-only ledger
//!
//! `docs/proto-ledger.md` §1–§2. One struct, one closed kind set, one opaque
//! slot for the runtime's bytes.
//!
//! **Identity is written into the event.** [`LedgerEvent::id`] is the runtime's
//! own id for an ingested line (Claude Code's `uuid`, present on 100% of
//! archived lines) or a v4 minted at authorship for a daemon-authored event.
//! `seq` is a POSITION, assigned at append, and never an identity — the
//! operator's *"write the event id into each event, not the jsonl line
//! position"*.
//!
//! **The vocabulary speaks the chassis's dtypes.** [`LEDGER_EVENT_CELLS`] is
//! declared through [`shapewire::declare_cells!`], so the same compile-time
//! checks that guard a shape's manifest guard this one, and
//! `the_serialized_event_is_the_manifest` proves the serialized key set is the
//! manifest's — a manifest that drifted from the struct is red there.
//!
//! **Immutable forever.** [`Ledger`] has `append` and reads. There is no
//! `events_mut`, no `remove`, no `set`; the file form is one line appended per
//! event. `append_never_moves_a_past_event` forces the one thing a bug here
//! could do — renumber — red.

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use shapewire::operation::{
    DType::{Cursor, Enum, Opaque, Ref, Scalar as Sc, Struct, Timestamp},
    Presence::{Optional, Required},
    Scalar::{Bool, Int, Str},
    TimeEncoding,
};

/// The runtime's name for an observation it made, or the daemon's for one it
/// authored. A uuid string in both cases today.
pub type EventId = String;

/// **THE CLOSED KIND SET** — thirteen words, `docs/proto-ledger.md` §1. Adding
/// one is a finding about what is universal across agent runtimes before it is
/// a change to this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Words put into the thread by the operator or a client.
    User,
    /// A text block the model produced.
    Assistant,
    /// A reasoning block, possibly opaque to us; the signature stays in `native`.
    Thinking,
    /// The model called a tool.
    ToolUse,
    /// The result fed back.
    ToolResult,
    /// A turn began.
    TurnStarted,
    /// A turn ended, with an outcome.
    TurnEnded,
    /// The runtime summarised its past: a boundary, then a summary.
    Compaction,
    /// The model was told something that is not a user message.
    Context,
    /// Tokens.
    Usage,
    /// A child agent started or finished.
    Child,
    /// A daemon-authored fact about the thread: settings, placement, projected, reconciled.
    System,
    /// A line the adapter has no generic kind for; the line is in `native`.
    Native,
}

/// The words of [`Kind`], as the cell manifest publishes them. Kept in lock-step
/// with the enum by `every_kind_has_its_word`.
pub const KIND_WORDS: &[&str] = &[
    "user",
    "assistant",
    "thinking",
    "tool_use",
    "tool_result",
    "turn_started",
    "turn_ended",
    "compaction",
    "context",
    "usage",
    "child",
    "system",
    "native",
];

impl Kind {
    pub const ALL: [Kind; 13] = [
        Kind::User,
        Kind::Assistant,
        Kind::Thinking,
        Kind::ToolUse,
        Kind::ToolResult,
        Kind::TurnStarted,
        Kind::TurnEnded,
        Kind::Compaction,
        Kind::Context,
        Kind::Usage,
        Kind::Child,
        Kind::System,
        Kind::Native,
    ];

    pub fn word(self) -> &'static str {
        match self {
            Kind::User => "user",
            Kind::Assistant => "assistant",
            Kind::Thinking => "thinking",
            Kind::ToolUse => "tool_use",
            Kind::ToolResult => "tool_result",
            Kind::TurnStarted => "turn_started",
            Kind::TurnEnded => "turn_ended",
            Kind::Compaction => "compaction",
            Kind::Context => "context",
            Kind::Usage => "usage",
            Kind::Child => "child",
            Kind::System => "system",
            Kind::Native => "native",
        }
    }

    /// **WHICH KINDS GET A MESSAGE ID.** The small addressable ordinal is for
    /// what a person refers to — what was said to the thread and what the
    /// model said back — exactly the two kinds the live store numbers today.
    pub fn addressable(self) -> bool {
        matches!(self, Kind::User | Kind::Assistant)
    }
}

/// **THE RUNTIME'S OWN LINE, WHOLE AND OPAQUE.** Declared `Opaque` in the
/// manifest: the general form stores and returns it and may not branch on it.
/// Only the adapter named by `runtime` reads it, and only at projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Native {
    pub runtime: String,
    pub line: Value,
}

/// The names a `Native.runtime` may carry. Only the first has an adapter; the
/// other two are named targets, and naming them here is the whole extent of
/// their existence in this crate.
pub const RUNTIME_WORDS: &[&str] = &["claude_code", "opencode", "codex"];

/// **ONE EVENT.** `docs/proto-ledger.md` §1. Optional fields serialize only when
/// present, so the file form carries exactly the manifest's `Required` keys plus
/// whichever `Optional` ones apply — which is what the manifest test checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerEvent {
    pub id: EventId,
    pub thread_id: String,
    pub seq: u64,
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<u64>,
    pub kind: Kind,
    pub body: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
}

shapewire::declare_cells! {
    /// **WHAT AN EVENT CARRIES**, in the chassis's dtypes. `body` is a tagged
    /// union over `kind`, declared as the manifest doc says a union is: the
    /// union of every kind's fields, each `Optional`. `native` is a struct
    /// whose `line` is `Opaque` — a positive prohibition, not an omission.
    pub LEDGER_EVENT_CELLS = [
        ("id", Ref("event"), Required, []),
        ("thread_id", Ref("thread"), Required, []),
        ("seq", Cursor("ledger"), Required, []),
        ("ts", Timestamp(TimeEncoding::Rfc3339), Required, []),
        ("turn", Ref("turn"), Optional, []),
        ("within", Ref("tool_use"), Optional, []),
        ("message_id", Cursor("message"), Optional, []),
        ("kind", Enum(KIND_WORDS), Required, []),
        ("body", Struct("EventBody"), Required, [
            // user
            ("text", Sc(Str), Optional),
            ("intent", Enum(&["immediate", "turn_end"]), Optional),
            ("metadata", Opaque, Optional),
            // assistant / usage
            ("api_message_id", Sc(Str), Optional),
            // tool_use
            ("id", Sc(Str), Optional),
            ("name", Sc(Str), Optional),
            ("input", Opaque, Optional),
            // tool_result
            ("tool_use_id", Sc(Str), Optional),
            ("content", Opaque, Optional),
            ("is_error", Sc(Bool), Optional),
            // turn_started / turn_ended
            ("attempt", Sc(Int), Optional),
            ("message_ids", shapewire::operation::DType::List(&Cursor("message")), Optional),
            ("outcome", Enum(&["ok", "failed", "interrupted", "cancelled"]), Optional),
            ("detail", Opaque, Optional),
            // compaction
            ("phase", Enum(&["boundary", "summary", "started", "finished"]), Optional),
            ("trigger", Sc(Str), Optional),
            ("pre_tokens", Sc(Int), Optional),
            ("summary_text", Sc(Str), Optional),
            // usage
            ("model", Sc(Str), Optional),
            ("input_tokens", Sc(Int), Optional),
            ("output_tokens", Sc(Int), Optional),
            ("cache_read_tokens", Sc(Int), Optional),
            ("cache_creation_tokens", Sc(Int), Optional),
            // child
            ("child_id", Ref("child"), Optional),
            // system
            ("what", Enum(&["init", "api_error", "settings", "placement", "projected", "reconciled"]), Optional),
        ]),
        ("native", Struct("Native"), Optional, [
            ("runtime", Enum(RUNTIME_WORDS), Required),
            ("line", Opaque, Required),
        ]),
    ]
}

/// **ONE THREAD'S METADATA.** `parent_id` is the whole of what distinguishes a
/// managed subagent from any other thread (the ruling: *"exactly the same as
/// their parents, except in the thread metadata we store their parent ID and
/// derive their depth"*). Depth is [`ThreadMeta::depth`], a walk, not a column.
///
/// # `parent_id` IS PRIVATE, AND THAT IS WHAT MAKES A CYCLE UNCONSTRUCTIBLE
///
/// There are exactly two ways to get a `ThreadMeta`: [`ThreadMeta::root`],
/// which has no parent, and [`ThreadMeta::child_of`], which takes **the
/// parent's own `ThreadMeta` by reference** rather than a parent id string.
/// You cannot name a parent you are not holding.
///
/// That single choice makes the parent-pointer graph a **forest by
/// construction**, and the proof is two lines: a child can only be built after
/// its parent exists, so every edge points strictly backwards in construction
/// order; a cycle needs an edge that points forwards; therefore there is no
/// cycle. Nothing counts, nothing checks, and no depth cap is load-bearing for
/// it — the shape is unreachable rather than rejected.
///
/// **The one door construction does not own is deserialization**, which is a
/// store read off disk and may hold anything a corrupt or hand-edited file
/// holds. So [`ThreadMeta::depth`] still terminates on a cycle — but it now
/// *names* one instead of returning a bounded wrong number, and it needs no
/// `limit` to do it. The cap on depth is a separate thing: a policy, enforced
/// at the spawn (`super::spawn::MAX_DEPTH`), never a cycle guard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadMeta {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_id: Option<String>,
    pub runtime: String,
    pub created_at: String,
}

/// **A CYCLE IN A STORE READ OFF DISK.** Construction cannot produce one
/// ([`ThreadMeta`]); deserialization can, and this is what the walk says when
/// it meets one. `at` is the id the walk arrived at for the second time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleFound {
    pub from: String,
    pub at: String,
    /// The ids walked, in order, up to and excluding the repeat.
    pub walked: Vec<String>,
}

impl std::fmt::Display for CycleFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "thread {} has no depth: its parent walk returns to {} after {} \
             — a cycle, which construction cannot build, so the store is corrupt",
            self.from,
            self.at,
            self.walked.join(" -> ")
        )
    }
}

impl ThreadMeta {
    /// **A ROOT.** No parent, depth 0.
    pub fn root(id: impl Into<String>, runtime: impl Into<String>, created_at: impl Into<String>) -> Self {
        ThreadMeta {
            id: id.into(),
            parent_id: None,
            runtime: runtime.into(),
            created_at: created_at.into(),
        }
    }

    /// **A CHILD OF A PARENT THAT EXISTS.** The parent arrives as a
    /// `&ThreadMeta`, not an id, which is the whole of the acyclicity argument
    /// on [`ThreadMeta`]: the parent had to be built first.
    ///
    /// The child is *exactly its parent* but for `parent_id` — `runtime` is
    /// taken from the parent and is not a parameter, because a child that
    /// could be given a different runtime would be a second kind of thread.
    pub fn child_of(parent: &ThreadMeta, id: impl Into<String>, created_at: impl Into<String>) -> Self {
        ThreadMeta {
            id: id.into(),
            parent_id: Some(parent.id.clone()),
            runtime: parent.runtime.clone(),
            created_at: created_at.into(),
        }
    }

    /// The parent's id, if this thread has one. Read-only: there is no setter,
    /// so a parent pointer never changes after construction.
    pub fn parent_id(&self) -> Option<&str> {
        self.parent_id.as_deref()
    }

    /// Depth derived by walking `parent_id`: 0 at a root. `lookup` answers a
    /// thread's metadata by id; a parent the store cannot find ends the walk
    /// (the depth counted so far is what is known).
    ///
    /// **No `limit`.** The walk carries the set of ids it has visited, so it
    /// terminates on a cycle because it recognises one, not because it gave up
    /// counting — and a legitimately deep chain is never silently truncated by
    /// a cap chosen for a different purpose. A cycle is impossible to
    /// construct, so meeting one means the store is corrupt, and the walk says
    /// which link closed it ([`CycleFound`]) rather than returning a number.
    pub fn depth(&self, lookup: impl Fn(&str) -> Option<ThreadMeta>) -> Result<u32, CycleFound> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut walked: Vec<String> = Vec::new();
        seen.insert(self.id.clone());
        walked.push(self.id.clone());
        let mut depth = 0;
        let mut cur = self.parent_id.clone();
        while let Some(pid) = cur {
            if !seen.insert(pid.clone()) {
                return Err(CycleFound {
                    from: self.id.clone(),
                    at: pid,
                    walked,
                });
            }
            walked.push(pid.clone());
            depth += 1;
            cur = lookup(&pid).and_then(|m| m.parent_id);
        }
        Ok(depth)
    }
}

/// **WHAT AN APPEND TAKES.** Everything the ledger does not assign: the ledger
/// assigns `seq`, `message_id` and — when `id` is `None` — a minted id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    pub id: Option<EventId>,
    pub ts: String,
    pub turn: Option<String>,
    pub within: Option<String>,
    pub kind: Option<Kind>,
    pub body: Value,
    pub native: Option<Native>,
}

impl Draft {
    pub fn new(kind: Kind, ts: impl Into<String>, body: Value) -> Self {
        Draft {
            id: None,
            ts: ts.into(),
            turn: None,
            within: None,
            kind: Some(kind),
            body,
            native: None,
        }
    }
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }
    pub fn in_turn(mut self, turn: Option<&str>) -> Self {
        self.turn = turn.map(str::to_string);
        self
    }
    pub fn within(mut self, within: Option<String>) -> Self {
        self.within = within;
        self
    }
    pub fn native(mut self, runtime: &str, line: Value) -> Self {
        self.native = Some(Native {
            runtime: runtime.to_string(),
            line,
        });
        self
    }
}

/// Mint a daemon-authored id. v4, at the moment of authorship —
/// `docs/proto-ledger.md` §2 says why not a content hash.
pub fn mint_id() -> EventId {
    uuid::Uuid::new_v4().to_string()
}

/// **THE APPEND-ONLY LEDGER OF ONE THREAD.**
#[derive(Debug, Clone)]
pub struct Ledger {
    meta: ThreadMeta,
    events: Vec<LedgerEvent>,
    ids: HashSet<EventId>,
    next_message_id: u64,
    last_compaction_message_id: u64,
}

/// What `append` reports about the event it wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Appended {
    pub seq: u64,
    pub message_id: Option<u64>,
    /// `true` when the draft carried no id and one was minted.
    pub id_minted: bool,
}

impl Ledger {
    pub fn new(meta: ThreadMeta) -> Self {
        Ledger {
            meta,
            events: Vec::new(),
            ids: HashSet::new(),
            next_message_id: 1,
            last_compaction_message_id: 0,
        }
    }

    pub fn meta(&self) -> &ThreadMeta {
        &self.meta
    }
    pub fn thread_id(&self) -> &str {
        &self.meta.id
    }
    pub fn events(&self) -> &[LedgerEvent] {
        &self.events
    }
    pub fn len(&self) -> usize {
        self.events.len()
    }
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
    pub fn contains_id(&self, id: &str) -> bool {
        self.ids.contains(id)
    }
    pub fn last_seq(&self) -> u64 {
        self.events.last().map(|e| e.seq).unwrap_or(0)
    }

    /// `<thread_id>.<last_compaction_message_id>` — unchanged from the live
    /// store's derivation, and moved by exactly one kind, `compaction`.
    pub fn context_id(&self) -> String {
        format!("{}.{}", self.meta.id, self.last_compaction_message_id)
    }

    /// **THE ONE WRITE.** Assigns `seq` (dense from 1), `message_id` (for
    /// addressable kinds), mints an id if the draft has none, and refuses a
    /// draft whose id is already here — a replay is not a second event.
    pub fn append(&mut self, draft: Draft) -> Result<Appended, AppendRefused> {
        let kind = draft.kind.ok_or(AppendRefused::NoKind)?;
        let (id, id_minted) = match draft.id {
            Some(id) => (id, false),
            None => (mint_id(), true),
        };
        if self.ids.contains(&id) {
            return Err(AppendRefused::Replay { id });
        }
        let seq = self.last_seq() + 1;
        let message_id = if kind.addressable() {
            let m = self.next_message_id;
            self.next_message_id += 1;
            Some(m)
        } else {
            None
        };
        if kind == Kind::Compaction
            && draft.body.get("phase").and_then(Value::as_str) == Some("boundary")
        {
            // Everything addressable before the boundary is behind it.
            self.last_compaction_message_id = self.next_message_id.saturating_sub(1);
        }
        self.ids.insert(id.clone());
        self.events.push(LedgerEvent {
            id,
            thread_id: self.meta.id.clone(),
            seq,
            ts: draft.ts,
            turn: draft.turn,
            within: draft.within,
            message_id,
            kind,
            body: draft.body,
            native: draft.native,
        });
        Ok(Appended {
            seq,
            message_id,
            id_minted,
        })
    }

    /// **THE FILE FORM.** Line 1 is `{"thread": <meta>}`; every later line is
    /// one event, its id inside it. Written whole here; [`Ledger::append_to_file`]
    /// is the append-only path a daemon would use.
    pub fn write_jsonl(&self, path: &Path) -> std::io::Result<()> {
        let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
        serde_json::to_writer(&mut f, &json!({ "thread": self.meta }))?;
        f.write_all(b"\n")?;
        for e in &self.events {
            serde_json::to_writer(&mut f, e)?;
            f.write_all(b"\n")?;
        }
        f.flush()
    }

    /// Append one already-appended event's line to an existing file.
    pub fn append_to_file(path: &Path, event: &LedgerEvent) -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
        serde_json::to_writer(&mut f, event)?;
        f.write_all(b"\n")
    }

    /// Read the file form back. Every event is re-appended through the one
    /// write, so `seq` and `message_id` are re-derived and a file whose
    /// numbering disagrees with its order is refused by name rather than
    /// trusted.
    pub fn read_jsonl(path: &Path) -> Result<Ledger, ReadError> {
        let f = std::fs::File::open(path).map_err(ReadError::Io)?;
        let mut lines = std::io::BufReader::new(f).lines();
        let head = lines
            .next()
            .ok_or(ReadError::Empty)?
            .map_err(ReadError::Io)?;
        let head: Value = serde_json::from_str(&head).map_err(|e| ReadError::Malformed(1, e.to_string()))?;
        let meta: ThreadMeta = serde_json::from_value(head.get("thread").cloned().unwrap_or(Value::Null))
            .map_err(|e| ReadError::Malformed(1, format!("line 1 is not {{\"thread\": …}}: {e}")))?;
        let mut ledger = Ledger::new(meta);
        for (i, line) in lines.enumerate() {
            let line = line.map_err(ReadError::Io)?;
            if line.trim().is_empty() {
                continue;
            }
            let n = i + 2;
            let e: LedgerEvent =
                serde_json::from_str(&line).map_err(|err| ReadError::Malformed(n, err.to_string()))?;
            let draft = Draft {
                id: Some(e.id.clone()),
                ts: e.ts.clone(),
                turn: e.turn.clone(),
                within: e.within.clone(),
                kind: Some(e.kind),
                body: e.body.clone(),
                native: e.native.clone(),
            };
            let got = ledger.append(draft).map_err(|r| ReadError::Malformed(n, format!("{r:?}")))?;
            if got.seq != e.seq || got.message_id != e.message_id {
                return Err(ReadError::Renumbered {
                    line: n,
                    id: e.id,
                    file: (e.seq, e.message_id),
                    derived: (got.seq, got.message_id),
                });
            }
        }
        Ok(ledger)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendRefused {
    /// A draft with no kind describes nothing.
    NoKind,
    /// This id is already in the ledger. A replay is not a second event.
    Replay { id: EventId },
}

#[derive(Debug)]
pub enum ReadError {
    Io(std::io::Error),
    Empty,
    Malformed(usize, String),
    /// The file's `seq`/`message_id` disagree with what its order derives —
    /// a line was removed or reordered, which an immutable ledger forbids.
    Renumbered {
        line: usize,
        id: EventId,
        file: (u64, Option<u64>),
        derived: (u64, Option<u64>),
    },
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "io: {e}"),
            ReadError::Empty => write!(f, "an empty file is not a ledger: line 1 must be {{\"thread\": …}}"),
            ReadError::Malformed(n, e) => write!(f, "line {n}: {e}"),
            ReadError::Renumbered { line, id, file, derived } => write!(
                f,
                "line {line} (event {id}) carries seq/message_id {file:?} but its position derives {derived:?}: \
                 a line was removed or reordered, which an immutable ledger forbids"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn meta() -> ThreadMeta {
        ThreadMeta::root("t-1", "claude_code", "2026-10-08T00:00:00Z")
    }

    fn full_event() -> LedgerEvent {
        LedgerEvent {
            id: "e-1".into(),
            thread_id: "t-1".into(),
            seq: 1,
            ts: "2026-10-08T00:00:00Z".into(),
            turn: Some("c-1".into()),
            within: Some("toolu_1".into()),
            message_id: Some(1),
            kind: Kind::User,
            body: json!({"text": "hi"}),
            native: Some(Native {
                runtime: "claude_code".into(),
                line: json!({"type": "user"}),
            }),
        }
    }

    fn keys(v: &Value) -> BTreeSet<String> {
        v.as_object().unwrap().keys().cloned().collect()
    }

    /// **THE SERIALIZED EVENT IS THE MANIFEST.** With every optional present the
    /// key set is every declared cell; with none present it is exactly the
    /// `Required` cells. A manifest that names a cell the struct does not
    /// carry, or a struct field the manifest does not declare, is red here.
    ///
    /// **THE MUTATION.** Rename `turn` to `cookie` in the manifest.
    ///
    /// Forced red:
    /// ```text
    /// the serialized event's keys are the manifest's cells
    /// ```
    #[test]
    fn the_serialized_event_is_the_manifest() {
        let all: BTreeSet<String> = LEDGER_EVENT_CELLS.names().iter().map(|s| s.to_string()).collect();
        let required: BTreeSet<String> = LEDGER_EVENT_CELLS
            .cells()
            .iter()
            .filter(|c| matches!(c.presence, Required))
            .map(|c| c.name.to_string())
            .collect();
        let full = serde_json::to_value(full_event()).unwrap();
        assert_eq!(keys(&full), all, "the serialized event's keys are the manifest's cells");
        let mut bare = full_event();
        bare.turn = None;
        bare.within = None;
        bare.message_id = None;
        bare.native = None;
        let bare = serde_json::to_value(bare).unwrap();
        assert_eq!(keys(&bare), required, "…and with every optional absent, exactly the required ones");
        // The native sub-struct, too.
        let native_fields: BTreeSet<String> = LEDGER_EVENT_CELLS
            .get("native")
            .unwrap()
            .fields
            .iter()
            .map(|f| f.name.to_string())
            .collect();
        assert_eq!(keys(&full["native"]), native_fields);
    }

    /// **EVERY KIND HAS ITS WORD, AND THE WORDS ARE THE ENUM'S.** The manifest
    /// publishes `KIND_WORDS`; serde spells `Kind`; the two must agree or a
    /// consumer checked against the manifest reads a word the file never
    /// carries.
    ///
    /// **THE MUTATION.** Drop `"native"` from `KIND_WORDS`.
    ///
    /// Forced red:
    /// ```text
    /// every Kind serializes to a word in KIND_WORDS, and every word is a Kind
    /// ```
    #[test]
    fn every_kind_has_its_word() {
        let spelled: Vec<String> = Kind::ALL
            .iter()
            .map(|k| serde_json::to_value(k).unwrap().as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            spelled,
            KIND_WORDS,
            "every Kind serializes to a word in KIND_WORDS, and every word is a Kind"
        );
        for k in Kind::ALL {
            assert_eq!(k.word(), serde_json::to_value(k).unwrap().as_str().unwrap());
        }
        assert_eq!(
            LEDGER_EVENT_CELLS.get("kind").unwrap().dtype.words(),
            KIND_WORDS,
            "and the manifest publishes those words"
        );
    }

    /// **APPEND NEVER MOVES A PAST EVENT.** The only write renumbers nothing:
    /// `seq` and `message_id` of event 1 are what they were after event 3 lands,
    /// and a replayed id is refused rather than becoming a second event.
    ///
    /// **THE MUTATION.** In `append`, after the push, add
    /// `self.events[0].seq = seq;` — a write that reaches a past event.
    ///
    /// Forced red:
    /// ```text
    /// an append never changes an earlier event
    /// ```
    #[test]
    fn append_never_moves_a_past_event() {
        let mut l = Ledger::new(meta());
        let a = l
            .append(Draft::new(Kind::User, "2026-10-08T00:00:01Z", json!({"text": "one"})).with_id("a"))
            .unwrap();
        assert_eq!((a.seq, a.message_id, a.id_minted), (1, Some(1), false));
        let first = l.events()[0].clone();
        let b = l
            .append(Draft::new(Kind::Usage, "2026-10-08T00:00:02Z", json!({"model": "m"})))
            .unwrap();
        assert_eq!((b.seq, b.message_id, b.id_minted), (2, None, true), "usage is not addressable; no id → minted");
        let c = l
            .append(Draft::new(Kind::Assistant, "2026-10-08T00:00:03Z", json!({"text": "two"})).with_id("c"))
            .unwrap();
        assert_eq!((c.seq, c.message_id), (3, Some(2)));
        assert_eq!(l.events()[0], first, "an append never changes an earlier event");
        assert_eq!(
            l.append(Draft::new(Kind::User, "2026-10-08T00:00:04Z", json!({})).with_id("a")),
            Err(AppendRefused::Replay { id: "a".into() }),
            "a replayed id is refused"
        );
        assert_eq!(l.len(), 3);
        assert_eq!(
            l.append(Draft { kind: None, ..Draft::default() }),
            Err(AppendRefused::NoKind)
        );
    }

    /// **THE FILE FORM ROUND-TRIPS, AND A RENUMBERED FILE IS REFUSED.**
    ///
    /// **THE MUTATION.** In `read_jsonl`, delete the `got.seq != e.seq` check.
    ///
    /// Forced red:
    /// ```text
    /// a file whose numbering disagrees with its order is refused
    /// ```
    #[test]
    fn the_file_form_round_trips_and_refuses_a_renumbered_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t-1.jsonl");
        let mut l = Ledger::new(meta());
        l.append(Draft::new(Kind::User, "2026-10-08T00:00:01Z", json!({"text": "one"})).with_id("a"))
            .unwrap();
        l.append(
            Draft::new(Kind::Compaction, "2026-10-08T00:00:02Z", json!({"phase": "boundary"})).with_id("b"),
        )
        .unwrap();
        l.append(Draft::new(Kind::Assistant, "2026-10-08T00:00:03Z", json!({"text": "two"})).with_id("c"))
            .unwrap();
        assert_eq!(l.context_id(), "t-1.1", "the boundary moved the context id past message 1");
        l.write_jsonl(&path).unwrap();
        let back = Ledger::read_jsonl(&path).unwrap();
        assert_eq!(back.events(), l.events());
        assert_eq!(back.meta(), l.meta());
        assert_eq!(back.context_id(), "t-1.1");
        // Append-only path writes the same line `write_jsonl` would.
        let mut more = l.clone();
        more.append(Draft::new(Kind::Usage, "2026-10-08T00:00:04Z", json!({"model": "m"})).with_id("d"))
            .unwrap();
        Ledger::append_to_file(&path, more.events().last().unwrap()).unwrap();
        assert_eq!(Ledger::read_jsonl(&path).unwrap().events(), more.events());

        // Remove the middle line: the third event's seq 3 now sits at position 2.
        let text = std::fs::read_to_string(&path).unwrap();
        let cut: Vec<&str> = text.lines().filter(|l| !l.contains("\"id\":\"b\"")).collect();
        std::fs::write(&path, cut.join("\n") + "\n").unwrap();
        match Ledger::read_jsonl(&path) {
            Err(ReadError::Renumbered { id, .. }) => assert_eq!(id, "c"),
            other => panic!(
                "a file whose numbering disagrees with its order is refused; got {:?}",
                other.map(|l| l.len())
            ),
        }
    }

    /// **DEPTH IS A WALK OVER `parent_id`.** Root 0, child 1, grandchild 2; a
    /// parent the store cannot find ends the walk. The chain is built the only
    /// way a chain can be built — each link from the link above it — which is
    /// the acyclicity argument in one line of test.
    ///
    /// **THE MUTATION.** Return `Ok(0)` from `depth`.
    ///
    /// Forced red:
    /// ```text
    /// a child is at depth 1
    /// ```
    #[test]
    fn depth_is_derived_from_parent_id() {
        let root = ThreadMeta::root("root", "claude_code", "");
        let child = ThreadMeta::child_of(&root, "child", "");
        let grand = ThreadMeta::child_of(&child, "grand", "");
        let store = vec![root, child, grand];
        let lookup = |id: &str| store.iter().find(|m| m.id == id).cloned();
        assert_eq!(store[0].depth(lookup), Ok(0));
        assert_eq!(store[1].depth(lookup), Ok(1), "a child is at depth 1");
        assert_eq!(store[2].depth(lookup), Ok(2), "a grandchild is at depth 2");
        assert_eq!(store[1].parent_id(), Some("root"));
        assert_eq!(store[0].parent_id(), None, "a root has no parent");

        // An unknown parent ends the walk: the depth counted so far is what is
        // known. Only a deserialized store can present this, because
        // `child_of` requires the parent in hand.
        let orphan: ThreadMeta =
            serde_json::from_value(json!({"id": "orphan", "parent_id": "gone", "runtime": "claude_code", "created_at": ""}))
                .unwrap();
        assert_eq!(orphan.depth(lookup), Ok(1), "an unknown parent ends the walk");
    }

    /// **A CYCLE CANNOT BE CONSTRUCTED, AND A DESERIALIZED ONE IS NAMED.**
    ///
    /// The construction half is the compiler's and is stated where it is
    /// enforced ([`ThreadMeta`]): the field is private, the only setter is
    /// `child_of`, and it takes the parent by reference, so an edge can only
    /// point at something that already exists. There is no runtime assertion
    /// for that — a test of a compile-time property is a probe that cannot
    /// fail, and the scan in `mod.rs` guards the privacy instead.
    ///
    /// **This test is the other door: `serde`.** A store read off disk can hold
    /// anything, so it is the one place a cycle can enter, and it is what makes
    /// the walk's cycle arm reachable at all. The sentinel against a vacuous
    /// pass is the legal 40-deep chain below: a walk that reported *everything*
    /// as a cycle, or that had kept the old `limit` and truncated, fails on it.
    ///
    /// **THE MUTATION.** In `depth`, replace the `seen.insert` guard with
    /// `seen.insert(pid.clone());` (never report).
    ///
    /// Forced red:
    /// ```text
    /// a cycle that only deserialization could build is named, not counted
    /// ```
    #[test]
    fn a_cycle_is_unconstructible_and_a_deserialized_one_is_named() {
        let mk = |id: &str, parent: &str| -> ThreadMeta {
            serde_json::from_value(
                json!({"id": id, "parent_id": parent, "runtime": "claude_code", "created_at": ""}),
            )
            .unwrap()
        };
        // THE SENTINEL: a legitimately deep chain, built the real way, is a
        // depth and never a cycle — and is deeper than any `limit` this walk
        // used to carry, so a cap could not pass this test.
        let mut chain = vec![ThreadMeta::root("t0", "claude_code", "")];
        for i in 1..40 {
            let prev = chain.last().unwrap().clone();
            chain.push(ThreadMeta::child_of(&prev, format!("t{i}"), ""));
        }
        let deep = |id: &str| chain.iter().find(|m| m.id == id).cloned();
        assert_eq!(chain[39].depth(deep), Ok(39), "a legal 39-deep chain is a depth, not a cycle");

        // A two-node cycle, which only `serde` can produce.
        let cyc = vec![mk("a", "b"), mk("b", "a")];
        let lookup = |id: &str| cyc.iter().find(|m| m.id == id).cloned();
        let err = cyc[0]
            .depth(lookup)
            .expect_err("a cycle that only deserialization could build is named, not counted");
        assert_eq!(err.at, "a", "the walk names the link that closed the loop");
        assert_eq!(err.from, "a");
        assert_eq!(err.walked, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            err.to_string(),
            "thread a has no depth: its parent walk returns to a after a -> b \
             — a cycle, which construction cannot build, so the store is corrupt"
        );

        // A one-node self-loop is the tightest case and is caught at step one.
        let me = vec![mk("s", "s")];
        let err = me[0]
            .depth(|id| me.iter().find(|m| m.id == id).cloned())
            .expect_err("a self-parent is a cycle");
        assert_eq!((err.at.as_str(), err.walked.len()), ("s", 1));
    }
}
