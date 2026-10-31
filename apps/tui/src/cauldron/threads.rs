//! **THE THREADS TAB** (8e4fca10 msg 186, task 0167, made it a MODE behind
//! the top-left button; msg 235 made it a TAB; msg 457 called it Agents;
//! the release plan (REL-6) names it Threads again and makes it REAL): the
//! interface he talks to the coordinator through — the harness `tui` chat
//! face — rebuilt inside this face, on the REAL threads daemon.
//!
//! > *"Threads are pinned or recent per environment, with no Projects
//! > grouping. Reuse the `tui --chat` client code."* (REL-6)
//!
//! What it keeps from the chat face: its CLIENT, whole — the seam, the
//! mirror, the chat face's [`crate::store::Store`] and its derivations
//! (`project_list`'s pinned and recent bands, `sort_pinned`, `Store::name_of`,
//! the daemon's status words, `thread_blocks` for a transcript,
//! `managed_rows` for the subagents) — reached through the cauldron face's one
//! connection, [`crate::cauldron::live`]. And its screens: the thread LIST
//! (Pinned, Recent, Archived — no Projects band) and a THREAD (the
//! transcript, the message box, the subagent tree under it).
//!
//! What it changes, for this face's visual system (`look`, msg 186): the
//! selection is a background, never a `▸` marker; his message is the node
//! CARD; the list's sections, a tool call and the subagents are BANDS on the
//! one band line, their counts as metadata segments; thread rows and subagent
//! rows are TABLES; no footer of key hints.
//!
//! **Nothing here holds a copy of the daemon's truth.** [`Threads::refresh`]
//! PROJECTS the store into rows and blocks once per batch; a message sent is
//! sent through the seam and appears when the daemon logs it (TC-15). The
//! daemon down: the rows the mirror last held stay drawn, marked stale, and
//! the message box refuses to send behind [`live::SEND_BLOCKED`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_render::compose::Composer;
use tui_render::palette;

use crate::cauldron::app::{App, Hit};
use crate::cauldron::live::{self, Live};
use crate::cauldron::look::{self, Card, Tone};
use crate::cauldron::view::{display_width, wrap};
use crate::agents::SubagentRow;
use tui_render::model::RowKey;

/// An in-process subagent's state in his words: it has no thread, so no
/// `rolled.display` — the chat face's one renaming (`store::status_word`).
/// No end and no running seen yet is drawn blank, never a word of its own
/// (consumer §8: blank before it lands).
fn direct_word(a: &SubagentRow) -> String {
    crate::store::status_word(a.state).map(str::to_string).unwrap_or_default()
}

// ── the rows, as the store projects them ──────────────────────────────────

/// One row of the thread list, projected from the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub id: String,
    /// `Store::name_of`, else `untitled` — never the id, never a path.
    pub name: String,
    /// The project index the row was read from, or the root its cwd is under.
    pub project: String,
    /// The daemon's own word (`rolled.display` when it said one), verbatim;
    /// empty before it has said anything — never `absent`.
    pub state: String,
    /// The daemon says it, or a descendant, is running.
    pub active: bool,
    pub unread: usize,
    /// Since his last input, as the list prints it.
    pub age: String,
    pub pinned: bool,
    pub archived: bool,
}

/// A subagent of the open thread: a MANAGED child, a thread of its own
/// (`Store::managed_rows`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    /// The subagent that launched it; none for the thread's own.
    pub parent: Option<String>,
    pub label: String,
    /// The daemon's word for it (`rolled.display`), else the child's state.
    pub state: String,
    pub age: String,
    pub tokens: u64,
    pub calls: u64,
}

/// One thing in a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// His message: its id (the thread's message number) and time.
    User { id: String, at: String, text: String },
    /// The assistant's turn: when, and how long it took.
    Assistant { at: String, took: String, text: String },
    /// One tool call (the call and its result merged, DEC-33): its glyph and
    /// what the chat face's projection says of it — one line closed, the
    /// full arguments and result when `open` (REL-6b). `key` is its ledger
    /// row, which is what is opened: the chat face's own `Opened` set.
    Tool { tool: String, text: String, meta: String, key: Option<RowKey>, expandable: bool, open: bool },
    /// A run of rows folded into one (`×n`); open, every row it stands for.
    Group { text: String, count: u64, key: Option<RowKey>, expandable: bool, open: bool },
    /// An in-process (Task) subagent launched here: the chat face's one
    /// tile at its launch row. → shows its own transcript (REL-6b).
    Agent { agent: String, label: String, state: String },
    Warn { text: String },
    /// A line the face adds and no daemon wrote (loading, a window's edge).
    Note { text: String },
    Compacted { text: String },
}

impl Block {
    /// Whether → does anything on it.
    fn opens(&self) -> bool {
        match self {
            Block::Tool { key, expandable, .. } | Block::Group { key, expandable, .. } => key.is_some() && *expandable,
            Block::Agent { .. } => true,
            _ => false,
        }
    }
    /// A lane row: stacked under the turn above, no blank line between.
    fn lane(&self) -> bool {
        matches!(self, Block::Tool { .. } | Block::Group { .. } | Block::Agent { .. } | Block::Warn { .. } | Block::Note { .. })
    }
}

/// The chat face's transcript block as this face draws it. `agents` are the
/// direct (Task) subagents of the thread the store has open: a tile at one's
/// launch row becomes [`Block::Agent`].
fn block_of(b: &tui_render::Block, agents: &[SubagentRow]) -> Block {
    use tui_render::{Block as R, Sender, Severity};
    let at = |ts: &Option<DateTime<Utc>>| ts.map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_default();
    match b {
        R::Message { id, sender: Sender::User, ts, text, .. } => Block::User { id: id.message_id.clone(), at: at(ts), text: text.clone() },
        R::Message { sender: Sender::Assistant, ts, text, .. } => Block::Assistant { at: at(ts), took: String::new(), text: text.clone() },
        R::Lane { severity: Severity::Warn | Severity::Critical, text, .. } => Block::Warn { text: text.clone() },
        R::Lane { key: Some(k), affordance: Some(_), .. } if agents.iter().any(|a| &a.launched_at == k) => {
            let a = agents.iter().find(|a| &a.launched_at == k).expect("just found");
            Block::Agent { agent: a.id.clone(), label: a.label.clone(), state: direct_word(a) }
        }
        R::Lane { text, count: Some(n), key, expandable, open, .. } => Block::Group { text: text.clone(), count: *n, key: key.clone(), expandable: *expandable, open: *open },
        R::Lane { glyph, text, key, expandable, open, .. } => Block::Tool { tool: glyph.to_string(), text: text.clone(), meta: String::new(), key: key.clone(), expandable: *expandable, open: *open },
        R::Rule { text } if text.contains("compact") => Block::Compacted { text: text.clone() },
        R::Rule { text } => Block::Note { text: text.clone() },
    }
}

/// `3m`, `2h`, `5d` since `t`; empty when the daemon has not said.
fn age(t: Option<DateTime<Utc>>) -> String {
    let Some(t) = t else { return String::new() };
    let s = (Utc::now() - t).num_seconds().max(0);
    match s {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// The word a row shows: the daemon's `rolled.display` verbatim when it said
/// one, else the store's shown word; never `absent` or `?` (consumer §8: a
/// state the daemon has not said is blank).
fn word(store: &crate::store::Store, id: &str, shown: &str) -> String {
    if let Some(d) = store.fact(id).and_then(|f| f.rolled.as_ref()).map(|r| r.display.clone()).filter(|d| !d.is_empty()) {
        return d;
    }
    match shown {
        "absent" | "?" => String::new(),
        w => w.to_string(),
    }
}

// ── the state ─────────────────────────────────────────────────────────────

/// Which region of the Threads tab holds the selection (navigation.md rule 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TFocus {
    /// The thread list (no thread open).
    List,
    /// The transcript.
    Chat,
    /// The message box.
    Input,
    /// The subagent tree.
    Agents,
}

/// One selectable row of the thread list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LItem {
    /// A band: `pinned`, `recent` or `archived`.
    Band(String),
    /// A thread, by its id.
    Thread(String),
}

/// What a key asked of the connection; [`settle`] does it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Want {
    Open(String),
    Send(String, String),
}

/// A laid-out page: its lines, and the line range of each selectable thing.
pub struct Laid<T> {
    pub lines: Vec<Line<'static>>,
    pub items: Vec<(std::ops::Range<usize>, T)>,
}

