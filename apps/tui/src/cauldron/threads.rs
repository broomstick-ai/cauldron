//! **THE THREADS TAB** (8e4fca10 msg 186, task 0167, made it a MODE behind
//! the top-left button; msg 235 made it a TAB: "I actually don't like the
//! cauldron and threads flipping back and forth with that top left button
//! ... let's add another top level tab called threads ... And then the
//! whole threads application can be moved inside of that threads tab"): the
//! interface he talks to the coordinator through — the harness `tui` chat
//! face — rebuilt inside this face, on its own data.
//!
//! > *"that word cauldron at the top left, we should make that a button ...
//! > we can have it switch from saying cauldron to saying threads and at a
//! > whole different interface ... I don't want you to just copy the whole
//! > lib because I want you to use fake data and I want you to be
//! > intentional about how it looks."*
//!
//! What it keeps from the chat face (`libs/tui-render`): its screens — the
//! thread LIST (pinned, recent, projects, archived) and a THREAD (the
//! transcript, the message box, the subagent tree under it); in a thread, a
//! first line that says its name, its state and its project; the
//! assistant on the left under `claude · time · took`, his words on the right
//! with their id at the top right; the tool lane (`⚙`, `◇ n calls`, `⚠`);
//! the compaction rule; a subagent tree whose first row is the thread itself
//! and whose selected row is whose transcript is shown; the region order chat
//! ↔ message box ↔ subagents; ← at the edge going back.
//!
//! What it changes, for this face's visual system (`look`, msg 186): the
//! selection is a background, never a `▸` marker; his message is the node
//! CARD (rounded, its id the legend), since a tinted block would hide the
//! selection's background; the list's sections, a project, a tool call, a
//! batch of calls and the subagents are BANDS on the one band line, their
//! counts as metadata segments; thread rows, subagent rows and an opened
//! call's details are TABLES; there is no footer of key hints (this face
//! keeps its keys in the `?` palette, and no legends).
//!
//! The data is `<seeds>/threads.json`. Nothing here talks to a daemon: a message
//! sent is kept in this face's memory and marked held.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use serde::Deserialize;
use tui_render::compose::Composer;
use tui_render::palette;

use crate::cauldron::app::{App, Hit};
use crate::cauldron::look::{self, Card, Tone};
use crate::cauldron::view::{display_width, wrap};

// ── the data ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Seed {
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub threads: Vec<Thread>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Thread {
    pub id: String,
    pub name: String,
    pub project: String,
    /// `active` (a turn is running) or `idle`.
    pub state: String,
    #[serde(default)]
    pub unread: usize,
    /// Since the last activity, as the list prints it.
    pub age: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub blocks: Vec<Block>,
    #[serde(default)]
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Agent {
    pub id: String,
    /// The subagent that launched it; none for the thread's own.
    #[serde(default)]
    pub parent: Option<String>,
    pub label: String,
    /// `running`, `done` or `failed`.
    pub state: String,
    pub age: String,
    #[serde(default)]
    pub tokens: u64,
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub blocks: Vec<Block>,
}

/// One thing in a transcript.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Block {
    /// His message: its id (the thread's message number) and time.
    User { id: String, at: String, text: String },
    /// The assistant's turn: when, and how long it took.
    Assistant {
        at: String,
        #[serde(default)]
        took: String,
        text: String,
    },
    /// One tool call: its glyph, what it did, a short result, its details.
    Tool {
        tool: String,
        text: String,
        #[serde(default)]
        meta: String,
        #[serde(default)]
        detail: Vec<[String; 2]>,
    },
    /// A run of tool calls folded into one row.
    Group {
        text: String,
        count: u64,
        #[serde(default)]
        items: Vec<String>,
    },
    /// A subagent launched here (its row in the tree).
    Agent { agent: String },
    Warn { text: String },
    /// A line this face adds and no daemon wrote (a message held here).
    Note { text: String },
    Compacted { text: String },
}

impl Block {
    /// Whether → does anything on it.
    fn opens(&self) -> bool {
        match self {
            Block::Tool { detail, .. } => !detail.is_empty(),
            Block::Group { items, .. } => !items.is_empty(),
            Block::Agent { .. } => true,
            _ => false,
        }
    }
    /// A lane row: stacked under the turn above, no blank line between.
    fn lane(&self) -> bool {
        matches!(self, Block::Tool { .. } | Block::Group { .. } | Block::Agent { .. } | Block::Warn { .. } | Block::Note { .. })
    }
}

/// `<seeds>/threads.json` (or `<seeds>/soup/<env>.threads.json` for a cauldron of
/// its own).
pub fn load(seeds: &Path, env: &str) -> Result<Seed> {
    let own = seeds.join("soup").join(format!("{env}.threads.json"));
    let path = if own.exists() { own } else { seeds.join("threads.json") };
    if !path.exists() {
        return Ok(Seed::default());
    }
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
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
    /// A band: `pinned`, `recent`, `projects`, `archived`, or `project:<name>`.
    Band(String),
    /// Thread `.0` (into `Seed::threads`).
    Thread(usize),
}