/// The link, as the tab says it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Link {
    /// No connection was started (`why`).
    #[default]
    None,
    Connecting,
    Up,
    /// It was up and is not: what is drawn is what the daemon last said.
    Down,
}

#[derive(Default)]
pub struct Threads {
    /// The list, projected from the store by [`Threads::refresh`].
    pub rows: Vec<Thread>,
    /// The open thread's id; none on the list.
    pub open: Option<String>,
    /// The open thread's row (its name, state and project), projected.
    pub head: Option<Thread>,
    /// Whose transcript is shown: a subagent's (a thread) id, or none for the
    /// thread's own.
    pub subject: Option<String>,
    /// The transcript on screen, projected from the store.
    pub transcript: Vec<Block>,
    /// The open thread's subagents, projected from the store.
    pub agent_rows: Vec<Agent>,
    /// The environment's project root (Q-T1): only threads under it are
    /// listed. None lists every thread.
    pub root: Option<PathBuf>,
    pub link: Link,
    /// Why there is no connection, when there is none.
    pub why_not: Option<String>,
    pub focus: Option<TFocus>,
    /// The selected row of the list.
    pub list_item: usize,
    pub list_scroll: usize,
    /// The selected block of the transcript.
    pub chat_item: usize,
    /// How far the transcript is scrolled UP from its newest line.
    pub chat_scroll: usize,
    /// Scroll the selection into view on the next frame.
    pub reveal: bool,
    /// The list's bands he turned from their default.
    pub bands: HashSet<String>,
    /// Transcript rows he opened in place (a tool call's full arguments and
    /// result): the chat face's own `Opened`, keyed by LEDGER ROW, so an
    /// opened row stays open as the tail grows (REL-6b).
    pub opened: crate::store::Opened,
    /// The open thread's in-process (Task) subagents, the chat face's
    /// `agents::direct_attribute` over the rows the mirror holds — no
    /// subscription of their own: they live in the thread's own log.
    pub direct: Vec<SubagentRow>,
    /// The last direct subagents seen for each thread, so the open thread's
    /// keep their rows while a managed child is the store's open thread.
    /// In memory only, and CAPPED at [`SUBAGENT_CACHE_THREADS`] threads,
    /// least recently used first out (REL-6a2; consumer §11).
    direct_of: HashMap<String, Vec<SubagentRow>>,
    /// `direct_of`'s keys, least recently used first.
    direct_lru: std::collections::VecDeque<String>,
    /// The subagent tree's row (0 its band, 1 the thread, then each agent).
    pub agent_row: usize,
    pub agents_shut: bool,
    /// Each thread's message box, by thread id.
    pub composers: HashMap<String, Composer>,
    /// The message box's text width in the last frame.
    pub input_w: usize,
    /// The first tree row the last frame drew (the band shows a window).
    pub tree_top: usize,
    /// A draft owed to the journal, and since when (coalesced, 100 ms).
    draft_owed: Option<(String, std::time::Instant)>,
    wants: Vec<Want>,
}

/// **THE CAP ON THE FACE'S SUBAGENT CACHE** (REL-6a2, REL-6ab-I F5): how
/// many threads' direct (Task) subagent rows the face keeps in memory.
/// Consumer §11 rules finished messages kept in memory WITH a cap and names
/// no number; 8 is a modest pick of this step's, equal to the seam's
/// `open_keys_per_conn` (the most `threads/log` windows a face holds at once),
/// so the cache never outlives the windows it was derived from by much. The
/// open thread is never evicted. Not an operator ruling: revisit freely.
pub const SUBAGENT_CACHE_THREADS: usize = 8;

/// How long a typed draft waits before it is written (consumer §9: coalesced
/// on a debounce of at most 100 ms).
pub const DRAFT_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(100);

impl Threads {
    pub fn focus(&self) -> TFocus {
        self.focus.unwrap_or(if self.open.is_some() { TFocus::Chat } else { TFocus::List })
    }

    /// Keep `rows` as `id`'s direct subagents, as its most recent use, and
    /// evict the least recently used past [`SUBAGENT_CACHE_THREADS`] — never
    /// the open thread.
    fn remember_direct(&mut self, id: &str, rows: Vec<SubagentRow>) {
        self.direct_of.insert(id.to_string(), rows);
        self.touch_direct(id);
        while self.direct_of.len() > SUBAGENT_CACHE_THREADS {
            let Some(at) = self.direct_lru.iter().position(|k| Some(k.as_str()) != self.open.as_deref()) else { break };
            if let Some(k) = self.direct_lru.remove(at) {
                self.direct_of.remove(&k);
            }
        }
    }

    /// A read or a write of `id`'s entry is a use (most recent last).
    fn touch_direct(&mut self, id: &str) {
        if !self.direct_of.contains_key(id) {
            return;
        }
        if let Some(at) = self.direct_lru.iter().position(|k| k == id) {
            self.direct_lru.remove(at);
        }
        self.direct_lru.push_back(id.to_string());
    }

    /// The threads whose subagent rows the face holds, least recently used
    /// first (for the cap's guard).
    pub fn direct_cached(&self) -> Vec<String> {
        self.direct_lru.iter().cloned().collect()
    }

    /// **PROJECT THE STORE**: the list's rows, the open thread's head, its
    /// transcript and its subagents. Once per batch, never per fact.
    pub fn refresh(&mut self, live: &Live) {
        let store = &live.store;
        self.link = if live.is_up() {
            Link::Up
        } else if live.is_down() {
            Link::Down
        } else if live.is_started() {
            Link::Connecting
        } else {
            Link::None
        };
        self.why_not = live.why_not.clone();
        let rows = project(store, self.root.as_deref());
        self.rows = rows;
        self.head = self.open.as_deref().map(|id| {
            self.rows.iter().find(|r| r.id == id).cloned().unwrap_or_else(|| {
                let shown = store.shown_word(id).unwrap_or_default();
                Thread {
                    id: id.to_string(),
                    name: store.name_of(id).unwrap_or_else(|| crate::store::UNTITLED.to_string()),
                    project: store.project_of(id).unwrap_or_default().to_string(),
                    state: word(store, id, &shown),
                    active: store.fact(id).is_some_and(|f| f.executing) || shown == "active",
                    unread: 0,
                    age: String::new(),
                    pinned: false,
                    archived: false,
                }
            })
        });
        // The direct (Task) subagents of whatever thread the store has open,
        // from the rows the mirror holds (the chat face's attribution).
        let here: Vec<SubagentRow> = match store.open_id() {
            Some(id) => {
                let rows = crate::store::open_rows(store);
                let d = crate::agents::direct_attribute(&rows, Utc::now());
                self.remember_direct(id, d.clone());
                d
            }
            None => Vec::new(),
        };
        if let Some(o) = self.open.clone() {
            self.touch_direct(&o);
        }
        self.direct = self.open.as_deref().and_then(|o| self.direct_of.get(o)).cloned().unwrap_or_default();
        let old = self.transcript.len();
        let was_last = self.chat_item + 1 >= old;
        // A direct subagent's chat is its parent thread's rows seen through
        // the chat face's lens — the store keeps the THREAD open; a managed
        // child is a thread of its own and the store opens it.
        let lens = self.direct_subject();
        let shown = if lens.is_some() { self.open.as_deref() } else { self.subject.as_deref().or(self.open.as_deref()) };
        self.transcript = match store.open_id() {
            Some(id) if Some(id) == shown => {
                let subject = lens.map_or(tui_render::TreeId::Parent, |a| tui_render::TreeId::Agent(a.to_string()));
                crate::store::thread_blocks(store, false, false, &self.opened, &here, &subject, None).iter().map(|b| block_of(b, &here)).collect()
            }
            _ if self.open.is_some() => vec![Block::Note { text: "loading this thread's tail from the daemon…".into() }],
            _ => Vec::new(),
        };
        if was_last {
            self.chat_item = self.transcript.len().saturating_sub(1);
        }
        self.agent_rows = match &self.open {
            Some(id) => store
                .managed_rows(id)
                .into_iter()
                .filter_map(|r| match r.id {
                    tui_render::TreeId::Thread(child) => Some(Agent {
                        state: r.status.clone().unwrap_or_else(|| r.state.word().to_string()),
                        id: child,
                        parent: None,
                        label: r.label,
                        age: age(r.started),
                        tokens: r.tokens.unwrap_or(0),
                        calls: r.calls.unwrap_or(0),
                    }),
                    _ => None,
                })
                .chain(self.direct.iter().map(|a| Agent {
                    id: a.id.clone(),
                    // Nested under another of the thread's own subagents; one
                    // whose parent left the tail is drawn at the first level.
                    parent: a.parent.clone().filter(|p| self.direct.iter().any(|d| &d.id == p)),
                    label: a.label.clone(),
                    state: direct_word(a),
                    age: age(a.started),
                    tokens: a.tokens.unwrap_or(0),
                    calls: a.calls.unwrap_or(0),
                }))
                .collect(),
            None => Vec::new(),
        };
    }

    /// The subject, when it is one of the open thread's in-process (Task)
    /// subagents rather than a managed child thread.
    pub fn direct_subject(&self) -> Option<&str> {
        self.subject.as_deref().filter(|s| self.direct.iter().any(|a| a.id == *s))
    }

    /// Unread across every thread that is not archived.
    pub fn unread(&self) -> usize {
        self.rows.iter().filter(|t| !t.archived).map(|t| t.unread).sum()
    }

    pub fn active(&self) -> usize {
        self.rows.iter().filter(|t| t.active).count()
    }

    /// The thread on screen.
    pub fn thread(&self) -> Option<&Thread> {
        self.head.as_ref()
    }

    pub fn agent(&self, id: &str) -> Option<&Agent> {
        self.agent_rows.iter().find(|a| a.id == id)
    }

    /// The transcript on screen: the subject's.
    pub fn blocks(&self) -> &[Block] {
        &self.transcript
    }

    /// The message box shows only for the thread itself: a subagent takes
    /// no messages.
    pub fn has_input(&self) -> bool {
        self.open.is_some() && self.subject.is_none()
    }

    pub fn composer(&mut self) -> &mut Composer {
        let id = self.open.clone().unwrap_or_default();
        self.composers.entry(id).or_default()
    }

    pub fn composer_ref(&self) -> Option<&Composer> {
        self.open.as_ref().and_then(|i| self.composers.get(i))
    }

    fn band_open(&self, key: &str) -> bool {
        let default = key != "archived";
        default ^ self.bands.contains(key)
    }

    fn set_band(&mut self, key: &str, open: bool) {
        if open == self.band_open(key) {
            return;
        }
        if !self.bands.remove(key) {
            self.bands.insert(key.to_string());
        }
    }

    /// The ledger row block `i` stands for, when it can open.
    fn block_key(&self, i: usize) -> Option<RowKey> {
        match self.blocks().get(i) {
            Some(b @ (Block::Tool { key, .. } | Block::Group { key, .. })) if b.opens() => key.clone(),
            _ => None,
        }
    }

    pub fn block_open(&self, i: usize) -> bool {
        matches!(self.blocks().get(i), Some(Block::Tool { open: true, .. } | Block::Group { open: true, .. }))
    }

    /// Open thread `id`: its transcript at the newest line. The connection
    /// is asked for it by [`settle`].
    pub fn open_thread(&mut self, id: &str) {
        self.open = Some(id.to_string());
        self.subject = None;
        self.transcript.clear();
        self.chat_item = 0;
        self.chat_scroll = 0;
        self.agent_row = 1;
        self.focus = Some(TFocus::Chat);
        self.reveal = true;
        self.wants.push(Want::Open(id.to_string()));
    }

    /// Back to the list, on the row of the thread he left.
    pub fn close_thread(&mut self) {
        let was = self.open.take();
        self.subject = None;
        self.head = None;
        self.transcript.clear();
        self.focus = Some(TFocus::List);
        if let Some(w) = was {
            if let Some(at) = self.list_items().iter().position(|(_, it)| *it == LItem::Thread(w.clone())) {
                self.list_item = at;
            }
        }
        self.reveal = true;
    }

    /// Show `subject`'s transcript (none: the thread's own).
    pub fn set_subject(&mut self, subject: Option<String>) {
        self.subject = subject;
        self.transcript.clear();
        self.chat_item = 0;
        self.chat_scroll = 0;
        self.agent_row = self.tree().iter().position(|r| r.id == self.subject).map_or(1, |p| p + 1);
        self.reveal = true;
        // A Task subagent's chat is in its thread's own log: the store keeps
        // (or goes back to) the thread — no subscription of its own.
        let target = if self.direct_subject().is_some() { self.open.clone() } else { self.subject.clone().or_else(|| self.open.clone()) };
        if let Some(id) = target {
            self.wants.push(Want::Open(id));
        }
    }

    // ── the list ──

    fn list_items(&self) -> Vec<(std::ops::Range<usize>, LItem)> {
        self.list(100).items
    }