/// A laid-out page: its lines, and the line range of each selectable thing.
pub struct Laid<T> {
    pub lines: Vec<Line<'static>>,
    pub items: Vec<(std::ops::Range<usize>, T)>,
}

#[derive(Default)]
pub struct Threads {
    pub seed: Seed,
    /// The thread on screen; none on the list.
    pub open: Option<usize>,
    /// Whose transcript is shown: a subagent's id, or none for the thread's.
    pub subject: Option<String>,
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
    /// Blocks opened in place, by `thread/subject/block`.
    pub opened: HashSet<String>,
    /// The subagent tree's row (0 its band, 1 the thread, then each agent).
    pub agent_row: usize,
    pub agents_shut: bool,
    /// Each thread's message box.
    pub composers: HashMap<usize, Composer>,
    /// The message box's text width in the last frame.
    pub input_w: usize,
    /// The first tree row the last frame drew (the band shows a window).
    pub tree_top: usize,
}

impl Threads {
    pub fn new(seed: Seed) -> Self {
        Self { seed, ..Self::default() }
    }

    pub fn focus(&self) -> TFocus {
        self.focus.unwrap_or(if self.open.is_some() { TFocus::Chat } else { TFocus::List })
    }

    /// Unread across every thread that is not archived.
    pub fn unread(&self) -> usize {
        self.seed.threads.iter().filter(|t| !t.archived).map(|t| t.unread).sum()
    }

    pub fn active(&self) -> usize {
        self.seed.threads.iter().filter(|t| t.state == "active").count()
    }

    /// The thread on screen.
    pub fn thread(&self) -> Option<&Thread> {
        self.open.and_then(|i| self.seed.threads.get(i))
    }

    pub fn agent(&self, id: &str) -> Option<&Agent> {
        self.thread()?.agents.iter().find(|a| a.id == id)
    }

    /// The transcript on screen: the subject's.
    pub fn blocks(&self) -> &[Block] {
        let Some(t) = self.thread() else { return &[] };
        match &self.subject {
            Some(id) => t.agents.iter().find(|a| &a.id == id).map(|a| a.blocks.as_slice()).unwrap_or(&[]),
            None => &t.blocks,
        }
    }

    /// The message box shows only for the thread itself: a subagent takes
    /// no messages.
    pub fn has_input(&self) -> bool {
        self.open.is_some() && self.subject.is_none()
    }

    pub fn composer(&mut self) -> &mut Composer {
        let i = self.open.unwrap_or(usize::MAX);
        self.composers.entry(i).or_default()
    }