    /// The thread list: PINNED, RECENT and ARCHIVED, on the one band line.
    /// No Projects band (REL-6: "with no Projects grouping").
    pub fn list(&self, width: usize) -> Laid<LItem> {
        let mut lines = Vec::new();
        let mut items = Vec::new();
        let threads = &self.rows;
        // What the link is, above the bands, when it is not simply up.
        let said = match (&self.link, threads.is_empty()) {
            (Link::Up, _) => None,
            (Link::Down, _) => Some(("threads is down · showing what it last said".to_string(), palette::amber())),
            (Link::Connecting, true) => Some(("loading threads from the daemon…".to_string(), palette::muted())),
            (Link::Connecting, false) => None,
            (Link::None, _) => Some((format!("no threads daemon · {}", self.why_not.as_deref().unwrap_or("not connected")), palette::muted())),
        };
        if let Some((text, style)) = said {
            lines.push(Line::from(Span::styled(text, style)));
            lines.push(Line::raw(""));
        }
        // One table for every thread row on the page, so their columns line
        // up whatever band they are in.
        let cells: Vec<Vec<String>> = threads.iter().map(|t| thread_cells(t, width)).collect();
        let widths = look::widths(&cells);
        let count = |idx: &[usize]| {
            let unread: usize = idx.iter().map(|&i| threads[i].unread).sum();
            let active = idx.iter().filter(|&&i| threads[i].active).count();
            look::meta([
                look::seg("", idx.len(), if idx.len() == 1 { "thread" } else { "threads" }, Tone::Plain),
                if active > 0 { look::seg("●", active, "active", Tone::Good) } else { Span::raw("") },
                if unread > 0 { look::seg("•", unread, "unread", Tone::Warn) } else { Span::raw("") },
            ])
        };
        let mut band = |lines: &mut Vec<Line<'static>>, key: &str, name: &str, idx: Vec<usize>| {
            if !lines.is_empty() && !lines.last().is_some_and(|l| l.width() == 0) {
                lines.push(Line::raw(""));
            }
            let open = self.band_open(key);
            let at = lines.len();
            lines.push(look::band_line(0, open, vec![look::name(name)], count(&idx), width));
            items.push((at..at + 1, LItem::Band(key.to_string())));
            if open {
                for i in idx {
                    let at = lines.len();
                    lines.push(thread_row(&threads[i], &cells[i], &widths, 1, width));
                    items.push((at..at + 1, LItem::Thread(threads[i].id.clone())));
                }
            }
        };
        let live: Vec<usize> = (0..threads.len()).filter(|&i| !threads[i].archived).collect();
        band(&mut lines, "pinned", "Pinned", live.iter().copied().filter(|&i| threads[i].pinned).collect());
        band(&mut lines, "recent", "Recent", live.iter().copied().filter(|&i| !threads[i].pinned).collect());
        band(&mut lines, "archived", "Archived", (0..threads.len()).filter(|&i| threads[i].archived).collect());
        Laid { lines, items }
    }

    // ── a thread ──

    /// The transcript on screen, laid out to `width`.
    pub fn chat(&self, width: usize) -> Laid<usize> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut items = Vec::new();
        let margin = if width > 80 { 10 } else { 4 };
        for (i, b) in self.blocks().iter().enumerate() {
            if !lines.is_empty() && !b.lane() {
                lines.push(Line::raw(""));
            }
            let at = lines.len();
            match b {
                Block::User { id, at: when, text } => {
                    let cw = width.saturating_sub(margin).max(12);
                    let from = lines.len();
                    lines.push(look::card_top(Card::Node, vec![Span::styled(when.clone(), palette::muted())], vec![Span::styled(format!("#{id}"), palette::ink2())], palette::muted(), cw));
                    let body = lines.len();
                    wrap(&mut lines, text, look::inside(cw), 0, palette::ink());
                    look::card_close(Card::Node, &mut lines, body, palette::muted(), cw);
                    look::indent(&mut lines, from, width.saturating_sub(cw));
                }
                Block::Assistant { at: when, took, text } => {
                    let mut head = vec![Span::styled("claude", palette::ink2().add_modifier(Modifier::BOLD)), Span::styled(format!(" · {when}"), palette::muted())];
                    if !took.is_empty() {
                        head.push(Span::styled(format!(" · {took}"), palette::muted()));
                    }
                    lines.push(Line::from(head));
                    wrap(&mut lines, text, width.saturating_sub(margin).max(20), 0, palette::ink());
                }
                // A tool call: one line closed; open, the chat face's whole
                // rendering of it — the full arguments and the full result —
                // under its line (REL-6b).
                Block::Tool { tool, text, meta, open, .. } => {
                    let first = text.lines().next().unwrap_or_default();
                    // A band carries its metadata (msg 183): an openable
                    // call's is its outcome, the projection's last clause
                    // (`ok`, `failed`, `running`).
                    let (first, outcome) = match (meta.is_empty() && b.opens(), first.rsplit_once(" · ")) {
                        (true, Some((head, last))) => (head, last.to_string()),
                        _ => (first, meta.clone()),
                    };
                    let name = vec![Span::styled(format!("{tool} {first}"), palette::ink2())];
                    let tone = match outcome.as_str() {
                        "failed" | "error" => Tone::Bad,
                        _ => Tone::Plain,
                    };
                    let meta = if outcome.is_empty() { vec![] } else { vec![look::word(outcome, tone)] };
                    lines.push(lane_line(b.opens(), *open, name, meta, width));
                    if *open {
                        wrap(&mut lines, text, width.saturating_sub(10).max(20), 8, palette::muted());
                    }
                }
                Block::Group { text, count, open, .. } => {
                    let first = text.lines().next().unwrap_or_default();
                    let name = vec![Span::styled(format!("◇ {first}"), palette::ink2())];
                    lines.push(lane_line(b.opens(), *open, name, vec![look::seg("", count, "calls", Tone::Plain)], width));
                    if *open {
                        wrap(&mut lines, text, width.saturating_sub(10).max(20), 8, palette::muted());
                    }
                }
                Block::Agent { label, state, .. } => {
                    let name = vec![Span::styled(format!("◆ {label}"), palette::ink2())];
                    lines.push(lane_line(true, false, name, vec![state_seg(state)], width));
                }
                Block::Warn { text } => lines.push(Line::from(vec![Span::raw("    "), Span::styled(format!("⚠ {text}"), palette::amber())])),
                Block::Note { text } => lines.push(Line::from(vec![Span::raw("    "), Span::styled(format!("◇ {text}"), palette::muted())])),
                // No `── TITLE ──` rule anywhere (msg 235: "Eradicate it.
                // From the entire application"): a compaction is a muted
                // line of its own, as a note is.
                Block::Compacted { text } => lines.push(Line::from(vec![Span::raw("    "), Span::styled(format!("⋯ {text}"), palette::muted())])),
            }
            items.push((at..lines.len(), i));
        }
        Laid { lines, items }
    }

    /// The subagent tree: the thread itself, then every subagent depth
    /// first under the one that launched it.
    pub fn tree(&self) -> Vec<TreeRow> {
        if self.thread().is_none() || self.agent_rows.is_empty() {
            return Vec::new();
        }
        let mut out = vec![TreeRow { id: None, depth: 0, last: false, rails: String::new() }];
        fn walk(t: &[Agent], parent: Option<&str>, depth: usize, rails: &str, out: &mut Vec<TreeRow>) {
            let kids: Vec<&Agent> = t.iter().filter(|a| a.parent.as_deref() == parent).collect();
            for (k, a) in kids.iter().enumerate() {
                let last = k + 1 == kids.len();
                out.push(TreeRow { id: Some(a.id.clone()), depth, last, rails: rails.to_string() });
                let next = format!("{rails}{}", if last { "  " } else { "│ " });
                walk(t, Some(&a.id), depth + 1, &next, out);
            }
        }
        walk(&self.agent_rows, None, 1, "", &mut out);
        out
    }

    /// The subagent band: its line, then (open) a row per tree row.
    pub fn agents(&self, width: usize) -> Vec<Line<'static>> {
        let Some(t) = self.thread() else { return Vec::new() };
        let tree = self.tree();
        if tree.is_empty() {
            return Vec::new();
        }
        // Counted off the daemon's words: `active…` runs, an error failed.
        let running = self.agent_rows.iter().filter(|a| a.state.starts_with("active") || a.state == "running").count();
        let failed = self.agent_rows.iter().filter(|a| a.state.starts_with("error") || a.state == "failed").count();
        let rest = self.agent_rows.len() - running - failed;
        let meta = look::meta([
            look::seg("▶", running, "running", if running > 0 { Tone::Good } else { Tone::Plain }),
            look::seg("✓", rest, "idle", Tone::Plain),
            if failed > 0 { look::seg("✕", failed, "failed", Tone::Bad) } else { Span::raw("") },
        ]);
        let open = !self.agents_shut;
        let mut lines = vec![look::band_line(0, open, vec![look::name("Subagents")], meta, width)];
        if !open {
            return lines;
        }
        let cells: Vec<Vec<String>> = tree
            .iter()
            .map(|r| match &r.id {
                None => vec![crate::cauldron::view::crop(&t.name, 44), "this thread".into(), String::new(), String::new(), String::new()],
                Some(id) => {
                    let a = self.agent(id).expect("in the tree");
                    vec![
                        format!("{}{}{}", r.rails, if r.last { "└ " } else { "├ " }, crate::cauldron::view::crop(&a.label, 40)),
                        a.state.clone(),
                        a.age.clone(),
                        format!("{} tok", tokens(a.tokens)),
                        format!("{} calls", a.calls),
                    ]
                }
            })
            .collect();
        let widths = look::widths(&cells);
        for (r, c) in tree.iter().zip(&cells) {
            let subject = r.id == self.subject;
            let label_style = if subject { palette::ink().add_modifier(Modifier::BOLD) } else { palette::ink2() };
            let state = r.id.as_ref().and_then(|id| self.agent(id)).map(|a| a.state.as_str()).unwrap_or("");
            let mut spans = vec![Span::raw("  "), Span::styled(look::cell(&c[0], widths[0] + 2), label_style)];
            let st = match state {
                s if s.starts_with("active") || s == "running" => look::tone(Tone::Good),
                s if s.starts_with("error") || s == "failed" => look::tone(Tone::Bad),
                _ => palette::muted(),
            };
            spans.push(Span::styled(look::cell(&c[1], widths[1] + 2), if r.id.is_none() { palette::muted() } else { st }));
            for (k, s) in c.iter().enumerate().skip(2) {
                spans.push(Span::styled(look::cell(s, widths[k] + 2), palette::muted()));
            }
            lines.push(Line::from(look::fit_spans(spans, width)));
        }
        lines
    }

    // ── what the header says ──

    /// A thread's first line: its name (and whose transcript, under a
    /// subagent), its state and its project; on the list, the whole list's
    /// counts.
    pub fn header(&self) -> Vec<Span<'static>> {
        let sep = || Span::styled(" · ", palette::muted());
        let elsewhere = self.unread();
        match self.thread() {
            Some(t) => {
                let mut s = vec![Span::styled(t.name.clone(), palette::ink().add_modifier(Modifier::BOLD))];
                if let Some(a) = self.subject.as_deref().and_then(|id| self.agent(id)) {
                    s.push(Span::styled(" › ", palette::muted()));
                    s.push(Span::styled(a.label.clone(), palette::ink2()));
                }
                if !t.state.is_empty() {
                    s.push(sep());
                    s.push(if t.active { Span::styled(t.state.clone(), palette::code()) } else { Span::styled(t.state.clone(), palette::muted()) });
                }
                if !t.project.is_empty() {
                    s.push(sep());
                    s.push(Span::styled(t.project.clone(), palette::muted()));
                }
                if self.link == Link::Down {
                    s.push(sep());
                    s.push(Span::styled("threads is down · showing what it last said", palette::amber()));
                }
                if elsewhere > 0 {
                    s.push(sep());
                    s.push(Span::styled(format!("{elsewhere} unread elsewhere"), palette::amber()));
                }
                s
            }
            None => vec![
                Span::styled(format!("{elsewhere} unread"), if elsewhere > 0 { palette::amber() } else { palette::muted() }),
                sep(),
                Span::styled(format!("{} active", self.active()), if self.active() > 0 { palette::code() } else { palette::muted() }),
            ],
        }
    }

    /// **SEND the message box's text into the thread** — through the seam's
    /// `send`, by [`settle`]; never a local echo. The message appears when
    /// the daemon logs it, and the box empties only when the daemon accepted
    /// it (an optimistic clear is a box that eats his words).
    pub fn send(&mut self) {
        let Some(id) = self.open.clone() else { return };
        let text = self.composer().text();
        if text.trim().is_empty() {
            return;
        }
        self.wants.push(Want::Send(id, text.trim_end().to_string()));
    }
}

impl Threads {
    /// The open thread's box changed: its draft is owed to the journal.
    pub fn owe_draft(&mut self) {
        if let Some(id) = self.open.clone() {
            self.draft_owed = Some((id, std::time::Instant::now()));
        }
    }
}

// ── the connection ────────────────────────────────────────────────────────

/// **DO WHAT THE KEYS ASKED OF THE CONNECTION, THEN PROJECT**: open a
/// transcript, issue a send, settle the sends the daemon answered (the box
/// empties on an ACCEPTED send only, and a refusal keeps his words), and
/// project the store once.
pub fn settle(app: &mut App) {
    for w in std::mem::take(&mut app.threads.wants) {
        match w {
            Want::Open(id) => {
                app.live.open(&id);
                // A draft he left in the journal comes back into an empty box,
                // without asking (consumer §9); it is never sent by itself.
                if app.live.drafts && app.threads.composers.get(&id).map_or(true, |c| c.text().is_empty()) {
                    if let Some((text, _)) = crate::draft::load(&id) {
                        app.threads.composers.insert(id.clone(), Composer::from_text(&text));
                    }
                }
            }
            Want::Send(id, text) => match app.live.send(&id, &text) {
                Ok(()) => {
                    app.error = None;
                    app.notice = Some("sending…".into());
                }
                Err(why) => {
                    app.notice = None;
                    app.error = Some(why);
                }
            },
        }
    }
    for (id, result) in app.live.take_sent() {
        match result {
            Ok(_) => {
                app.threads.composers.insert(id.clone(), Composer::new());
                if app.threads.draft_owed.as_ref().is_some_and(|(d, _)| *d == id) {
                    app.threads.draft_owed = None;
                }
                if app.live.drafts {
                    crate::draft::clear(&id);
                }
                app.error = None;
                app.notice = Some("sent".into());
            }
            Err(why) => {
                app.notice = None;
                app.error = Some(format!("not sent: {why}"));
            }
        }
    }
    app.threads.refresh(&app.live);
    // The link is back: the refusal it caused is no longer true.
    if app.threads.link == Link::Up && app.error.as_deref() == Some(live::SEND_BLOCKED) {
        app.error = None;
    }
}

/// **ONE TICK**: drain the connection (never waiting on it), write an owed
/// draft, and project once if anything arrived. Returns how many events were
/// applied — non-zero means a frame is owed.
pub fn tick(app: &mut App) -> usize {
    let n = app.live.drain();
    flush_draft(app, false);
    if n > 0 {
        settle(app);
    }
    n
}

/// Write the owed draft once it has waited [`DRAFT_DEBOUNCE`] (or `now`).
pub fn flush_draft(app: &mut App, now: bool) {
    let Some((id, at)) = app.threads.draft_owed.clone() else { return };
    if !now && at.elapsed() < DRAFT_DEBOUNCE {
        return;
    }
    app.threads.draft_owed = None;
    if app.live.drafts {
        let text = app.threads.composers.get(&id).map(|c| c.text()).unwrap_or_default();
        crate::draft::save(&id, &text);
    }
}

/// A row of the subagent tree: the thread (`id` none) or a subagent, with
/// the rails drawn left of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub id: Option<String>,
    pub depth: usize,
    pub last: bool,
    pub rails: String,
}

/// `312k`, `1.2M`.
fn tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{}k", n / 1_000),
        n => n.to_string(),
    }
}

fn state_seg(state: &str) -> Span<'static> {
    match state {
        "running" => look::seg("▶", "running", "", Tone::Good),
        "failed" => look::seg("✕", "failed", "", Tone::Bad),
        "done" => look::seg("✓", "done", "", Tone::Plain),
        s => look::word(s.to_string(), Tone::Plain),
    }
}

/// A lane row: four columns in, a chevron if it opens, metadata flush right.
fn lane_line(opens: bool, open: bool, name: Vec<Span<'static>>, meta: Vec<Span<'static>>, width: usize) -> Line<'static> {
    if opens {
        return look::band_line(2, open, name, meta, width);
    }
    let mut left = vec![Span::raw("      ")];
    left.extend(name);
    look::lr(left, meta, width)
}

/// A thread's row cells: name, project, state, unread, age.
fn thread_cells(t: &Thread, width: usize) -> Vec<String> {
    let name_w = width.saturating_sub(50).clamp(16, 56);
    vec![
        crate::cauldron::view::crop(&t.name, name_w),
        t.project.clone(),
        if t.active { format!("● {}", t.state) } else { t.state.clone() },
        if t.unread > 0 { format!("• {}", t.unread) } else { String::new() },
        t.age.clone(),
    ]
}

fn thread_row(t: &Thread, c: &[String], w: &[usize], depth: usize, width: usize) -> Line<'static> {
    let mut spans = vec![Span::raw("  ".repeat(depth + 1)), Span::styled(look::cell(&c[0], w[0] + 2), palette::ink())];
    spans.push(Span::styled(look::cell(&c[1], w[1] + 2), palette::muted()));
    spans.push(Span::styled(look::cell(&c[2], w[2] + 2), if t.active { palette::code() } else { palette::muted() }));
    spans.push(Span::styled(look::cell(&c[3], w[3] + 2), palette::amber()));
    spans.push(Span::styled(format!("{:>width$}", c[4], width = w[4]), palette::muted()));
    Line::from(look::fit_spans(spans, width))
}

/// **THE LIST, PROJECTED FROM THE CHAT FACE'S STORE** — its own bands, not a
/// second derivation: `project_list` (the index drives, the daemon
/// decorates; `sort_pinned`'s order; recent newest-first on his last input,
/// with the daemon's own recent threads no index carries), each band taken
/// whole (its `see all` page). A pinned thread is listed once, under Pinned.
/// ARCHIVED is the closed threads of the indexes.
///
/// `root` (Q-T1, "per environment"): only threads whose project root or
/// working directory is under it. A FACE-SIDE filter over rows the face
/// already holds — no shape changes.
pub fn project(store: &crate::store::Store, root: Option<&std::path::Path>) -> Vec<Thread> {
    use tui_render::{BandId, Known};
    let under = |id: &str, project: &str| -> bool {
        let Some(root) = root else { return true };
        let in_root = store.projects().iter().any(|p| p.name == project && p.root.starts_with(root));
        let cwd = store.fact(id).map(|f| f.cwd.as_str()).unwrap_or("");
        in_root || (!cwd.is_empty() && std::path::Path::new(cwd).starts_with(root))
    };
    let row = |r: &tui_render::ThreadRow, pinned: bool, archived: bool| -> Thread {
        let state = word(store, &r.id, &r.state);
        Thread {
            id: r.id.clone(),
            name: if r.summary.trim().is_empty() { crate::store::UNTITLED.to_string() } else { r.summary.clone() },
            project: r.project.clone(),
            active: r.executing || r.state == "active",
            state,
            unread: match r.unread {
                Known::Known(n) => n as usize,
                Known::Unknown => 0,
            },
            age: age(r.updated),
            pinned,
            archived,
        }
    };
    let pinned = crate::store::project_list(store, 0, 0, Some(BandId::Pinned)).pinned;
    let recent = crate::store::project_list(store, 0, 0, Some(BandId::Recent)).recent;
    let mut out: Vec<Thread> = Vec::new();
    for r in &pinned {
        if !r.closed && under(&r.id, &r.project) {
            out.push(row(r, true, false));
        }
    }
    for r in &recent {
        if !out.iter().any(|t| t.id == r.id) && under(&r.id, &r.project) {
            out.push(row(r, false, false));
        }
    }
    // The closed threads, each once, newest input first.
    let mut closed: Vec<(String, String, Option<DateTime<Utc>>)> = Vec::new();
    for p in store.projects() {
        for t in p.threads.iter().filter(|t| t.closed) {
            if !closed.iter().any(|(id, _, _)| id == &t.id) && under(&t.id, &p.name) {
                closed.push((t.id.clone(), p.name.clone(), t.last_user_input_at));
            }
        }
    }
    closed.sort_by_key(|c| std::cmp::Reverse(c.2));
    for (id, project, at) in closed {
        let shown = store.shown_word(&id).unwrap_or_default();
        out.push(Thread {
            name: store.name_of(&id).unwrap_or_else(|| crate::store::UNTITLED.to_string()),
            project,
            active: store.fact(&id).is_some_and(|f| f.executing) || shown == "active",
            state: word(store, &id, &shown),
            unread: 0,
            age: age(at),
            pinned: false,
            archived: true,
            id,
        });
    }
    out
}