    pub fn composer_ref(&self) -> Option<&Composer> {
        self.open.and_then(|i| self.composers.get(&i))
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

    fn block_key(&self, i: usize) -> String {
        format!("{}/{}/{i}", self.open.unwrap_or(0), self.subject.as_deref().unwrap_or("-"))
    }

    pub fn block_open(&self, i: usize) -> bool {
        self.opened.contains(&self.block_key(i))
    }

    /// Open thread `i`: its transcript at the newest line, read.
    pub fn open_thread(&mut self, i: usize) {
        if i >= self.seed.threads.len() {
            return;
        }
        self.open = Some(i);
        self.subject = None;
        self.seed.threads[i].unread = 0;
        self.chat_item = self.blocks().len().saturating_sub(1);
        self.chat_scroll = 0;
        self.agent_row = 1;
        self.focus = Some(TFocus::Chat);
        self.reveal = true;
    }

    /// Back to the list, on the row of the thread he left.
    pub fn close_thread(&mut self) {
        let was = self.open.take();
        self.subject = None;
        self.focus = Some(TFocus::List);
        if let Some(w) = was {
            if let Some(at) = self.list_items().iter().position(|(_, it)| *it == LItem::Thread(w)) {
                self.list_item = at;
            }
        }
        self.reveal = true;
    }

    /// Show `subject`'s transcript (none: the thread's own).
    pub fn set_subject(&mut self, subject: Option<String>) {
        self.subject = subject;
        self.chat_item = self.blocks().len().saturating_sub(1);
        self.chat_scroll = 0;
        self.agent_row = self.tree().iter().position(|r| r.id == self.subject).map_or(1, |p| p + 1);
        self.reveal = true;
    }

    // ── the list ──

    fn list_items(&self) -> Vec<(std::ops::Range<usize>, LItem)> {
        self.list(100).items
    }

    /// The thread list: PINNED, RECENT, PROJECTS (each project a band of its
    /// threads) and ARCHIVED, on the one band line.
    pub fn list(&self, width: usize) -> Laid<LItem> {
        let mut lines = Vec::new();
        let mut items = Vec::new();
        let threads = &self.seed.threads;
        // One table for every thread row on the page, so their columns line
        // up whatever band they are in.
        let cells: Vec<Vec<String>> = threads.iter().map(|t| thread_cells(t, width)).collect();
        let widths = look::widths(&cells);
        let count = |idx: &[usize]| {
            let unread: usize = idx.iter().map(|&i| threads[i].unread).sum();
            let active = idx.iter().filter(|&&i| threads[i].state == "active").count();
            look::meta([
                look::seg("", idx.len(), if idx.len() == 1 { "thread" } else { "threads" }, Tone::Plain),
                if active > 0 { look::seg("●", active, "active", Tone::Good) } else { Span::raw("") },
                if unread > 0 { look::seg("•", unread, "unread", Tone::Warn) } else { Span::raw("") },
            ])
        };
        let band = |lines: &mut Vec<Line<'static>>, items: &mut Vec<(std::ops::Range<usize>, LItem)>, key: &str, name: &str, idx: Vec<usize>, depth: usize, extra: Vec<Span<'static>>| {
            if !lines.is_empty() && depth == 0 {
                lines.push(Line::raw(""));
            }
            let open = self.band_open(key);
            let at = lines.len();
            let mut label = vec![look::name(name)];
            label.extend(extra);
            lines.push(look::band_line(depth, open, label, count(&idx), width));
            items.push((at..at + 1, LItem::Band(key.to_string())));
            if open {
                for i in idx {
                    let at = lines.len();
                    // Under its project, a thread's project cell is blank:
                    // the band above says it (the columns still line up).
                    lines.push(thread_row(&threads[i], &cells[i], &widths, depth + 1, !key.starts_with("project:"), width));
                    items.push((at..at + 1, LItem::Thread(i)));
                }
            }
        };
        let live: Vec<usize> = (0..threads.len()).filter(|&i| !threads[i].archived).collect();
        band(&mut lines, &mut items, "pinned", "Pinned", live.iter().copied().filter(|&i| threads[i].pinned).collect(), 0, vec![]);
        band(&mut lines, &mut items, "recent", "Recent", live.iter().copied().filter(|&i| !threads[i].pinned).collect(), 0, vec![]);
        // PROJECTS: a band, and each project a band of its threads.
        lines.push(Line::raw(""));
        let open = self.band_open("projects");
        let at = lines.len();
        let n = self.seed.projects.len();
        lines.push(look::band_line(0, open, vec![look::name("Projects")], look::meta([look::seg("", n, if n == 1 { "project" } else { "projects" }, Tone::Plain)]), width));
        items.push((at..at + 1, LItem::Band("projects".into())));
        if open {
            let pw = self.seed.projects.iter().map(|p| display_width(&p.name)).max().unwrap_or(0);
            for p in &self.seed.projects {
                let idx: Vec<usize> = live.iter().copied().filter(|&i| threads[i].project == p.name).collect();
                let key = format!("project:{}", p.name);
                let pad = " ".repeat(pw.saturating_sub(display_width(&p.name)) + 2);
                band(&mut lines, &mut items, &key, &p.name, idx, 1, vec![Span::styled(format!("{pad}{}", p.path), palette::muted())]);
            }
        }
        band(&mut lines, &mut items, "archived", "Archived", (0..threads.len()).filter(|&i| threads[i].archived).collect(), 0, vec![]);
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
                Block::Tool { tool, text, meta, detail } => {
                    let open = self.block_open(i);
                    let name = vec![Span::styled(format!("{tool} {text}"), palette::ink2())];
                    let meta = if meta.is_empty() { vec![] } else { vec![look::word(meta.clone(), Tone::Plain)] };
                    lines.push(lane_line(!detail.is_empty(), open, name, meta, width));
                    if open {
                        let rows = detail.iter().map(|[k, v]| (k.clone(), vec![Span::styled(v.clone(), palette::ink2())])).collect();
                        lines.extend(look::table(rows, 8, width));
                    }
                }
                Block::Group { text, count, items: calls } => {
                    let open = self.block_open(i);
                    let name = vec![Span::styled(format!("◇ {text}"), palette::ink2())];
                    lines.push(lane_line(!calls.is_empty(), open, name, vec![look::seg("", count, "calls", Tone::Plain)], width));
                    if open {
                        for c in calls {
                            lines.push(Line::from(vec![Span::raw(" ".repeat(8)), Span::styled(c.clone(), palette::muted())]));
                        }
                    }
                }
                Block::Agent { agent } => {
                    let (label, state) = self.agent(agent).map_or((agent.clone(), "unknown".to_string()), |a| (a.label.clone(), a.state.clone()));
                    let name = vec![Span::styled(format!("◆ {label}"), palette::ink2())];
                    lines.push(lane_line(true, false, name, vec![state_seg(&state)], width));
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
        let Some(t) = self.thread() else { return Vec::new() };
        if t.agents.is_empty() {
            return Vec::new();
        }
        let mut out = vec![TreeRow { id: None, depth: 0, last: false, rails: String::new() }];
        fn walk(t: &Thread, parent: Option<&str>, depth: usize, rails: &str, out: &mut Vec<TreeRow>) {
            let kids: Vec<&Agent> = t.agents.iter().filter(|a| a.parent.as_deref() == parent).collect();
            for (k, a) in kids.iter().enumerate() {
                let last = k + 1 == kids.len();
                out.push(TreeRow { id: Some(a.id.clone()), depth, last, rails: rails.to_string() });
                let next = format!("{rails}{}", if last { "  " } else { "│ " });
                walk(t, Some(&a.id), depth + 1, &next, out);
            }
        }
        walk(t, None, 1, "", &mut out);
        out
    }

    /// The subagent band: its line, then (open) a row per tree row.
    pub fn agents(&self, width: usize) -> Vec<Line<'static>> {
        let Some(t) = self.thread() else { return Vec::new() };
        let tree = self.tree();
        if tree.is_empty() {
            return Vec::new();
        }
        let n = |s: &str| t.agents.iter().filter(|a| a.state == s).count();
        let meta = look::meta([
            look::seg("▶", n("running"), "running", if n("running") > 0 { Tone::Good } else { Tone::Plain }),
            look::seg("✓", n("done"), "done", Tone::Plain),
            if n("failed") > 0 { look::seg("✕", n("failed"), "failed", Tone::Bad) } else { Span::raw("") },
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
                "running" => look::tone(Tone::Good),
                "failed" => look::tone(Tone::Bad),
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
                s.push(sep());
                s.push(if t.state == "active" { Span::styled("active", palette::code()) } else { Span::styled(t.state.clone(), palette::muted()) });
                s.push(sep());
                s.push(Span::styled(t.project.clone(), palette::muted()));
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

    /// Send the message box's text into the thread: it lands as his message
    /// with the next id, and is held here — there is no daemon behind this
    /// tab — which the transcript says.
    pub fn send(&mut self) {
        let Some(i) = self.open else { return };
        let text = self.composer().text();
        if text.trim().is_empty() {
            return;
        }
        let t = &mut self.seed.threads[i];
        let next = t.blocks.iter().filter_map(|b| match b {
            Block::User { id, .. } => id.parse::<u64>().ok(),
            _ => None,
        }).max().unwrap_or(0) + 1;
        let at = chrono::Local::now().format("%H:%M").to_string();
        t.blocks.push(Block::User { id: next.to_string(), at, text: text.trim_end().to_string() });
        t.blocks.push(Block::Note { text: "held in this face · no daemon behind the Agents tab".into() });
        self.composers.insert(i, Composer::new());
        self.chat_item = self.blocks().len().saturating_sub(1);
        self.chat_scroll = 0;
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
        if t.state == "active" { "● active".into() } else { t.state.clone() },
        if t.unread > 0 { format!("• {}", t.unread) } else { String::new() },
        t.age.clone(),
    ]
}

fn thread_row(t: &Thread, c: &[String], w: &[usize], depth: usize, project: bool, width: usize) -> Line<'static> {
    let mut spans = vec![Span::raw("  ".repeat(depth + 1)), Span::styled(look::cell(&c[0], w[0] + 2), palette::ink())];
    spans.push(Span::styled(look::cell(if project { &c[1] } else { "" }, w[1] + 2), palette::muted()));
    spans.push(Span::styled(look::cell(&c[2], w[2] + 2), if t.state == "active" { palette::code() } else { palette::muted() }));
    spans.push(Span::styled(look::cell(&c[3], w[3] + 2), palette::amber()));
    spans.push(Span::styled(format!("{:>width$}", c[4], width = w[4]), palette::muted()));
    Line::from(look::fit_spans(spans, width))
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
        let (said, said_style) = match (&app.error, &app.notice) {
            (Some(e), _) => (format!(" {e} "), palette::red()),
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
        return input_key(app, key, ctrl, alt);
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
            Some(LItem::Thread(i)) => t.open_thread(i),
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
        LItem::Band(k) if k.starts_with("project:") => 1,
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
    let top = |i: usize| matches!(&items[i].1, LItem::Band(k) if !k.starts_with("project:"));
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
            let k = t.block_key(t.chat_item);
            if !t.opened.remove(&k) {
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
        Block::Agent { agent } => t.set_subject(Some(agent)),
        b if b.opens() => {
            let k = t.block_key(i);
            if !t.opened.remove(&k) {
                t.opened.insert(k);
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
}