// ── drawing ───────────────────────────────────────────────────────────────

/// The message box's rows: one at least, then as many as it holds, to 8.
const INPUT_MAX: usize = 8;
/// The subagent band shows at most this many tree rows.
const TREE_MAX: usize = 6;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.threads.open.is_some() {
        draw_thread(frame, app, area);
    } else {
        draw_list(frame, app, area);
    }
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let area = Rect { x: area.x + 1, width: area.width.saturating_sub(2), ..area };
    let width = area.width as usize;
    let laid = app.threads.list(width);
    let t = &mut app.threads;
    t.list_item = t.list_item.min(laid.items.len().saturating_sub(1));
    let h = area.height as usize;
    if t.reveal {
        if let Some((r, _)) = laid.items.get(t.list_item) {
            if r.start < t.list_scroll {
                t.list_scroll = r.start;
            } else if r.end > t.list_scroll + h {
                t.list_scroll = r.end.saturating_sub(h);
            }
        }
        t.reveal = false;
    }
    t.list_scroll = t.list_scroll.min(laid.lines.len().saturating_sub(h));
    let top = t.list_scroll;
    let sel = laid.items.get(t.list_item).map(|(r, _)| r.clone());
    for (i, (r, _)) in laid.items.iter().enumerate() {
        for row in r.clone() {
            if row >= top && row < top + h {
                app.hits.push((Rect { x: area.x, y: area.y + (row - top) as u16, width: area.width, height: 1 }, Hit::TList(i as u16)));
            }
        }
    }
    frame.render_widget(Paragraph::new(laid.lines).scroll((top as u16, 0)).style(palette::base()), area);
    if let Some(r) = sel {
        paint(frame, area, r, top, true);
    }
}

fn paint(frame: &mut Frame, area: Rect, r: std::ops::Range<usize>, top: usize, focused: bool) {
    let h = area.height as usize;
    let from = r.start.max(top);
    let to = r.end.min(top + h);
    if from < to {
        look::paint(frame.buffer_mut(), Rect { y: area.y + (from - top) as u16, height: (to - from) as u16, ..area }, look::sel_bg(focused));
    }
}

fn draw_thread(frame: &mut Frame, app: &mut App, area: Rect) {
    let area = Rect { x: area.x + 1, width: area.width.saturating_sub(2), ..area };
    let width = area.width as usize;
    // Its first line: which thread this is.
    let head = look::fit_spans(app.threads.header(), width);
    frame.render_widget(Paragraph::new(Line::from(head)).style(palette::base()), Rect { height: 1, ..area });
    let focus = app.threads.focus();
    // The subagent band, under the message box (the chat face's order).
    let mut tree = app.threads.agents(width);
    let tree_rows = app.threads.tree().len();
    if tree.len() > 1 + TREE_MAX {
        // A window over the tree that keeps the cursor's row in it.
        let cur = app.threads.agent_row.saturating_sub(1).min(tree_rows.saturating_sub(1));
        let start = cur.saturating_sub(TREE_MAX - 1).min(tree_rows - TREE_MAX);
        let head = tree.remove(0);
        tree = std::iter::once(head).chain(tree.into_iter().skip(start).take(TREE_MAX)).collect();
        app.threads.tree_top = start;
    } else {
        app.threads.tree_top = 0;
    }
    let tree_h = tree.len() as u16;
    let input_w = width.saturating_sub(2).max(1);
    let input_rows = if app.threads.has_input() {
        app.threads.composer_ref().map_or(1, |c| c.visual_rows(input_w)).clamp(1, INPUT_MAX) as u16
    } else {
        0
    };
    let input_h = if app.threads.has_input() { input_rows + 1 } else { 0 };
    let gap = u16::from(tree_h > 0);
    let chat_h = area.height.saturating_sub(input_h + tree_h + gap + 1);
    let chat = Rect { y: area.y + 1, height: chat_h, ..area };
    let input = Rect { y: chat.bottom(), height: input_h, ..area };
    let tree_area = Rect { y: input.bottom() + gap, height: tree_h, ..area };

    // ── the transcript, anchored at its newest line ──
    let laid = app.threads.chat(width);
    let t = &mut app.threads;
    let n = laid.items.len();
    t.chat_item = t.chat_item.min(n.saturating_sub(1));
    let h = chat.height as usize;
    let total = laid.lines.len();
    let max_scroll = total.saturating_sub(h);
    if t.reveal {
        if let Some((r, _)) = laid.items.get(t.chat_item) {
            // `chat_scroll` counts up from the bottom.
            let top = max_scroll.saturating_sub(t.chat_scroll);
            if r.start < top {
                t.chat_scroll = max_scroll.saturating_sub(r.start);
            } else if r.end > top + h {
                t.chat_scroll = max_scroll.saturating_sub(r.end.saturating_sub(h).min(r.start));
            }
        }
        t.reveal = false;
    }
    t.chat_scroll = t.chat_scroll.min(max_scroll);
    let top = max_scroll - t.chat_scroll;
    // Short of a screen, the transcript sits on the message box.
    let pad = h.saturating_sub(total) as u16;
    let body = Rect { y: chat.y + pad, height: chat.height - pad, ..chat };
    for (i, (r, _)) in laid.items.iter().enumerate() {
        for row in r.clone() {
            if row >= top && row < top + h {
                app.hits.push((Rect { x: body.x, y: body.y + (row - top) as u16, width: body.width, height: 1 }, Hit::TChat(i as u16)));
            }
        }
    }
    let sel = laid.items.get(app.threads.chat_item).map(|(r, _)| r.clone());
    frame.render_widget(Paragraph::new(laid.lines).scroll((top as u16, 0)).style(palette::base()), body);
    if let (Some(r), true) = (sel, n > 0) {
        paint(frame, body, r, top, focus == TFocus::Chat);
    }

    // ── the message box ──
    if app.threads.has_input() {
        app.hits.push((input, Hit::TInput));
        let focused = focus == TFocus::Input && app.modal.is_none();
        let name = app.threads.thread().map(|t| t.name.clone()).unwrap_or_default();
        let left = format!("Message · {name} ");
        let fill = width.saturating_sub(display_width(&left));
        // While the link is not up the box still takes keys, and says it
        // cannot send (consumer §9).
        let blocked = app.threads.link != Link::Up;
        let (said, said_style) = match (&app.error, &app.notice) {
            (Some(e), _) => (format!(" {e} "), palette::red()),
            (None, _) if blocked => (format!(" {} ", live::SEND_BLOCKED), palette::amber()),
            (None, Some(n)) => (format!(" {n} "), palette::green()),
            _ => (String::new(), palette::muted()),
        };
        // Its name on the left and what was last said on the right, on THE
        // strip's background — never a `── TITLE ──` rule (msg 235).
        let gap = fill.saturating_sub(display_width(&said));
        let bg = palette::c().strip_bg;
        let head = Line::from(vec![
            Span::styled(left, if focused { palette::amber().add_modifier(Modifier::BOLD) } else { palette::ink2() }.bg(bg)),
            Span::styled(" ".repeat(gap), Style::new().bg(bg)),
            Span::styled(said, said_style.bg(bg)),
        ]);
        frame.render_widget(Paragraph::new(head).style(palette::base()), Rect { height: 1, ..input });
        let text_area = Rect { x: input.x + 2, y: input.y + 1, width: input.width.saturating_sub(2), height: input_rows };
        let tw = text_area.width.max(1) as usize;
        app.threads.input_w = tw;
        let c = app.threads.composer();
        let layout = c.layout(tw);
        let (crow, ccol) = c.caret(tw);
        let rows = input_rows as usize;
        let top = crow.saturating_sub(rows.saturating_sub(1));
        let lines: Vec<Line> = layout.iter().skip(top).take(rows).map(|r| Line::styled(c.row_text(*r).to_string(), palette::ink())).collect();
        frame.render_widget(Paragraph::new(lines).style(palette::base()), text_area);
        if focused {
            frame.set_cursor_position(Position { x: text_area.x + ccol as u16, y: text_area.y + (crow - top) as u16 });
        }
    }

    // ── the subagents ──
    if tree_h > 0 {
        let start = app.threads.tree_top;
        for i in 0..tree_h as usize {
            // Row 0 is the band; row k (k ≥ 1) is tree row start + k - 1.
            let row = if i == 0 { 0 } else { start + i };
            app.hits.push((Rect { y: tree_area.y + i as u16, height: 1, ..tree_area }, Hit::TAgent(row as u16)));
        }
        frame.render_widget(Paragraph::new(tree).style(palette::base()), tree_area);
        let on = app.threads.agent_row;
        let shown = if on == 0 { Some(0) } else { on.checked_sub(start).filter(|k| *k >= 1 && *k < tree_h as usize) };
        if let Some(k) = shown {
            paint(frame, tree_area, k..k + 1, 0, focus == TFocus::Agents);
        }
    }
}

// ── keys ──────────────────────────────────────────────────────────────────

/// Keys on the Threads tab, when no modal is open (navigation.md, Threads).
pub fn key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let bare = !ctrl && !alt && !key.modifiers.contains(KeyModifiers::SUPER);
    let focus = app.threads.focus();
    if focus == TFocus::Input && app.threads.has_input() {
        input_key(app, key, ctrl, alt);
        app.threads.owe_draft();
        return settle(app);
    }
    let code = match key.code {
        KeyCode::Char('p') if ctrl => KeyCode::Up,
        KeyCode::Char('n') if ctrl => KeyCode::Down,
        c => c,
    };
    match code {
        KeyCode::Char('/' | '7' | '_') if ctrl => return app.open_palette(),
        KeyCode::Char('?') => return app.open_palette(),
        KeyCode::Char('q') if bare => {
            app.quit = true;
            return;
        }
        _ => {}
    }
    let t = &mut app.threads;
    match focus {
        TFocus::List => list_key(t, code, alt),
        TFocus::Chat => chat_key(t, code, alt, bare),
        TFocus::Agents => agents_key(t, code),
        TFocus::Input => {}
    }
    settle(app);
}

fn list_key(t: &mut Threads, code: KeyCode, alt: bool) {
    let items = t.list_items();
    let n = items.len();
    let cur = items.get(t.list_item).map(|(_, it)| it.clone());
    t.reveal = true;
    match code {
        KeyCode::Up if alt => t.list_item = section(&items, t.list_item, -1),
        KeyCode::Down if alt => t.list_item = section(&items, t.list_item, 1),
        KeyCode::Up => t.list_item = t.list_item.saturating_sub(1),
        KeyCode::Down => t.list_item = (t.list_item + 1).min(n.saturating_sub(1)),
        KeyCode::Home => t.list_item = 0,
        KeyCode::End => t.list_item = n.saturating_sub(1),
        KeyCode::PageUp => t.list_scroll = t.list_scroll.saturating_sub(10),
        KeyCode::PageDown => t.list_scroll += 10,
        KeyCode::Right | KeyCode::Enter => match cur {
            Some(LItem::Thread(id)) => t.open_thread(&id),
            // A closed band opens where it is; on an open one, → steps to
            // its first row (rule 4).
            Some(LItem::Band(k)) if !t.band_open(&k) => t.set_band(&k, true),
            Some(LItem::Band(_)) => t.list_item = (t.list_item + 1).min(n.saturating_sub(1)),
            None => {}
        },
        KeyCode::Left => match cur {
            Some(LItem::Band(k)) if t.band_open(&k) => t.set_band(&k, false),
            // A row (or a band inside a band) climbs to the band it is in.
            _ => {
                if let Some(up) = parent_band(&items, t.list_item) {
                    t.list_item = up;
                }
            }
        },
        _ => {}
    }
}

/// The band row above `at` whose line is less indented — the band `at` is in.
fn parent_band(items: &[(std::ops::Range<usize>, LItem)], at: usize) -> Option<usize> {
    let depth = |it: &LItem| match it {
        LItem::Band(_) => 0,
        LItem::Thread(_) => usize::MAX,
    };
    let mine = items.get(at).map(|(_, it)| depth(it))?;
    (0..at).rev().find(|&i| match &items[i].1 {
        LItem::Band(_) => mine == usize::MAX || depth(&items[i].1) < mine,
        LItem::Thread(_) => false,
    })
}

/// The next (or previous) top-level band from `at` (⌥↓ / ⌥↑).
fn section(items: &[(std::ops::Range<usize>, LItem)], at: usize, d: isize) -> usize {
    let top = |i: usize| matches!(&items[i].1, LItem::Band(_));
    if d < 0 {
        (0..at).rev().find(|&i| top(i)).unwrap_or(at)
    } else {
        (at + 1..items.len()).find(|&i| top(i)).unwrap_or(at)
    }
}

fn chat_key(t: &mut Threads, code: KeyCode, alt: bool, bare: bool) {
    let n = t.blocks().len();
    t.reveal = true;
    match code {
        // ⌥↑ / ⌥↓: the previous / next message he sent (rule 6).
        KeyCode::Up | KeyCode::Down if alt => {
            let mine = |i: usize| matches!(t.blocks().get(i), Some(Block::User { .. }));
            let at = t.chat_item;
            let to = if code == KeyCode::Up { (0..at).rev().find(|&i| mine(i)) } else { (at + 1..n).find(|&i| mine(i)) };
            if let Some(i) = to {
                t.chat_item = i;
            }
        }
        KeyCode::Up => t.chat_item = t.chat_item.saturating_sub(1),
        // Past the newest block: on into the message box, or the subagents.
        KeyCode::Down if t.chat_item + 1 >= n => {
            if t.has_input() {
                t.focus = Some(TFocus::Input);
            } else if !t.tree().is_empty() {
                t.focus = Some(TFocus::Agents);
            }
        }
        KeyCode::Down => t.chat_item += 1,
        KeyCode::Home => t.chat_item = 0,
        KeyCode::End => {
            t.chat_item = n.saturating_sub(1);
            t.chat_scroll = 0;
        }
        KeyCode::PageUp => {
            t.chat_scroll += 10;
            t.reveal = false;
        }
        KeyCode::PageDown => {
            t.chat_scroll = t.chat_scroll.saturating_sub(10);
            t.reveal = false;
        }
        KeyCode::Right | KeyCode::Enter => open_block(t),
        // ← closes what is open, else climbs: a subagent to its thread, a
        // thread to the list.
        KeyCode::Left => {
            let k = t.block_key(t.chat_item).filter(|_| t.block_open(t.chat_item));
            if !k.is_some_and(|k| t.opened.remove(&k)) {
                if t.subject.is_some() {
                    t.set_subject(None);
                } else {
                    t.close_thread();
                }
            }
        }
        // Esc (and `a`) give the message box the selection (rule 7).
        KeyCode::Esc if t.has_input() => t.focus = Some(TFocus::Input),
        KeyCode::Char('a') if bare && t.has_input() => t.focus = Some(TFocus::Input),
        _ => {}
    }
}

fn open_block(t: &mut Threads) {
    let i = t.chat_item;
    let Some(b) = t.blocks().get(i).cloned() else { return };
    match b {
        Block::Agent { agent, .. } => t.set_subject(Some(agent)),
        b if b.opens() => {
            if let Some(k) = t.block_key(i) {
                if !t.opened.remove(&k) {
                    t.opened.insert(k);
                }
            }
        }
        _ => {}
    }
}

fn agents_key(t: &mut Threads, code: KeyCode) {
    let rows = t.tree().len();
    match code {
        KeyCode::Up if t.agent_row == 0 => t.focus = Some(if t.has_input() { TFocus::Input } else { TFocus::Chat }),
        KeyCode::Up => t.agent_row -= 1,
        KeyCode::Down if !t.agents_shut => t.agent_row = (t.agent_row + 1).min(rows),
        KeyCode::Home => t.agent_row = 0,
        KeyCode::End if !t.agents_shut => t.agent_row = rows,
        KeyCode::Right | KeyCode::Enter => {
            if t.agent_row == 0 {
                if t.agents_shut {
                    t.agents_shut = false;
                } else {
                    t.agent_row = 1;
                }
            } else if let Some(r) = t.tree().get(t.agent_row - 1) {
                let id = r.id.clone();
                t.set_subject(id);
            }
        }
        KeyCode::Left => {
            if t.agent_row == 0 {
                t.agents_shut = true;
            } else {
                t.agent_row = 0;
            }
        }
        KeyCode::Esc if t.has_input() => t.focus = Some(TFocus::Input),
        _ => {}
    }
}

/// The message box: the harness composer's grammar (navigation.md rule 5),
/// without the bucket's notes and chips. Enter sends.
fn input_key(app: &mut App, key: KeyEvent, ctrl: bool, alt: bool) {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let t = &mut app.threads;
    let w = t.input_w.max(1);
    let has_tree = !t.tree().is_empty();
    let c = t.composer();
    match key.code {
        KeyCode::Enter if alt || shift => c.newline(),
        KeyCode::Char('j') if ctrl => c.newline(),
        KeyCode::Enter => t.send(),
        KeyCode::Esc => t.focus = Some(TFocus::Chat),
        KeyCode::Left if !ctrl && !alt && c.cursor().line == 0 && c.cursor().byte == 0 => t.focus = Some(TFocus::Chat),
        KeyCode::Left if alt => c.word_left(),
        KeyCode::Right if alt => c.word_right(),
        KeyCode::Left if ctrl => c.row_start(w),
        KeyCode::Right if ctrl => c.row_end(w),
        KeyCode::Left => c.left(),
        KeyCode::Right => c.right(),
        KeyCode::Up if alt => {
            c.para_up();
        }
        KeyCode::Down if alt => {
            c.para_down();
        }
        KeyCode::Up if ctrl => c.buffer_start(),
        KeyCode::Down if ctrl => c.buffer_end(),
        KeyCode::Up => {
            if !c.up(w) {
                t.focus = Some(TFocus::Chat);
                t.reveal = true;
            }
        }
        KeyCode::Down => {
            if !c.down(w) && has_tree {
                t.focus = Some(TFocus::Agents);
            }
        }
        KeyCode::Home if alt => c.buffer_start(),
        KeyCode::End if alt => c.buffer_end(),
        KeyCode::Home => c.row_start(w),
        KeyCode::End => c.row_end(w),
        KeyCode::Char('a') if ctrl => c.row_start(w),
        KeyCode::Char('e') if ctrl => c.row_end(w),
        KeyCode::Backspace if alt => c.kill_word_back(),
        KeyCode::Backspace => c.backspace(),
        KeyCode::Char('h') if ctrl => c.backspace(),
        KeyCode::Delete => c.delete(),
        KeyCode::Char('d') if ctrl => c.delete(),
        KeyCode::Char('k') if ctrl => c.kill_to_line_end(),
        KeyCode::Char('u') if ctrl => c.kill_to_line_start(),
        KeyCode::Char('w') if ctrl => c.kill_word_back(),
        KeyCode::Char('y') if ctrl => c.yank(),
        KeyCode::Char('z') if ctrl => {
            c.undo();
        }
        KeyCode::Char('r') if ctrl => {
            c.redo();
        }
        // ^P / ^N scroll the transcript one row, as in the harness.
        KeyCode::Char('p') if ctrl => t.chat_scroll += 1,
        KeyCode::Char('n') if ctrl => t.chat_scroll = t.chat_scroll.saturating_sub(1),
        KeyCode::Char('/' | '7' | '_') if ctrl => app.open_palette(),
        KeyCode::Tab => c.insert_paste("    "),
        KeyCode::Char(ch) if !ctrl && !key.modifiers.contains(KeyModifiers::SUPER) => c.insert_char(ch),
        _ => {}
    }
}

/// A paste lands only in the message box (rule 2).
pub fn paste(app: &mut App, s: &str) {
    if app.threads.focus() == TFocus::Input && app.threads.has_input() {
        app.threads.composer().insert_paste(&s.replace('\r', ""));
        app.threads.owe_draft();
    } else {
        app.notice = Some("the message box is not open: `a` opens it, then paste".into());
    }
}

// ── the mouse ─────────────────────────────────────────────────────────────

/// A mouse event on the Threads tab (the top bar is the caller's): a click
/// on a thread opens it (0214 A5); elsewhere a click selects and a click on
/// the selection opens it (rule 9) — and a BAND opens or
/// closes at one click (msg 235: "anywhere that we have a band that can be
/// expanded ... they all need to toggle by single click"): a band of the
/// list, a tool call or a batch of calls in the transcript, the subagents'
/// band. The wheel scrolls what is under it.
pub fn mouse(app: &mut App, m: MouseEvent, hit: Option<Hit>) {
    let t = &mut app.threads;
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => match hit {
            Some(Hit::TList(i)) => {
                t.list_item = i as usize;
                t.focus = Some(TFocus::List);
                let band = t.list_items().get(i as usize).and_then(|(_, it)| match it {
                    LItem::Band(k) => Some(k.clone()),
                    LItem::Thread(_) => None,
                });
                match band {
                    Some(k) => {
                        let open = t.band_open(&k);
                        t.set_band(&k, !open);
                    }
                    // A thread opens at ONE click (0214 A5).
                    None => list_key(t, KeyCode::Enter, false),
                }
            }
            Some(Hit::TChat(i)) => {
                let again = t.chat_item == i as usize && t.focus() == TFocus::Chat;
                t.chat_item = i as usize;
                t.focus = Some(TFocus::Chat);
                let band = t.blocks().get(i as usize).is_some_and(|b| b.opens() && !matches!(b, Block::Agent { .. }));
                if band || again {
                    open_block(t);
                }
            }
            Some(Hit::TAgent(r)) => {
                let again = t.agent_row == r as usize && t.focus() == TFocus::Agents;
                t.agent_row = r as usize;
                t.focus = Some(TFocus::Agents);
                if r == 0 {
                    t.agents_shut = !t.agents_shut;
                } else if again {
                    agents_key(t, KeyCode::Enter);
                }
            }
            Some(Hit::TInput) => t.focus = Some(TFocus::Input),
            _ => {}
        },
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let up = m.kind == MouseEventKind::ScrollUp;
            match hit {
                Some(Hit::TList(_)) => t.list_scroll = if up { t.list_scroll.saturating_sub(3) } else { t.list_scroll + 3 },
                Some(Hit::TChat(_)) => t.chat_scroll = if up { t.chat_scroll + 3 } else { t.chat_scroll.saturating_sub(3) },
                _ => {}
            }
        }
        _ => {}
    }
    settle(app);
}

#[cfg(test)]
mod subagent_cache_tests {
    use super::*;

    /// **REL-6a2: the subagent cache is capped**, least recently used out,
    /// and never evicts the open thread. Opposition: at the cap nothing is
    /// evicted, and a READ of an entry keeps it (reads count as use).
    #[test]
    fn rel6a2_the_subagent_cache_holds_at_most_its_cap_and_keeps_the_open_thread() {
        let mut t = Threads::default();
        let id = |i: usize| format!("t{i}");
        t.open = Some(id(0));
        for i in 0..SUBAGENT_CACHE_THREADS {
            t.remember_direct(&id(i), Vec::new());
        }
        assert_eq!(t.direct_of.len(), SUBAGENT_CACHE_THREADS, "at the cap nothing is evicted");
        // A read of t1 (the open-thread path's `touch_direct`) is a use.
        t.touch_direct(&id(1));
        for i in SUBAGENT_CACHE_THREADS..SUBAGENT_CACHE_THREADS * 3 {
            t.remember_direct(&id(i), Vec::new());
            assert!(t.direct_of.len() <= SUBAGENT_CACHE_THREADS, "{} threads held past the cap", t.direct_of.len());
            assert_eq!(t.direct_lru.len(), t.direct_of.len(), "the order and the map disagree");
        }
        assert!(t.direct_of.contains_key(&id(0)), "the open thread was evicted: {:?}", t.direct_cached());
        assert!(!t.direct_of.contains_key(&id(1)), "t1 outlived 2x the cap of newer threads: {:?}", t.direct_cached());
        let newest = id(SUBAGENT_CACHE_THREADS * 3 - 1);
        assert_eq!(t.direct_cached().last(), Some(&newest));
        // Reads count: t1 touched just before 1 newer insert survives where
        // an untouched sibling of the same age does not.
        let mut u = Threads::default();
        for i in 0..SUBAGENT_CACHE_THREADS {
            u.remember_direct(&id(i), Vec::new());
        }
        u.touch_direct(&id(0));
        u.remember_direct("newer", Vec::new());
        assert!(u.direct_of.contains_key(&id(0)), "a read did not count as use: {:?}", u.direct_cached());
        assert!(!u.direct_of.contains_key(&id(1)), "the least recently used was kept: {:?}", u.direct_cached());
    }
}
