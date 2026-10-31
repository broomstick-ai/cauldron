//! Application state and input handling. No terminal here: `handle` takes an
//! event and mutates state, so the headless dump and the tests drive exactly
//! the code path the live loop drives — keys AND mouse, since a click is
//! resolved against the hit regions the last drawn frame recorded.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use env::Environment;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use tui_render::compose::Composer;

use crate::cauldron::blueprint::{ClusterRepo, RepoId, Repos};
use crate::cauldron::corpus::{Corpus, ClusterRegion, NodeId, RegionId};
use crate::cauldron::dash::Dash;
use crate::cauldron::menu::{Bound, Field, Nav};
use crate::cauldron::ripples;
use crate::cauldron::settings::{self, Settings};
use crate::cauldron::model::{Forest, ClusterId, Message, RippleState, TaskState};
use crate::cauldron::store::{self, CreatedCluster, CreatedClusters, DraftStore, Layout, LayoutStore, SendLog};
use crate::cauldron::tags::{self, Tag};

/// The cauldron tabs, across the top (8e4fca10 msg 235, his final word:
/// "we have the cauldron tab. Then we have a threads tab, then we have a
/// status tab, and then we have a config tab ... instead of the root tab
/// being called cauldron, let's name the root tab surface"). SURFACE is the
/// soup (its clusters in the sidebar) over the bucket; THREADS is the
/// threads app (it was a mode behind the top-left button: "I actually
/// don't like the cauldron and threads flipping back and forth with that
/// top left button"); STATUS is the mesh, the tokens, the soup's loose ends
/// and the event bus, each a sub-tab; CONFIG is every setting. There is no
/// System tab: "there's no system level config stuff. That's my call."
/// 8e4fca10 msg 457 renamed two of them: "Let's rename the root surface tab
/// to Cauldron", and "instead of getting rid of the threads tab there at the
/// top, let's just call it agents" — CAULDRON | AGENTS | STATUS | CONFIG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CTab {
    Cauldron,
    Agents,
    Status,
    Config,
}

/// The cauldron tabs' names before 8e4fca10 msg 457, and what each is now.
/// Typed (a flag) they are REFUSED, by name, as earlier renames were; read
/// back from what this face STORED (a reference's origin, a setting) they are
/// the tab they always were, because his data outlives the word.
pub const CTAB_RENAMED: &[(&str, CTab)] = &[("surface", CTab::Cauldron), ("threads", CTab::Agents)];

impl CTab {
    pub const ALL: [CTab; 4] = [CTab::Cauldron, CTab::Agents, CTab::Status, CTab::Config];
    pub fn label(self) -> &'static str {
        match self {
            CTab::Cauldron => "Cauldron",
            CTab::Agents => "Agents",
            CTab::Status => "Status",
            CTab::Config => "Config",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.label().eq_ignore_ascii_case(s))
    }
    /// The tab `s` names under its name before msg 457, if it is one.
    pub fn renamed(s: &str) -> Option<Self> {
        CTAB_RENAMED.iter().find(|(old, _)| old.eq_ignore_ascii_case(s)).map(|(_, t)| *t)
    }
    /// A tab as this face stored it: today's name, or its old one.
    pub fn stored(s: &str) -> Option<Self> {
        Self::parse(s).or_else(|| Self::renamed(s))
    }
    fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).expect("in ALL")
    }
}

/// The cluster tabs, inside a cluster's page (msg 165; msg 186 merged
/// Artifacts into what was Soup; msg 235: "the tab inside of what we are
/// currently calling a surface tab that's named soup, we'll change that
/// word soup to contents", and "I actually think we can drop config from
/// the cluster"): Details (it was Overview) is what the cluster IS, in
/// always-expanded bands; Questions and Actions what waits on him; Tasks its execution graph —
/// its tasks and milestones as a DAG; Nodes its regions, its artifacts,
/// its repos and its directories; Status its ripples and its log. b964d571
/// msg 23: "Rename the Tasks {N} tab to Human and then let's have a Tasks
/// tab for DAG stuff" — the Engine moved from Status to the new Tasks.
/// 8e4fca10 msg 385: "flatten out the Human tab into Questions and Actions
/// in the main cluster tab bar" — each carries its own count — and "move
/// overview to the end and call it Details".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KTab {
    Questions,
    Actions,
    Tasks,
    Nodes,
    Status,
    Details,
}

impl KTab {
    pub const ALL: [KTab; 6] = [KTab::Questions, KTab::Actions, KTab::Tasks, KTab::Nodes, KTab::Status, KTab::Details];
    pub fn label(self) -> &'static str {
        match self {
            KTab::Details => "Details",
            KTab::Questions => "Questions",
            KTab::Actions => "Actions",
            KTab::Tasks => "Tasks",
            KTab::Nodes => "Nodes",
            KTab::Status => "Status",
        }
    }
    /// The name a tab takes when the tabs do not fit.
    pub fn short(self) -> &'static str {
        match self {
            KTab::Details => "Det",
            KTab::Questions => "Qs",
            KTab::Actions => "Acts",
            KTab::Tasks => "Tsk",
            KTab::Nodes => "Nod",
            KTab::Status => "St",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.to_ascii_lowercase().replace(['-', '_', ' '], "");
        Self::ALL.into_iter().find(|t| t.label().to_ascii_lowercase() == s)
    }
}

/// A selectable footer line: `5 below` (turns aggregate on), `log ▸` (the
/// cluster's log, at the foot of its Status tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Footer {
    /// Turns aggregate on.
    Below,
    /// Opens the cluster's log.
    Log,
}

/// A collapsible band on a cluster's page: a status band on Tasks (active,
/// complete, superseded, archived questions; active, done, archived human
/// actions), or a band of its Status tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Band {
    /// A band of Details (msg 186: "use the collapsible band structure to
    /// organize the overview tab"; 8e4fca10 msg 385: "make the sections into
    /// bands without expand ability") — always expanded, see [`Band::fixed`].
    Details(DBand),
    Questions(QBand),
    Actions(HaBand),
    /// The cluster's ripples, and under it each state.
    Ripples,
    RippleState(RBand),
    /// Its execution graph.
    Engine,
    /// Under aggregate, one cluster's part of the execution graph.
    EngineCluster(ClusterId),
    /// Under aggregate, the waits that cross between clusters.
    EngineAcross,
    /// A day of the cluster's log (`None`: what carries no time).
    LogDay(Option<chrono::NaiveDate>),
}

/// Details' bands, in order: what the cluster is, his last dump here,
/// his directives and constraints, the agents' objectives and milestones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DBand {
    Scope,
    LastDump,
    Directives,
    Constraints,
    Objectives,
    Milestones,
    /// The cluster itself: its id, and what may be done to it.
    Cluster,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QBand {
    Active,
    /// Questions answered by an assumption that stands (msg 186): second.
    Assumed,
    Complete,
    Superseded,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HaBand {
    Active,
    Done,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RBand {
    Active,
    Parked,
    Pending,
    Background,
}

impl Band {
    /// An ALWAYS-EXPANDED band (8e4fca10 msg 235's word; msg 385: Details'
    /// sections are "bands without expand ability"): drawn with a dot for its
    /// chevron, never closed by a key, a click or a stored toggle.
    pub fn fixed(self) -> bool {
        matches!(self, Band::Details(_))
    }

    /// Open until he closes it: Details' bands (which never close), the active bands, and the
    /// Status tab's ripples, engine and active ripples.
    pub fn open_by_default(self) -> bool {
        self.fixed() || matches!(self, Band::Questions(QBand::Active) | Band::Actions(HaBand::Active) | Band::Ripples | Band::Engine | Band::RippleState(RBand::Active) | Band::EngineCluster(_) | Band::EngineAcross | Band::LogDay(_))
    }
}

/// Where the Artifacts tab is opened to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RNav {
    /// A repo's page: its blueprint.
    Repo(RepoId),
    /// One blueprint entry's page.
    Entry(RepoId, usize),
}

/// Which region holds the selection — `docs/navigation.md` rule 1. Printable
/// keys type into the input ONLY when it has the focus; everywhere else they
/// are commands. (A modal, when one is open, takes every key over all three.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Content,
    Input,
}

/// One selectable thing on a cluster's page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// The cluster's SCOPE band on Details: its boundaries.
    Description(ClusterId),
    /// The LAST DUMP band on Details.
    LastDump(ClusterId),
    /// Directive `ix` of cluster `cluster` (the cluster that holds it).
    Directive { cluster: ClusterId, ix: usize },
    /// Constraint `ix` of cluster `cluster`.
    Constraint { cluster: ClusterId, ix: usize },
    Question { cluster: ClusterId, serial: u32 },
    /// `opt` is 1-based, as printed.
    Option { cluster: ClusterId, serial: u32, opt: usize },
    /// A question no longer active (complete, superseded or archived).
    Closed { cluster: ClusterId, serial: u32 },
    /// Artifact `ix` inserted in question `serial` (msg 186): a card in its
    /// card; Enter opens it in a modal, `o` hands it to `open`.
    QArt { cluster: ClusterId, serial: u32, ix: usize },
    Objective { cluster: ClusterId, serial: u32 },
    Milestone { cluster: ClusterId, serial: u32 },
    Task { cluster: ClusterId, serial: u32 },
    Ha { cluster: ClusterId, serial: u32 },
    /// Interactive part `part` of a human action's steps: a field to paste
    /// into, a step's `done`, a text to copy, a link to open.
    HaPart { cluster: ClusterId, serial: u32, part: usize },
    /// A repo a cluster claims (index into `Repos::repos`).
    Repo(RepoId),
    /// Blueprint entry `node` of repo `repo`.
    Bp { repo: RepoId, node: usize },
    /// A background ripple (`ix` into the cluster's list).
    Background { cluster: ClusterId, ix: usize },
    /// One of his messages (index into `App::messages`).
    Message(usize),
    Footer(Footer),
    /// A collapsible band's header.
    Band(Band),
    /// A region of the soup (index into `Corpus::regions`).
    Region(RegionId),
    /// A node of the soup (index into `Corpus::nodes`).
    Node(NodeId),
    /// The ripple carrying task `serial` of `cluster` out.
    Ripple { cluster: ClusterId, serial: u32 },
    /// Management act `.0` of the selected cluster, at the foot of its
    /// Details (a cluster has no Config, msg 235): 0 rename, 1 move
    /// under, 2 delete.
    Manage(u8),
    /// The cluster's LOG (a trace page off its Status tab's `log ▸`): every
    /// dump that landed on it and every ripple run that touched it.
    Log,
    /// Dump `.0` (its sequence number) on the log.
    Sent(u64),
    /// Run `.0` (an index of `ripples::Store::indexed_runs`) on the log.
    Run(usize),
    /// A row of the Artifacts tab (design 0146), by the hash of its key
    /// (`artpage::hash`): a band, a worktree, an artifact, a file.
    Art(u64),
}

/// One selectable row on the Status or Config tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// Row `.0` of the Status tab ([`crate::cauldron::status::view`]).
    Status(usize),
    /// Row `.0` of the Config page on screen (its selectable entries).
    Setting(usize),
}

/// What a screen region does when clicked. Recorded by the view each frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    CTab(CTab),
    KTab(KTab),
    TreeRow(ClusterId),
    Disclosure(ClusterId),
    /// The `+` in the sidebar's header: a new ROOT cluster.
    AddRoot,
    /// The `+` beside a cluster: a new cluster under it.
    AddChild(ClusterId),
    /// The switch at the foot of the sidebar, `Aggregated clusters` (b964d571
    /// msg 23): its name flips it; `BelowOpt(on)` is its `on` or its `off`.
    Below,
    BelowOpt(bool),
    /// The line between the sidebar and the page: press and drag to resize.
    Divider,
    /// The grabber on the line between the soup and the bucket (msg 235:
    /// "we should be able to resize the boundary if we want"): press and
    /// drag it up or down.
    BucketDivider,
    Item(usize),
    Row(usize),
    /// Sub-tab `.0` of the strip on a cauldron tab (Status, Config).
    Strip(u8),
    /// A title the DEV rename reaches by a double click (msg 235): the
    /// sidebar's title, a box's legend. Tabs are reached by their own hits.
    Title(Title),
    /// The cauldron's name in the top bar: opens the cauldrons dropdown.
    Env,
    /// Anywhere inside an open modal or the dropdown. A click outside every
    /// such region closes it and goes no further.
    Modal,
    /// Entry `i` of the cauldrons dropdown; `i == entries.len()` is `+`.
    Drop(usize),
    /// Option `i` of a picker.
    Choice(usize),
    /// The ⓘ on selectable Config row `i`: its explanation.
    Tip(usize),
    /// Chip `.0` of the dump as the last frame drew it (`App::chip_hits`):
    /// opens it.
    Chip(u16),
    /// Button `.0` of the chip modal: insert as text, keep, discard.
    ChipButton(u8),
    /// Button `.0` of a confirm or a form modal: 0 does what it asks (Enter),
    /// 1 cancels (Esc).
    ConfirmButton(u8),
    /// `copy` on the bucket's top border (msg 186: "maybe you could even just
    /// give me a copy button"): everything in the bucket to the clipboard.
    BucketCopy,
    /// A path on the Nodes tab (`.0` into `App::hotspot_paths`): one click
    /// copies it (0146: a CLICK TARGET, acting on the first click).
    CopyAt(u16),
    /// The `↗` beside a path: one click runs `open <path>`.
    OpenAt(u16),
    Input,
    /// Row `.0` of the thread list (the Threads tab).
    TList(u16),
    /// Block `.0` of the transcript on screen.
    TChat(u16),
    /// Row `.0` of the subagent band (0 its line, 1 the thread, then each).
    TAgent(u16),
    /// The message box.
    TInput,
    /// Link `.0` of the bus modal's page on screen.
    BusLink(u16),
    /// Option `opt` of switch `cell` on selectable Config row `row` (a
    /// setting's switch is cell 0; a table row's are counted along it): a
    /// click picks it.
    Opt { row: u16, cell: u8, opt: u8 },
    /// The revert glyph ↺ on selectable Config row `.0`.
    Revert(u16),
    /// Selectable row `.0` of a table row's modal.
    MRow(u16),
    /// Option `opt` of switch `cell` on the row modal's row `row`.
    MOpt { row: u16, cell: u8, opt: u8 },
    /// The ↺ on the row modal's row `.0`.
    MRevert(u16),
    /// The ⓘ on line `.0` of the row modal (an index into its entries).
    MTip(u16),
    /// The button on selectable Config row `.0`: one click presses it.
    Press(u16),
    /// The button on the row modal's row `.0`.
    MPress(u16),
}

/// A window over the face. While one is open it takes every key, and a click
/// outside it closes it and does nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    /// Naming a new cauldron.
    NewCauldron { name: String, error: Option<String> },
    /// Making a new cluster: at the root (the `+` in the sidebar's header),
    /// or under `parent` (the `+` beside a cluster). He names it and says, in
    /// his own words, what belongs in it — its BOUNDARIES, which the seeding
    /// ripple reads (msg 112). `at` is the field taking keys: 0 the name, 1
    /// the boundaries; `caret` the byte offset in it (clamped to its length,
    /// so `usize::MAX` is its end).
    NewCluster { parent: Option<String>, name: String, boundaries: String, at: u8, caret: usize, error: Option<String> },
    /// Every key binding.
    Palette { scroll: usize },
    /// What a setting means: its ⓘ (msg 165: the explanation lives ONLY
    /// here, never on the page).
    Help { title: String, text: String },
    /// Choosing a value from a list: a setting's (an enum, a model, a pool),
    /// or where a cluster moves.
    Picker { for_: crate::cauldron::menu::PickFor, title: String, options: Vec<String>, selected: usize },
    /// Naming an account to add to a pool.
    AddAccount { pool: String, name: String, error: Option<String> },
    /// Typing an item to add to a list setting.
    AddItem { list: Bound, name: String, error: Option<String> },
    /// A name (or a path) a management act needs: renaming a cluster or an
    /// account, registering a repo.
    Name { act: crate::cauldron::manage::Act, title: String, name: String, error: Option<String> },
    /// An act that throws something away, asked first: Enter (or →) does it,
    /// Esc (or ←) keeps things as they were.
    Confirm { act: crate::cauldron::manage::Act, title: String, body: Vec<String>, error: Option<String> },
    /// A paste chip of the bucket opened (design 0138 §4.4; msg 235: "when
    /// you click on it I want a modal to come up the modal shows me the
    /// content of the paste you can change it if you want or you can click
    /// insert as text"): its full text, editable, with `insert as text`,
    /// `keep` and `discard`. Closing it any way KEEPS the edit.
    Chip { id: String, editor: Composer, at: crate::cauldron::dump::ChipAt, confirm: bool },
    /// An inserted artifact opened (msg 186: "I can click on that artifact
    /// to like bring up a modal"): its file's text, scrolled by ↑ ↓; `o`
    /// hands it to `open` instead.
    Artifact { title: String, path: String, lines: Vec<String>, scroll: usize },
    /// The event bus explored (Status › Bus, msg 235: "if I click on one of
    /// those rows, then it opens up a modal where I can see more information
    /// about it ... And then when I click on that trigger, I should be able
    /// to see details about it, what the handler was, the corresponding
    /// execution"): a stack of pages, each with its selected link and
    /// scroll. → opens a link onto the stack, ← steps back, Esc closes.
    Bus { stack: Vec<(crate::cauldron::bus::BusPage, usize, usize)> },
    /// DEV ONLY — NOT FOR THE PRODUCTION VERSION (msg 235: "this should not
    /// make it into the production version. This is just for the
    /// developer"): a title renamed in place, to try a word on the screen.
    /// `key` is the title as the code writes it.
    DevRename { key: String, name: String },
    /// A table's ROW opened (msg 235: "if we have a row item, like an
    /// account, instead of making it a collapsible header, we could make
    /// configuring it something that you click on it and it opens a modal"):
    /// every setting of `page`, `sel` the selected one.
    Settings { page: crate::cauldron::menu::SPage, sel: usize },
}

/// A title the DEV rename can reach (msg 235): the words the face shows as
/// a box's or a pane's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Title {
    /// The sidebar's `Clusters`.
    Sidebar,
    /// The top box's legend, `surface` (it was `soup`: 8e4fca10 msg 457,
    /// "change the legend, which is currently called soup, to say surface.
    /// So we have the bucket and we have the surface").
    Surface,
    /// The bottom box's legend, `bucket`.
    Bucket,
}

impl Title {
    pub fn word(self) -> &'static str {
        match self {
            Title::Sidebar => "Clusters",
            Title::Surface => "surface",
            Title::Bucket => "bucket",
        }
    }
}

/// The cauldrons DROPDOWN under the cauldron's name (msg 165): not a modal
/// — no title, anchored at its top left under the name, as wide as the
/// longest name (at least 20). Each cauldron shows whether it is on; that is
/// not changed here. The row after the last is `+`, a new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dropdown {
    pub entries: Vec<(String, bool)>,
    pub selected: usize,
}

/// What the face asks the terminal to do, which a headless face cannot:
/// the live loop drains it after each event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outbox {
    /// Put this text on the clipboard (OSC 52, via term-shell).
    Copy(String),
    /// `open <path>`.
    Open(String),
}

/// Every key binding, in the order the palette lists them. The palette and
/// `--help` both print THIS table, and `docs/navigation.md` is the philosophy
/// it implements — the one the harness `tui` (and `tui --chat`) follows.
pub const KEYMAP: &[(&str, &str)] = &[
    ("↑ ↓ · ^P ^N", "move; at a pane's edge, on to the next pane"),
    ("→ · Enter", "open what is selected · expand a band"),
    ("←", "back one level · close a band"),
    ("⌥↑ ⌥↓", "previous / next section (paragraph, in the bucket)"),
    ("Home  End", "first / last row (line start / end, in the bucket)"),
    ("PgUp  PgDn", "scroll the page"),
    ("Esc", "to the bucket (Cauldron) · in the bucket, back to the page"),
    ("click", "a setting, band or button acts at once · else select, and click again to open"),
    ("click, on Cauldron", "its reference at the caret, in place of the one there · double click: a quote"),
    ("drag a ⣿", "resize the sidebar · the bucket"),
    ("⇧Tab · F1 … F4", "next cauldron tab · Cauldron · Agents · Status · Config"),
    ("→ ←, on a sub-tab strip · click one", "the next / previous sub-tab (Status, Config)"),
    ("^T", "disable or enable the cauldron (asks first; also `cauldron enabled`, Config's last tab)"),
    ("Tab · ⌥1 … ⌥6 · ⌥, ⌥.", "next cluster tab · a cluster tab · previous / next"),
    ("Space", "flip a switch (a setting, an account)"),
    ("h · click its ⓘ", "what a setting means"),
    ("x · Backspace", "clear an override: inherit again (in a list: asks, and again removes)"),
    ("+ -", "nudge a number, percent or duration"),
    ("Enter · Esc, in a value", "keep what is typed"),
    ("i", "quote the selected item into the bucket: ⌥↓, two new lines, > [its tag]"),
    ("a · click the bucket", "write in the bucket"),
    ("Enter, in the bucket", "dump the whole bucket"),
    ("^S, in the bucket", "dump the note the caret is in, alone"),
    ("a blank line, in the bucket", "starts a new note"),
    ("⌥A · click ⧉ copy", "copy the whole bucket (pastes as their text)"),
    ("^O · click a paste", "open it: its whole text, insert as text · keep · discard"),
    ("click a reference, in the bucket", "go to it: the cluster, tab and item it was put in from"),
    ("⌥Enter · ⇧Enter · ^J", "new line"),
    ("Backspace · Delete by a [reference] or paste", "remove the whole of it (← → jump it)"),
    ("← at the start · ↑ on the top line", "leave the bucket"),
    ("⌥← ⌥→", "word left / right, in the bucket"),
    ("^A ^E · ⌥Home ⌥End", "line start / end · start / end of the bucket"),
    ("^K ^U ^W ^Y · ^Z ^R", "cut to end / start / word · put back · undo / redo"),
    ("in a modal: ↑ ↓ · → · ←", "move · do it (Enter) · leave (Esc)"),
    ("+, on Clusters · on a cluster", "make a cluster at the root · under it"),
    ("Enter, on a step", "type into its field · mark it done · copy · open"),
    ("c · o · click a path · click ↗, on Nodes", "copy the path · open it"),
    ("^F", "search the cluster's contents and its nodes"),
    ("^G", "include everything below the cluster · Aggregated clusters: the whole cauldron too"),
    ("^E · click the cauldron", "cauldrons"),
    ("→ ←, on a ripple, on the Ledger", "open / shut the ripples it ran · Enter · click: explore it"),
    ("→ · Enter, in Agents", "open a thread, a band, a tool call · on a subagent, its transcript"),
    ("←, in Agents", "back: close it · a subagent to its thread · a thread to the list"),
    ("⌥↑ ⌥↓, in a thread", "the previous / next of your messages"),
    ("Enter, in a thread's message box", "send it"),
    ("double click a title or a tab", "rename it on screen (dev only)"),
    ("? · ^/", "this palette"),
    ("q · ^C", "quit (^C only, in the bucket)"),
];

/// A key that moves between tabs, from anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKey {
    Cauldron(usize),
    CauldronStep(isize),
    Cluster(usize),
    ClusterStep(isize),
}

/// What macOS types for Option-1 … Option-9 on a US layout when the terminal
/// sends Option as a character (iTerm2's and Terminal.app's default), so the
/// cluster-tab keys work without the terminal reporting an Alt modifier.
/// Two presses on the same item or row this close together are a double
/// click (msg 186: "If there's a reference item, like one that the eye toggle
/// would insert, let's make double click also insert it").
pub const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(400);

/// The bucket's text rows: never fewer, and, undragged, never more (msg 235).
pub const BUCKET_MIN: u16 = 3;
pub const BUCKET_MAX: u16 = 6;

const OPTION_DIGITS: [char; 9] = ['¡', '™', '£', '¢', '∞', '§', '¶', '•', 'ª'];

/// The tab key `key` is, if any. ⇧Tab steps the cauldron tabs (his answer to
/// the ⌘ question — ⌘ is no longer bound); F1–F4 jump to one. ⌥ arrives as
/// ALT (kitty, or Option-as-Meta) or as the character macOS types for it.
/// A click's reference put in at the bucket's caret ([`App::click_inserted`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickInsert {
    /// The bucket's text before it, and its caret's byte there.
    pub before: String,
    pub caret: usize,
    /// The text it made, before its save merged anything in.
    pub made: String,
    /// The reference it put in, as written.
    pub tag: String,
}

pub fn tab_key(key: &KeyEvent) -> Option<TabKey> {
    let m = key.modifiers;
    let digit = |c: char| c.to_digit(10).filter(|d| *d >= 1).map(|d| d as usize - 1);
    match key.code {
        KeyCode::F(n @ 1..=12) => Some(TabKey::Cauldron(n as usize - 1)),
        KeyCode::BackTab => Some(TabKey::CauldronStep(1)),
        KeyCode::Tab if m.contains(KeyModifiers::SHIFT) => Some(TabKey::CauldronStep(1)),
        KeyCode::Char(c) if m.contains(KeyModifiers::ALT) && !m.contains(KeyModifiers::CONTROL) => match c {
            ',' | '<' | '≤' => Some(TabKey::ClusterStep(-1)),
            '.' | '>' | '≥' => Some(TabKey::ClusterStep(1)),
            c => digit(c).or_else(|| OPTION_DIGITS.iter().position(|&o| o == c)).map(TabKey::Cluster),
        },
        KeyCode::Char('≤') if !m.contains(KeyModifiers::CONTROL) => Some(TabKey::ClusterStep(-1)),
        KeyCode::Char('≥') if !m.contains(KeyModifiers::CONTROL) => Some(TabKey::ClusterStep(1)),
        KeyCode::Char(c) if !m.contains(KeyModifiers::CONTROL) => {
            OPTION_DIGITS.iter().position(|&o| o == c).map(TabKey::Cluster)
        }
        _ => None,
    }
}

pub struct App {
    /// The environment this face is in.
    pub env: Environment,
    /// The Threads tab: its data and where he is in it.
    pub threads: crate::cauldron::threads::Threads,
    /// The FIXTURE seed directory a test or `--seeds` opened this face
    /// with; on a real environment `store::no_seeds`, which holds nothing,
    /// so the environment shows only its own data (REL-3.S2).
    pub seeds: PathBuf,
    pub forest: Forest,
    pub corpus: Corpus,
    pub dash: Dash,
    /// The registered repos and their blueprints.
    pub repos: Repos,
    /// His messages a trace quotes, by `thread#msg`.
    pub messages: Vec<(String, Message)>,
    pub ctab: CTab,
    pub ktab: KTab,
    /// The cluster page's bands he has turned from their default (open or
    /// closed): see [`Band::open_by_default`].
    pub bands: HashSet<Band>,
    pub focus: Focus,
    /// Where Esc (or an arrow) from the input returns to.
    pub back_to: Focus,
    /// The focused cluster — where an untagged send lands.
    pub selected: ClusterId,
    /// The sidebar's cursor is on the `+` in its header, above the first
    /// cluster (the page still shows `selected`).
    pub on_add_root: bool,
    /// The sidebar's cursor is on the switch at its foot, `Everything
    /// below`, the row after the last cluster.
    pub on_below: bool,
    pub collapsed: HashSet<ClusterId>,
    /// Show descendants' items on the page too.
    pub aggregate: bool,
    /// Content-pane scroll offset, in rendered rows.
    pub scroll: usize,
    /// The selected content item (index into [`App::items`]).
    pub item: usize,
    /// Scroll the selected item into view on the next frame.
    pub reveal: bool,
    /// Selected row on the Status and Config tabs.
    pub rows: [usize; 2],
    /// The trace pages stacked on the current tab: each entry, the item it
    /// was opened on and the (item, scroll) to return to.
    pub trace: Vec<(Item, usize, usize)>,
    /// How far the Artifacts tab is opened, with the (item, scroll) to return to.
    pub rnav: Vec<(RNav, usize, usize)>,
    /// Modules expanded on a repo's page.
    pub rexpanded: HashSet<(RepoId, usize)>,
    /// The Nodes tab's search query.
    pub search: String,
    /// Printable keys go to the search field (entered with ^F).
    pub searching: bool,
    pub composer: Composer,
    /// The cauldron's ONE bucket (msg 235: "You should not have a different
    /// bucket for each surface. I changed my mind on that. We're gonna have
    /// one bucket and because it's all going into one soup").
    pub draft: DraftStore,
    /// The bodies of its paste chips.
    pub chips: crate::cauldron::dump::Artifacts,
    /// The chips the last frame drew in the bucket, for [`Hit::Chip`].
    pub chip_hits: Vec<Tag>,
    /// Where each reference in the bucket was put in from (8e4fca10 msg
    /// 385), on disk beside it.
    pub refs: crate::cauldron::refs::Index,
    /// The Artifacts tab's soup: its directories, worktree and artifact
    /// nodes (FAKE, `<seeds>/artifacts.json`).
    pub arts: crate::cauldron::artifacts::ArtifactSeed,
    /// Every repo's worktrees: the seed's, replaced repo by repo by what git
    /// says when this face reads git (`live_git`).
    pub worktrees: Vec<crate::cauldron::artifacts::Worktree>,
    /// The Artifacts rows he has turned from their default (open or closed),
    /// by key, and the `n more ▸` lists he extended.
    pub art_toggled: HashSet<String>,
    /// The path a copy just went to: its cell flashes green for one frame.
    pub flash: Option<String>,
    /// The paths the last frame's copy and open targets name.
    pub hotspot_paths: Vec<String>,
    /// This face reads git for the worktrees (the live loop; never --dump).
    pub live_git: bool,
    /// A git read in flight, off the UI thread, and when the last one began.
    pub git_rx: Option<std::sync::mpsc::Receiver<GitRead>>,
    pub git_at: Option<std::time::Instant>,
    /// The seed's bucket being written (FAKE), used while the cauldron has
    /// none of its own on disk.
    pub seed_draft: String,
    pub sends: SendLog,
    /// What ran for each dump (FAKE): Status › Ledger (8e4fca10 msg 457).
    pub ledger: crate::cauldron::ledger::Seed,
    pub modal: Option<Modal>,
    /// A row's modal under the modal on screen (a picker or a name asked
    /// from it): it comes back when that one closes.
    pub modal_back: Option<Modal>,
    /// The cauldrons dropdown, while it is open (it takes every key, as a
    /// modal does).
    pub dropdown: Option<Dropdown>,
    /// Where the last frame drew the cauldron's name: the dropdown's top
    /// left hangs under it.
    pub dropdown_anchor: (u16, u16),
    /// What the live loop is to hand the terminal (a copy, an `open`).
    pub outbox: Vec<Outbox>,
    /// Click regions of the last drawn frame.
    pub hits: Vec<(Rect, Hit)>,
    /// The sidebar's share of the width, and where it is kept.
    pub layout: Layout,
    pub(crate) layout_store: LayoutStore,
    /// The sidebar's divider is being dragged.
    pub dragging: bool,
    /// The bucket's divider is being dragged.
    pub dragging_bucket: bool,
    /// Where the last frame drew the open modal (its colour is painted there).
    pub modal_rect: Option<Rect>,
    /// The last left press on an item or row, and when: a second press on
    /// the same one inside [`DOUBLE_CLICK`] is a DOUBLE CLICK, which inserts
    /// it as `i` does (msg 186). `None` forgets it.
    pub last_click: Option<(std::time::Instant, Hit)>,
    /// When the face's own garbage collection last ran on its interval (or
    /// the face opened), and when disk pressure was last looked at.
    pub gc_at: std::time::Instant,
    pub gc_looked: std::time::Instant,
    /// A press on a page item, acted on at its release over the SAME item:
    /// a press dragged off it does nothing (0211 R11). Its item and whether
    /// it was the second of a double click.
    pub item_press: Option<(usize, bool)>,
    /// The last click's reference put in at the bucket's caret: the text
    /// before it (and the caret's byte there) and the text it made, before
    /// its save merged in anything another face wrote. A double click takes
    /// back THAT edit and nothing else (0255 H1), and while it stands, what he
    /// types next goes to the bucket (0255 H2).
    pub click_inserted: Option<ClickInsert>,
    /// The click BEFORE that one, when the last click replaced the reference
    /// it had put in (b964d571 msg 23: "click on an item and then click
    /// immediately on another item"): a double click takes it out too, and
    /// only that — a reference he typed, or one a quote put in, is his and
    /// stays (0258 J2).
    pub click_prior: Option<ClickInsert>,
    /// The item or row the last click opened or closed as a band: a double
    /// click turns it back.
    pub click_toggled: Option<usize>,
    /// The reference a click in the bucket went by — the bucket's text then,
    /// the byte of its `[` and the reference as written — until he edits the
    /// bucket himself: the next page click does not replace it but goes
    /// beside it (0258 J7). Another face's typing does not end it: the
    /// reference is followed through the merge by its anchor (0305 K2).
    pub went_by: Option<(String, usize, String)>,
    /// Where the last frame drew the sidebar and page together (the divider
    /// drag maps a column to a share of this).
    pub clusters_area: Rect,
    /// Where the last frame drew the soup's box and the bucket's together
    /// (the bucket's divider drag maps a row to the bucket's height).
    pub surface_area: Rect,
    /// The input's text width in the last frame: the composer's line motions
    /// are by VISUAL row, so they need it.
    pub input_w: usize,
    /// Where the bucket's text was drawn in the last frame, and the first
    /// visual row shown: a click there places the caret (0220 E6).
    pub input_at: (ratatui::layout::Rect, usize),
    /// The bucket's first visual row shown, kept frame to frame: it moves
    /// only when the caret would leave the box (0225 F3).
    pub bucket_top: usize,
    /// A confirmation; cleared by the next key, press or scroll.
    pub notice: Option<String>,
    /// The stale-state line last said ([`App::stale_notice`]), so a re-read
    /// that finds the same dangling entries says nothing again (REL-3.S2b).
    stale_said: Option<String>,
    /// A standing error (a failed write), shown until the next success.
    pub error: Option<String>,
    pub quit: bool,
    /// False on the `--dump` path: nothing is written.
    pub persist: bool,
    /// The cauldron's settings, for this environment.
    pub settings: Settings,
    /// Which bands of the cauldron's Config tab are open.
    pub snav: Nav,
    /// The cauldron Config's tab: Cauldron or Global (msg 186).
    pub cfg_tab: crate::cauldron::menu::CfgTab,
    /// Which bands of the Status tab he has turned from their default.
    pub stnav: HashSet<String>,
    /// The Status tab's sub-tab (msg 186).
    pub sttab: crate::cauldron::status::StatusTab,
    /// A setting value being typed.
    pub field: Option<Field>,
    /// Why the last change to a setting was refused, shown on its row.
    pub refusal: Option<(Bound, String)>,
    /// A list entry `x` was pressed on once (0220 E4): the list's key and the
    /// entry's index. It goes only at a second `x` on the same entry; any
    /// other key forgets it.
    pub pending_remove: Option<(String, usize)>,
    /// The ripple policy and what the ripples did: the event bus.
    pub rstore: ripples::Store,
    /// How many pages of each event type Status › Bus shows (`show more`
    /// adds one), by type.
    pub bus_pages: std::collections::HashMap<String, usize>,
    /// DEV ONLY — NOT FOR THE PRODUCTION VERSION (msg 235): the titles he
    /// renamed on screen, by the word the code writes.
    pub dev_names: crate::cauldron::store::DevNames,
    /// What other faces on this environment can change, as last read.
    pub watch: store::Watch,
    /// What he has done on the human actions' steps here: the fields
    /// submitted and the steps marked done (never a value he typed).
    pub tickets: crate::cauldron::tickets::Store,
    /// Seedings in flight: each runs OFF the UI loop and lands on a quiet
    /// tick ([`App::poll_seeding`]).
    pub seeding: Vec<PendingSeed>,
    /// When this face last asked whether a seeding shown running lost its face.
    orphans_looked: std::time::Instant,
    /// A new-cluster modal closed before it made anything: its parent, name
    /// and boundaries, restored when he opens one under the same parent
    /// again, so what he typed is never thrown away (F38).
    pub new_cluster_kept: Option<(Option<String>, String, String)>,
    /// The name and boundaries the open new-cluster modal opened with: ← at
    /// the start of a field still holding that (or nothing) leaves the modal.
    new_cluster_opened: (String, String),
}

/// One git read of every registered repo: each repo's worktrees.
pub type GitRead = Vec<(String, Vec<crate::cauldron::artifacts::Worktree>)>;

/// What [`App::spawn_seeding`] starts.
struct PendingSeedArgs {
    id: String,
    name: String,
    dir: PathBuf,
    at: String,
    boundaries: String,
    run_id: String,
    items: Vec<crate::cauldron::seeding::Candidate>,
    live: crate::cauldron::seeding::Live,
    policy: crate::cauldron::ripples::Policy,
    lock: store::SeedLock,
}

/// A seeding in flight: the cluster was made and opened at once, marked as
/// seeding; its claims land when the call answers.
pub struct PendingSeed {
    /// The cluster it seeds.
    pub id: String,
    pub name: String,
    /// The soup it was made in (the face may have switched cauldron since).
    pub dir: PathBuf,
    /// When it was made (`HH:MM`).
    pub at: String,
    rx: std::sync::mpsc::Receiver<crate::cauldron::seeding::Seeded>,
    /// The seeding's kernel lock (IS-32): held until its answer is written,
    /// so every other face sees it live; dropped with this, or when the face
    /// dies, after which they see it interrupted.
    _lock: store::SeedLock,
}

impl App {
    /// Open environment `env`, reading its seed from `seeds`. With `persist`
    /// false nothing is ever written (the `--dump` path); what is on disk is
    /// still read, so a dump shows the live draft and the live sends.
    pub fn open(env: Environment, seeds: PathBuf, persist: bool) -> Result<Self> {
        let dir = store::soup_dir(&env)?;
        let soup = store::load_soup(&seeds, &env.name, &dir)?;
        let layout_store = LayoutStore::new(&dir, persist);
        let layout = layout_store.load()?;
        let (repos, repos_adopted) = store::load_repos(&env, &seeds, &soup.forest, persist)?;
        let services = soup.dash.mesh.iter().flat_map(|d| d.services.iter()).map(|s| (s.name.clone(), (s.version.clone(), s.addr.clone()))).collect();
        let global = store::global_dir(&env);
        let settings = Settings::open(&dir, &global, &env.name, settings::seed_path(&seeds, &env.name).as_deref(), settings::global_seed_path(&seeds).as_deref(), persist, services)?;
        let rstore = ripples::Store::open(&seeds, &dir, persist)?;
        // REL-3.S2b: an entry naming nothing in this soup never stops the
        // face opening; it is kept on disk and said below (`stale_notice`).
        let tickets = crate::cauldron::tickets::Store::open(&dir, persist)?;
        let dev_names = crate::cauldron::store::DevNames::load(&store::global_dir(&env), persist)?;
        let mut app = App {
            forest: soup.forest,
            corpus: soup.corpus,
            dash: soup.dash,
            repos,
            messages: soup.messages.into_iter().collect(),
            ctab: CTab::Cauldron,
            // The first cluster tab (8e4fca10 msg 385 moved Details, which
            // the face used to open on, to the end of the bar).
            ktab: KTab::ALL[0],
            bands: HashSet::new(),
            focus: Focus::Tree,
            back_to: Focus::Tree,
            selected: 0,
            on_add_root: false,
            on_below: false,
            collapsed: HashSet::new(),
            aggregate: false,
            scroll: 0,
            item: 0,
            reveal: false,
            rows: [0; 2],
            trace: Vec::new(),
            rnav: Vec::new(),
            rexpanded: HashSet::new(),
            search: String::new(),
            searching: false,
            composer: Composer::new(),
            draft: DraftStore::new(&dir, persist),
            chips: crate::cauldron::dump::Artifacts::new(&dir, persist),
            chip_hits: Vec::new(),
            refs: crate::cauldron::refs::Index::open(&dir, persist).unwrap_or_else(|_| crate::cauldron::refs::Index::fresh(&dir, persist)),
            seed_draft: String::new(),
            arts: Default::default(),
            worktrees: Vec::new(),
            art_toggled: HashSet::new(),
            flash: None,
            hotspot_paths: Vec::new(),
            live_git: false,
            git_rx: None,
            git_at: None,
            sends: SendLog::new(&dir, persist),
            ledger: crate::cauldron::ledger::Seed::default(),
            modal: None,
            modal_back: None,
            dropdown: None,
            dropdown_anchor: (0, 1),
            outbox: Vec::new(),
            hits: Vec::new(),
            layout,
            layout_store,
            dragging: false,
            dragging_bucket: false,
            modal_rect: None,
            last_click: None,
            gc_at: std::time::Instant::now(),
            gc_looked: std::time::Instant::now(),
            item_press: None,
            click_inserted: None,
            click_prior: None,
            went_by: None,
            click_toggled: None,
            clusters_area: Rect::default(),
            surface_area: Rect::default(),
            input_w: 80,
            input_at: (ratatui::layout::Rect::default(), 0),
            bucket_top: 0,
            notice: None,
            stale_said: None,
            error: None,
            quit: false,
            persist,
            settings,
            snav: Nav::cauldron(),
            // Config opens on its first tab, this cauldron's (b964d571 msg 23).
            cfg_tab: crate::cauldron::menu::CfgTab::Cauldron,
            stnav: HashSet::new(),
            // Status opens on its first sub-tab, Tokens (b964d571 msg 23).
            sttab: crate::cauldron::status::StatusTab::Ledger,
            field: None,
            refusal: None,
            pending_remove: None,
            rstore,
            bus_pages: Default::default(),
            dev_names,
            watch: store::Watch::now(&dir, &global),
            tickets,
            seeding: Vec::new(),
            orphans_looked: std::time::Instant::now(),
            new_cluster_kept: None,
            new_cluster_opened: (String::new(), String::new()),
            threads: Default::default(),
            env,
            seeds,
        };
        app.settings.pool_ids = app.pool_ids();
        app.settings.root = app.env.root.display().to_string();
        // A retired key a settings file still holds is read, never used, and
        // written back as he wrote it: nothing changes, so nothing is said
        // (0211 R2). Global keys a cauldron's file held move to the global
        // file silently at the first change, for the same reason.
        // This cauldron's own repo registry is the global one's now (msg 235).
        if let Some(said) = repos_adopted.say(&app.env.name).filter(|_| persist) {
            let before = app.notice.take().map_or(String::new(), |n| format!("{n} · "));
            app.notice = Some(format!("{before}{said}"));
        }
        // A line of a settings file this build cannot use is kept and said,
        // never a reason not to open (0220 E3).
        if let Some(first) = app.settings.warnings().first() {
            let n = app.settings.warnings().len();
            let more = if n > 1 { format!(" (and {} more)", n - 1) } else { String::new() };
            let before = app.notice.take().map_or(String::new(), |n| format!("{n} · "));
            app.notice = Some(format!("{before}{first}{more}"));
        }
        // State this environment kept that names items no longer here — a
        // ticket step of a seed REL-3.S2 took away, a seed repo he
        // unregistered — is kept on disk, skipped, and said once (REL-3.S2b).
        app.stale_said = app.stale_notice();
        if let Some(said) = app.stale_said.clone() {
            let before = app.notice.take().map_or(String::new(), |n| format!("{n} · "));
            app.notice = Some(format!("{before}{said}"));
        }
        // The face opens where the TUI settings say.
        let none = settings::Ctx::default();
        if let settings::Val::Str(t) = app.settings.get(settings::def("ui.default_tab"), &none).val {
            if let Some(t) = CTab::parse(&t) {
                app.set_ctab(t);
            }
        }
        app.aggregate = app.settings.get(settings::def("ui.aggregate_default"), &none).val.as_bool();
        app.sync_whole()?;
        app.arts = crate::cauldron::artifacts::load(&app.seeds, &app.env.name, &store::soup_dir(&app.env)?)?;
        app.threads = crate::cauldron::threads::Threads::new(crate::cauldron::threads::load(&app.seeds, &app.env.name)?);
        app.worktrees = app.arts.worktrees.clone();
        let seed = store::DumpSeed::load(&app.seeds, &app.env.name)?;
        app.seed_draft = seed.draft;
        app.sends.seed(seed.dumps);
        app.ledger = crate::cauldron::ledger::Seed::load(&app.seeds, &app.env.name, &store::soup_dir(&app.env)?)?;
        let mut text = app.draft.load()?;
        if !app.draft.exists() {
            text = app.seed_draft.clone();
            app.draft.start_from(&text);
        }
        app.composer = Composer::from_text(&text);
        // What a cauldron's buckets held before there was ONE (one per
        // cluster, design 0138; one per environment before that) comes into
        // the one bucket: nothing he wrote is lost (msg 235). With no bucket
        // yet, the old texts replace the seed's; with one, whatever an older
        // face wrote into them since comes in too (0217 B1). Said by
        // nothing: the text is simply there (task 0209 D13).
        if !app.draft.exists() && store::readopt(&dir, "", &store::Adopted::default()).0.is_some() {
            app.composer = Composer::from_text("");
        }
        app.readopt_legacy();
        app.sends.poll()?;
        if let Err(e) = app.sends.adopt_old_log() {
            app.error = Some(format!("taking in an older face's dumps: {e:#}"));
        }
        Ok(app)
    }

    // ── cauldrons ──────────────────────────────────────────────────────────

    /// Move this face into the cauldron `name`: its soup, its bucket, its
    /// sends. Switching creates nothing on disk. Only the DATA changes (msg
    /// 235: "We're switching the cauldron that we're in, which is just
    /// changing the data. It's not supposed to be like a whole new tab"):
    /// the tab he is on, every sub-tab, and every band he opened or closed
    /// stay as they were — Config's, Status's, a cluster page's, the
    /// sidebar's.
    pub fn switch_env(&mut self, name: &str) -> Result<()> {
        let env = self.env.for_name(name)?;
        let mut next = App::open(env, self.seeds.clone(), self.persist)?;
        next.ctab = self.ctab;
        next.ktab = self.ktab;
        next.sttab = self.sttab;
        next.cfg_tab = self.cfg_tab;
        next.bands = std::mem::take(&mut self.bands);
        next.snav = std::mem::replace(&mut self.snav, Nav::cauldron());
        next.stnav = std::mem::take(&mut self.stnav);
        next.art_toggled = std::mem::take(&mut self.art_toggled);
        next.bus_pages = std::mem::take(&mut self.bus_pages);
        next.rows = self.rows;
        next.aggregate = self.aggregate;
        // The whole cauldron comes with `contents below` (b964d571 msg 23).
        next.sync_whole()?;
        let key = |a: &App, g: ClusterId| a.forest.get(g).key.clone();
        let selected = key(self, self.selected);
        next.selected = next.forest.find(&selected).unwrap_or(0);
        let collapsed: Vec<String> = self.collapsed.iter().map(|&g| key(self, g)).collect();
        next.collapsed = collapsed.iter().filter_map(|k| next.forest.find(k)).collect();
        next.focus = self.focus;
        next.back_to = self.back_to;
        next.notice = Some(format!("cauldron → {name}"));
        *self = next;
        Ok(())
    }

    /// The cauldrons dropdown, under the cauldron's name: every cauldron,
    /// whether each is on, then `+`.
    pub fn open_cauldrons(&mut self) {
        match store::list_envs(&self.env) {
            Ok(names) => {
                let entries: Vec<(String, bool)> = names.into_iter().map(|n| {
                    let on = self.cauldron_on(&n);
                    (n, on)
                }).collect();
                let selected = entries.iter().position(|e| e.0 == self.env.name).unwrap_or(0);
                self.dropdown = Some(Dropdown { entries, selected });
            }
            Err(e) => self.error = Some(format!("listing cauldrons: {e:#}")),
        }
    }

    /// Whether cauldron `name` is on: its own `cauldron.enabled`, read from
    /// its settings (this face's, for its own).
    pub fn cauldron_on(&self, name: &str) -> bool {
        let def = settings::def("cauldron.enabled");
        if name == self.env.name {
            return self.settings.get(def, &settings::Ctx::default()).val.as_bool();
        }
        let read = || -> Result<bool> {
            let env = self.env.for_name(name)?;
            let dir = store::soup_dir(&env)?;
            let s = Settings::open(&dir, &store::global_dir(&env), name, settings::seed_path(&self.seeds, name).as_deref(), settings::global_seed_path(&self.seeds).as_deref(), false, Default::default())?;
            Ok(s.get(def, &settings::Ctx::default()).val.as_bool())
        };
        // A cauldron whose settings cannot be read is not said to be off.
        read().unwrap_or(true)
    }

    /// The cauldron enabled or disabled (msg 186, msg 235): its switch is
    /// the FIRST row of Config's last tab, `<name> cauldron`, not the top
    /// bar, and it asks first. ^T is the same confirm, from anywhere.
    pub fn toggle_cauldron(&mut self) {
        self.ask_enable();
    }

    pub fn open_palette(&mut self) {
        self.modal = Some(Modal::Palette { scroll: 0 });
    }

    /// Activate entry `i` of the cauldrons dropdown: switch to it, or — the
    /// entry after the last — ask for a new one's name.
    fn pick_cauldron(&mut self, i: usize) {
        let Some(d) = &self.dropdown else { return };
        match d.entries.get(i).map(|e| e.0.clone()) {
            Some(name) => match self.switch_env(&name) {
                Ok(()) => self.dropdown = None,
                Err(e) => self.error = Some(format!("{e:#}")),
            },
            None => {
                self.dropdown = None;
                self.modal = Some(Modal::NewCauldron { name: String::new(), error: None });
            }
        }
    }

    /// Keys while the dropdown is open: it takes every key, as a modal does.
    fn dropdown_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(d) = self.dropdown.as_mut() else { return };
        match key.code {
            KeyCode::Esc | KeyCode::Left => self.dropdown = None,
            KeyCode::Char('e') if ctrl => self.dropdown = None,
            KeyCode::Up => d.selected = d.selected.saturating_sub(1),
            // The entry after the last is `+`.
            KeyCode::Down => d.selected = (d.selected + 1).min(d.entries.len()),
            KeyCode::Home => d.selected = 0,
            KeyCode::End => d.selected = d.entries.len(),
            KeyCode::Enter | KeyCode::Right => {
                let i = d.selected;
                self.pick_cauldron(i);
            }
            _ => {}
        }
    }

    /// Create the cauldron being named, move into it, and land on its
    /// Settings.
    fn create_cauldron(&mut self) {
        let Some(Modal::NewCauldron { name, .. }) = &self.modal else { return };
        let name = name.trim().to_string();
        let made = if name.is_empty() {
            Err(anyhow::anyhow!("a cauldron needs a name"))
        } else if !self.persist {
            Err(anyhow::anyhow!("nothing is created by --dump"))
        } else {
            store::create_env(&self.env, &name).and_then(|_| self.switch_env(&name))
        };
        match made {
            Ok(()) => {
                self.modal = None;
                self.set_ctab(CTab::Config);
                self.notice = Some(format!("created {name}"));
            }
            Err(e) => {
                if let Some(Modal::NewCauldron { error, .. }) = &mut self.modal {
                    *error = Some(format!("{e:#}"));
                }
            }
        }
    }

    /// The `+` in the sidebar's header (`parent` none) or beside a cluster:
    /// ask for the new cluster's name and its boundaries.
    /// What he typed into one that closed unmade comes back.
    pub fn open_new_cluster(&mut self, parent: Option<ClusterId>) {
        // Under the whole cauldron is the root.
        let parent = parent.filter(|&g| !self.forest.get(g).whole).map(|g| self.forest.get(g).key.clone());
        let (name, boundaries) = match self.new_cluster_kept.take() {
            Some((p, n, b)) if p == parent => (n, b),
            other => {
                self.new_cluster_kept = other;
                (String::new(), String::new())
            }
        };
        self.new_cluster_opened = (name.clone(), boundaries.clone());
        self.modal = Some(Modal::NewCluster { parent, name, boundaries, at: 0, caret: usize::MAX, error: None });
    }

    /// Close whatever modal is open. A new-cluster modal keeps what was typed
    /// for the next one opened under the same parent.
    pub fn close_modal(&mut self) {
        if matches!(self.modal, Some(Modal::Chip { .. })) {
            return self.keep_chip();
        }
        if let Some(Modal::NewCluster { parent, name, boundaries, .. }) = self.modal.take() {
            if !name.trim().is_empty() || !boundaries.trim().is_empty() {
                self.new_cluster_kept = Some((parent, name, boundaries));
            }
        }
    }

    /// The name of the cluster whose key is `key` (a run records the key),
    /// or the key itself when no cluster has it.
    pub fn cluster_name(&self, key: &str) -> String {
        self.forest.find(key).map_or_else(|| key.to_string(), |g| self.forest.get(g).name.clone())
    }

    /// The recorded run a cluster's BACKGROUND row is: the one it names
    /// (a seeding writes it), else the run of that ripple, on that cluster,
    /// in that state, that took in the same message.
    pub fn background_run(&self, cluster: ClusterId, ix: usize) -> Option<String> {
        let g = self.forest.get(cluster);
        let b = g.background.get(ix)?;
        if let Some(r) = &b.run {
            return self.rstore.run(r).map(|r| r.id.clone());
        }
        self.rstore
            .runs()
            .into_iter()
            .find(|r| r.ripple == b.kind && r.cluster == g.key && r.state == b.state && b.cite.as_ref().is_none_or(|c| *c == r.cite))
            .map(|r| r.id.clone())
    }

    /// Everything the seeding may claim or link: every region, every repo,
    /// every open question and every task not done.
    pub fn seed_candidates(&self) -> Vec<crate::cauldron::seeding::Candidate> {
        use crate::cauldron::seeding::{CandKind, Candidate};
        let mut out: Vec<Candidate> = self
            .corpus
            .regions
            .iter()
            .map(|r| Candidate { kind: CandKind::Region, id: r.id.clone(), text: format!("{} {}", r.handle, r.boundary) })
            .collect();
        out.extend(self.repos.repos.iter().map(|r| Candidate { kind: CandKind::Repo, id: r.name.clone(), text: format!("{} {}", r.name, r.path) }));
        for g in &self.forest.clusters {
            for q in &g.questions {
                out.push(Candidate { kind: CandKind::Node, id: q.written_id(), text: format!("{} {}", q.question, q.why) });
            }
            for t in g.tasks.iter().filter(|t| t.state != TaskState::Done) {
                out.push(Candidate { kind: CandKind::Node, id: t.written_id(), text: t.title.clone() });
            }
        }
        out
    }

    /// The LIVE settings a seeding reads: the type-one roles' models as the
    /// cauldron's `settings.toml` says (a cluster has no settings of its
    /// own, msg 235), and the side it trusts.
    pub fn seed_live(&self) -> crate::cauldron::seeding::Live {
        let defaults = crate::cauldron::seeding::Live::defaults();
        let model = |role: &str, dflt: &str| {
            let ctx = settings::Ctx::role(role);
            match self.settings.get(settings::def("roles.{role}.model"), &ctx).val {
                settings::Val::Str(m) if !m.trim().is_empty() => m,
                _ => dflt.to_string(),
            }
        };
        crate::cauldron::seeding::Live { primary: model("type_one.primary", &defaults.primary), secondary: model("type_one.secondary", &defaults.secondary), trusted: defaults.trusted }
    }

    /// Make the cluster being named — a ROOT, or under the cluster it was
    /// asked for from — select it, close the modal, and START its seeding.
    /// Seeding is the policy's `seed_cluster` ripple: ONE type-one call over
    /// the soup, through the one seam
    /// ([`crate::cauldron::seeding::TypeOneCall`]), with the adapter its
    /// MODEL row selects under the LIVE settings ([`App::seed_live`]). The
    /// call runs OFF the UI loop: the cluster opens at once, its background
    /// row says it is seeding, and its claims land on a quiet tick
    /// ([`App::poll_seeding`]); a timeout or a failure is answered by the
    /// stand-in, with the reason on the run. Refused (and said so in the
    /// modal) for a name with no letter or digit, a name a sibling has, no
    /// boundary conditions, or the headless dump. Its id is MINTED, `X-<n>` (msg 235: "Don't have
    /// a slug ID because that means that we need something that creates the
    /// slug ID. If you want to use an ID, use a minted ID"), never read off
    /// its name.
    fn create_cluster(&mut self) {
        let Some(Modal::NewCluster { parent, name, boundaries, .. }) = &self.modal else { return };
        let (parent, name, boundaries) = (parent.clone(), name.trim().to_string(), boundaries.trim().to_string());
        let id = store::mint_cluster_id(&self.forest);
        let made = if !name.chars().any(char::is_alphanumeric) {
            Err(anyhow::anyhow!("a cluster needs a name with a letter or a digit in it"))
        } else if (0..self.forest.clusters.len()).any(|g| !self.forest.get(g).whole && self.forest.parent_key(g) == parent && self.forest.get(g).name.eq_ignore_ascii_case(&name)) {
            // Its id is minted, so only its name tells it from a sibling.
            Err(anyhow::anyhow!("a cluster named `{name}` already exists here"))
        } else if boundaries.is_empty() {
            Err(anyhow::anyhow!("say what belongs in it: the seeding reads its boundary conditions to find its context"))
        } else if !self.persist {
            Err(anyhow::anyhow!("nothing is created by --dump"))
        } else {
            let now = chrono::Local::now();
            let run_id = format!("r-{}", &uuid::Uuid::new_v4().simple().to_string()[..4]);
            let items = self.seed_candidates();
            let live = self.seed_live();
            let policy = self.rstore.live.clone();
            let asking = crate::cauldron::seeding::will_ask(&policy, &live);
            let at = now.format("%H:%M").to_string();
            let cluster = CreatedCluster {
                id: id.clone(),
                name: name.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
                parent: parent.clone(),
                boundaries: boundaries.clone(),
                regions: Vec::new(),
                repos: Vec::new(),
                links: Vec::new(),
                seeded_by: Some(store::SeededBy { run: run_id.clone(), line: format!("seeding · asking {asking} over {} candidates", items.len()), at: at.clone(), pending: true }),
                unowned: false,
            };
            // The seeding's lock is taken BEFORE `pending` is written, so no
            // face ever sees this seeding pending with nobody holding it.
            let added = {
                let forest = &self.forest;
                store::soup_dir(&self.env).and_then(|dir| {
                    let lock = store::SeedLock::take(&dir, &id, &run_id)?.with_context(|| format!("`{id}` is being made in another face"))?;
                    CreatedClusters::new(&dir).add(cluster, |k| forest.find(k).is_some()).map(|()| (dir, lock))
                })
            };
            added.and_then(|dl| self.reload_soup().map(|()| dl)).map(|(dir, lock)| {
                self.spawn_seeding(PendingSeedArgs { id: id.clone(), name: name.clone(), dir, at, boundaries: boundaries.clone(), run_id, items, live, policy, lock });
                asking
            })
        };
        match made.and_then(|a| self.forest.find(&id).map(|g| (g, a)).with_context(|| format!("`{id}` was not loaded"))) {
            Ok((g, asking)) => {
                self.modal = None;
                self.new_cluster_kept = None;
                self.select(g);
                self.set_ktab(KTab::Details);
                self.focus = Focus::Tree;
                self.notice = Some(format!("created {name} — seeding · asking {asking}"));
            }
            Err(e) => {
                if let Some(Modal::NewCluster { error, .. }) = &mut self.modal {
                    *error = Some(format!("{e:#}"));
                }
            }
        }
    }

    /// Run a seeding's call OFF the UI loop; it lands on a quiet tick
    /// ([`App::poll_seeding`]).
    fn spawn_seeding(&mut self, a: PendingSeedArgs) {
        let (tx, rx) = std::sync::mpsc::channel();
        let PendingSeedArgs { id, name, dir, at, boundaries, run_id, items, live, policy, lock } = a;
        let key = id.clone();
        std::thread::spawn(move || {
            let s = crate::cauldron::seeding::seed(&policy, &live, &crate::cauldron::seeding::LocalMatcher, &boundaries, &items, &key, &run_id, chrono::Local::now());
            let _ = tx.send(s);
        });
        self.seeding.push(PendingSeed { id, name, dir, at, rx, _lock: lock });
    }

    /// The selected cluster's seeding, when it is INTERRUPTED (IS-32): its
    /// soup dir and the cluster as made here.
    fn interrupted_seed(&self, key: &str) -> Result<(PathBuf, CreatedCluster)> {
        let dir = store::soup_dir(&self.env)?;
        let made = CreatedClusters::new(&dir).list()?.into_iter().find(|c| c.id == key).with_context(|| format!("`{key}` was not made here"))?;
        match made.seed_state(&dir, chrono::Utc::now()) {
            store::SeedState::Interrupted(_) => Ok((dir, made)),
            store::SeedState::Running => anyhow::bail!("its seeding is live in another face; nothing was touched"),
            _ => anyhow::bail!("its seeding is not interrupted (it landed or was cleared)"),
        }
    }

    /// RESEED cluster `key`, whose seeding was interrupted (its face died):
    /// the same seeding again — its boundaries, the same line, asking what
    /// the live settings ask — under this face's lock. Of two faces
    /// reseeding at once exactly one wins; the other is told. A live
    /// seeding in another face is never touched.
    pub fn reseed_cluster(&mut self, key: &str) {
        let done = (|| -> Result<String> {
            anyhow::ensure!(self.persist, "nothing is reseeded by --dump");
            let (dir, made) = self.interrupted_seed(key)?;
            let orphan = made.seeded_by.as_ref().map(|b| b.run.clone()).unwrap_or_default();
            let run_id = format!("r-{}", &uuid::Uuid::new_v4().simple().to_string()[..4]);
            let lock = store::SeedLock::take(&dir, key, &run_id)?.context("another face is reseeding it this moment; nothing was touched")?;
            let items = self.seed_candidates();
            let live = self.seed_live();
            let policy = self.rstore.live.clone();
            let asking = crate::cauldron::seeding::will_ask(&policy, &live);
            let at = chrono::Local::now().format("%H:%M").to_string();
            let by = store::SeededBy { run: run_id.clone(), line: format!("seeding · asking {asking} over {} candidates", items.len()), at: at.clone(), pending: true };
            CreatedClusters::new(&dir).reseed(key, &orphan, by, &lock)?;
            self.reload_soup_if(&dir)?;
            self.spawn_seeding(PendingSeedArgs { id: key.to_string(), name: made.name.clone(), dir, at, boundaries: made.boundaries.clone(), run_id, items, live, policy, lock });
            Ok(format!("reseeding {} — its seeding was interrupted · asking {asking}", made.name))
        })();
        match done {
            Ok(n) => self.notice = Some(n),
            Err(e) => self.error = Some(format!("not reseeded: {e:#}")),
        }
    }

    /// CLEAR cluster `key`'s interrupted seeding mark: it stays, with no
    /// claims and no seeding. A live seeding in another face is never
    /// touched.
    pub fn clear_seed(&mut self, key: &str) {
        let done = (|| -> Result<String> {
            anyhow::ensure!(self.persist, "nothing is cleared by --dump");
            let (dir, made) = self.interrupted_seed(key)?;
            let orphan = made.seeded_by.as_ref().map(|b| b.run.clone()).unwrap_or_default();
            let lock = store::SeedLock::take(&dir, key, &orphan)?.context("another face is reseeding it this moment; nothing was touched")?;
            CreatedClusters::new(&dir).clear_seed(key, &orphan, &lock)?;
            drop(lock);
            self.reload_soup_if(&dir)?;
            Ok(format!("cleared {}'s interrupted seeding — it has no claims and is not seeding", made.name))
        })();
        match done {
            Ok(n) => self.notice = Some(n),
            Err(e) => self.error = Some(format!("not cleared: {e:#}")),
        }
    }

    /// A seeding shown running whose face has died since is shown
    /// interrupted (IS-32): nothing in `clusters.json` changes when a face
    /// dies, so the file watch alone would never notice. Asked of the kernel
    /// at most once a second, and only while a seeding is shown running.
    fn look_for_orphans(&mut self) {
        if self.orphans_looked.elapsed() < std::time::Duration::from_secs(1) {
            return;
        }
        self.orphans_looked = std::time::Instant::now();
        let shown = self.forest.clusters.iter().any(|g| g.made_here && g.background.iter().any(|b| b.kind == "seed" && b.state == "running"));
        if !shown {
            return;
        }
        let Ok(dir) = store::soup_dir(&self.env) else { return };
        let Ok(list) = CreatedClusters::new(&dir).list() else { return };
        let now = chrono::Utc::now();
        if list.iter().any(|c| matches!(c.seed_state(&dir, now), store::SeedState::Interrupted(_)) && self.forest.find(&c.id).is_some_and(|g| self.forest.get(g).background.iter().any(|b| b.kind == "seed" && b.state == "running"))) {
            if let Err(e) = self.adopt_soup() {
                self.error = Some(format!("re-reading the clusters: {e:#}"));
            }
        }
    }

    /// Land every seeding whose call has answered: write its claims and its
    /// run onto the cluster it seeds, record the run, and say so. Called on
    /// every quiet tick (from [`App::sync`]); never blocks.
    pub fn poll_seeding(&mut self) {
        let mut i = 0;
        while i < self.seeding.len() {
            match self.seeding[i].rx.try_recv() {
                Ok(seeded) => {
                    let p = self.seeding.remove(i);
                    self.land_seeding(p, seeded);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => i += 1,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    let p = self.seeding.remove(i);
                    let why = "the seeding thread ended without an answer".to_string();
                    let _ = CreatedClusters::new(&p.dir).seeded(&p.id, Default::default(), None);
                    self.error = Some(format!("{}: not seeded: {why}", p.name));
                    let _ = self.reload_soup_if(&p.dir);
                }
            }
        }
    }

    /// Wait (up to `timeout`) for every seeding in flight to land. The live
    /// loop never calls this; tests and headless proofs do.
    pub fn wait_seeding(&mut self, timeout: std::time::Duration) -> bool {
        let t0 = std::time::Instant::now();
        while !self.seeding.is_empty() && t0.elapsed() < timeout {
            self.poll_seeding();
            if !self.seeding.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        self.seeding.is_empty()
    }

    fn land_seeding(&mut self, p: PendingSeed, seeded: crate::cauldron::seeding::Seeded) {
        let by = seeded.run.as_ref().map(|r| store::SeededBy { run: r.id.clone(), line: r.line.clone(), at: p.at.clone(), pending: false });
        let wrote = CreatedClusters::new(&p.dir).seeded(&p.id, crate::cauldron::store::Claims { regions: seeded.regions.clone(), repos: seeded.repos.clone(), links: seeded.links.clone() }, by);
        // The run is recorded even when the cluster went meanwhile: it ran.
        if let Some(run) = seeded.run.clone() {
            let rec = if self.soup_dir_is(&p.dir) {
                self.rstore.record_run(run)
            } else {
                ripples::Store::open(&self.seeds, &p.dir, self.persist).and_then(|mut s| s.record_run(run))
            };
            if let Err(e) = rec {
                self.error = Some(format!("the seeding run NOT recorded: {e:#}"));
            }
        }
        if let Err(e) = wrote {
            self.error = Some(format!("{}: the seeding answered but its claims were NOT written: {e:#}", p.name));
        }
        if let Err(e) = self.reload_soup_if(&p.dir) {
            self.error = Some(format!("re-reading the clusters: {e:#}"));
        }
        self.notice = Some(match (&seeded.run, &seeded.skipped) {
            (Some(r), _) => format!("seeded {} — {}", p.name, r.line),
            (None, Some(why)) => format!("{} — not seeded: {why}", p.name),
            (None, None) => format!("seeded {}", p.name),
        });
    }

    fn soup_dir_is(&self, dir: &Path) -> bool {
        store::soup_dir(&self.env).is_ok_and(|d| d == dir)
    }

    /// Re-read the soup, keeping where he is, when `dir` is this face's.
    fn reload_soup_if(&mut self, dir: &Path) -> Result<()> {
        if self.soup_dir_is(dir) { self.adopt_soup() } else { Ok(()) }
    }

    /// Read this environment's soup again (after a cluster was made, renamed,
    /// moved or deleted, or a repo registered), keeping where he is. A cluster
    /// made under another one lands inside the tree, so ids shift: the
    /// selection and the collapsed clusters are carried across by KEY, and
    /// anything opened on the page is closed.
    pub(crate) fn reload_soup(&mut self) -> Result<()> {
        self.load_soup_keeping(false)
    }

    /// The soup changed under this face (another face wrote it). When every
    /// cluster and every repo is still where it was — a rename, a seeding's
    /// claims — nothing he has open moves; otherwise this is
    /// [`App::reload_soup`].
    pub(crate) fn adopt_soup(&mut self) -> Result<()> {
        self.load_soup_keeping(true)
    }

    fn load_soup_keeping(&mut self, keep_page: bool) -> Result<()> {
        let dir = store::soup_dir(&self.env)?;
        let soup = store::load_soup_rooted(&self.seeds, &self.env.name, &dir, self.aggregate)?;
        let (repos, adopted) = store::load_repos(&self.env, &self.seeds, &soup.forest, self.persist)?;
        if let Some(said) = adopted.say(&self.env.name) {
            self.notice = Some(said);
        }
        // A cauldron's own seed carries its artifacts beside its clusters
        // (`store::own_seed`): an edit to either is one soup change.
        let arts = crate::cauldron::artifacts::load(&self.seeds, &self.env.name, &dir)?;
        let keys = |f: &Forest| f.clusters.iter().map(|g| g.key.clone()).collect::<Vec<_>>();
        let names = |r: &Repos| r.repos.iter().map(|x| x.name.clone()).collect::<Vec<_>>();
        let same = keys(&self.forest) == keys(&soup.forest) && names(&self.repos) == names(&repos);
        let selected = self.forest.get(self.selected).key.clone();
        let collapsed: Vec<String> = self.collapsed.iter().map(|&g| self.forest.get(g).key.clone()).collect();
        self.repos = repos;
        self.arts = arts;
        self.forest = soup.forest;
        self.corpus = soup.corpus;
        self.dash = soup.dash;
        self.messages = soup.messages.into_iter().collect();
        self.selected = self.forest.find(&selected).unwrap_or(0);
        self.collapsed = collapsed.iter().filter_map(|k| self.forest.find(k)).collect();
        self.say_stale();
        if keep_page && same {
            return Ok(());
        }
        self.reset_page();
        Ok(())
    }

    /// Keys while a modal is open — `docs/navigation.md` rule 8. The modal
    /// takes every key; ↑ ↓ move inside it, → acts as Enter, ← escapes it as
    /// Esc does.
    fn modal_key(&mut self, key: KeyEvent) {
        if matches!(self.modal, Some(Modal::Settings { .. })) {
            return self.row_modal_key(key);
        }
        if matches!(self.modal, Some(Modal::Chip { .. })) {
            return self.chip_modal_key(key);
        }
        if matches!(self.modal, Some(Modal::Bus { .. })) {
            return self.bus_modal_key(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(modal) = self.modal.as_mut() else { return };
        match modal {
            // Routed to `row_modal_key`, `chip_modal_key` and `bus_modal_key` above.
            Modal::Settings { .. } | Modal::Chip { .. } | Modal::Bus { .. } => {}
            Modal::Help { .. } => match key.code {
                KeyCode::Esc | KeyCode::Left | KeyCode::Enter | KeyCode::Right | KeyCode::Char('h' | 'q') => self.modal = None,
                _ => {}
            },
            Modal::Artifact { path, lines, scroll, .. } => {
                let max = lines.len().saturating_sub(1);
                match key.code {
                    KeyCode::Esc | KeyCode::Left | KeyCode::Enter | KeyCode::Char('q') => self.modal = None,
                    KeyCode::Up => *scroll = scroll.saturating_sub(1),
                    KeyCode::Down => *scroll = (*scroll + 1).min(max),
                    KeyCode::PageUp => *scroll = scroll.saturating_sub(20),
                    KeyCode::PageDown | KeyCode::Char(' ') => *scroll = (*scroll + 20).min(max),
                    KeyCode::Home => *scroll = 0,
                    KeyCode::End => *scroll = max,
                    KeyCode::Char('o') => {
                        let p = path.clone();
                        self.modal = None;
                        self.open_path(p);
                    }
                    _ => {}
                }
            }
            // Two fields: the name, then the boundaries (several lines). ↑ ↓
            // move between them; Enter on the name goes on to the
            // boundaries, Enter on the boundaries makes the cluster; ⌥Enter,
            // ⇧Enter or ^J start a new line there.
            //
            // Each field has a caret (F38): ← and → move in the text, Home and
            // End to the line's ends, ↑ ↓ move between the boundaries' lines
            // and, past the first, up to the name. ← at the start of a field
            // that is empty or as it opened leaves the modal; → at the end
            // acts as Enter. Leaving keeps what was typed (`close_modal`).
            Modal::NewCluster { name, boundaries, at, caret, error, .. } => {
                let alt = key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
                let opened = if *at == 0 { &self.new_cluster_opened.0 } else { &self.new_cluster_opened.1 };
                let field = if *at == 0 { name } else { boundaries };
                let c = (*caret).min(field.len());
                *caret = c;
                let line_start = field[..c].rfind('\n').map_or(0, |i| i + 1);
                let line_end = field[c..].find('\n').map_or(field.len(), |i| c + i);
                let col = field[line_start..c].chars().count();
                // The byte offset of column `col` on the line starting at `from`.
                let at_col = |f: &str, from: usize, col: usize| -> usize {
                    let end = f[from..].find('\n').map_or(f.len(), |i| from + i);
                    f[from..end].char_indices().nth(col).map_or(end, |(i, _)| from + i)
                };
                match key.code {
                    KeyCode::Esc => self.close_modal(),
                    KeyCode::Left if c == 0 => {
                        if field.is_empty() || field == opened {
                            self.close_modal();
                        }
                    }
                    KeyCode::Left => *caret = field[..c].char_indices().next_back().map_or(0, |(i, _)| i),
                    KeyCode::Right if c < field.len() => *caret = c + field[c..].chars().next().map_or(0, char::len_utf8),
                    KeyCode::Home => *caret = line_start,
                    KeyCode::End => *caret = line_end,
                    KeyCode::Up | KeyCode::BackTab if *at == 1 && line_start > 0 && key.code == KeyCode::Up => {
                        let prev = field[..line_start - 1].rfind('\n').map_or(0, |i| i + 1);
                        *caret = at_col(field, prev, col);
                    }
                    KeyCode::Up | KeyCode::BackTab => {
                        *at = 0;
                        *caret = usize::MAX;
                    }
                    KeyCode::Down if *at == 1 && line_end < field.len() => *caret = at_col(field, line_end + 1, col),
                    KeyCode::Down | KeyCode::Tab => {
                        if *at == 0 {
                            *at = 1;
                            *caret = usize::MAX;
                        }
                    }
                    KeyCode::Enter if alt && *at == 1 => {
                        field.insert(c, '\n');
                        *caret = c + 1;
                    }
                    KeyCode::Char('j') if ctrl && *at == 1 => {
                        field.insert(c, '\n');
                        *caret = c + 1;
                    }
                    KeyCode::Enter | KeyCode::Right if *at == 0 => {
                        *at = 1;
                        *caret = usize::MAX;
                    }
                    KeyCode::Enter | KeyCode::Right => self.create_cluster(),
                    KeyCode::Backspace if c > 0 => {
                        let from = field[..c].char_indices().next_back().map_or(0, |(i, _)| i);
                        field.replace_range(from..c, "");
                        *caret = from;
                        *error = None;
                    }
                    KeyCode::Delete if c < field.len() => {
                        let to = c + field[c..].chars().next().map_or(0, char::len_utf8);
                        field.replace_range(c..to, "");
                        *error = None;
                    }
                    KeyCode::Char('u') if ctrl => {
                        field.replace_range(line_start..c, "");
                        *caret = line_start;
                    }
                    KeyCode::Char(ch) if !ctrl => {
                        field.insert(c, ch);
                        *caret = c + ch.len_utf8();
                        *error = None;
                    }
                    _ => {}
                }
            }
            // A name field: the letters are text, Backspace edits, and the
            // arrows still mean what they mean in every modal.
            Modal::Confirm { .. } => match key.code {
                KeyCode::Esc | KeyCode::Left => self.modal = None,
                KeyCode::Enter | KeyCode::Right => self.finish_confirm(),
                _ => {}
            },
            Modal::NewCauldron { name, error }
            | Modal::AddAccount { name, error, .. }
            | Modal::AddItem { name, error, .. }
            | Modal::Name { name, error, .. } => match key.code {
                KeyCode::Esc | KeyCode::Left => match self.modal {
                    // One level back: to the cauldrons it was opened from.
                    Some(Modal::NewCauldron { .. }) => {
                        self.modal = None;
                        self.open_cauldrons();
                    }
                    _ => self.modal = None,
                },
                KeyCode::Enter | KeyCode::Right => match self.modal {
                    Some(Modal::NewCauldron { .. }) => self.create_cauldron(),
                    Some(Modal::Name { .. }) => self.finish_name(),
                    _ => self.add_named(),
                },
                KeyCode::Backspace => {
                    name.pop();
                    *error = None;
                }
                KeyCode::Char('u') if ctrl => name.clear(),
                KeyCode::Char(c) if !ctrl => {
                    name.push(c);
                    *error = None;
                }
                _ => {}
            },
            Modal::Picker { options, selected, .. } => match key.code {
                KeyCode::Esc | KeyCode::Left => self.modal = None,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(options.len().saturating_sub(1)),
                KeyCode::Home => *selected = 0,
                KeyCode::End => *selected = options.len().saturating_sub(1),
                KeyCode::Enter | KeyCode::Right => {
                    let i = *selected;
                    self.pick(i);
                }
                _ => {}
            },
            Modal::Palette { scroll } => match key.code {
                KeyCode::Up => *scroll = scroll.saturating_sub(1),
                KeyCode::Down => *scroll = (*scroll + 1).min(KEYMAP.len().saturating_sub(1)),
                KeyCode::Esc | KeyCode::Left | KeyCode::Enter | KeyCode::Right | KeyCode::Char('?') => {
                    self.modal = None
                }
                KeyCode::Char('/' | '7' | '_') if ctrl => self.modal = None,
                _ => {}
            },
            // DEV ONLY — NOT FOR THE PRODUCTION VERSION (msg 235): a title's
            // new word. Enter keeps it (an empty one goes back to the code's
            // word); Esc or ← at an empty field keeps it as it was.
            Modal::DevRename { key: k, name } => match key.code {
                KeyCode::Esc => self.modal = None,
                KeyCode::Left if name.is_empty() => self.modal = None,
                KeyCode::Enter | KeyCode::Right => {
                    let (k, n) = (k.clone(), name.trim().to_string());
                    self.modal = None;
                    if let Err(e) = self.dev_names.set(&k, &n) {
                        self.error = Some(format!("the name NOT kept: {e:#}"));
                    }
                }
                KeyCode::Backspace => {
                    name.pop();
                }
                KeyCode::Char('u') if ctrl => name.clear(),
                KeyCode::Char(c) if !ctrl => name.push(c),
                _ => {}
            },
        }
    }

    /// DEV ONLY — NOT FOR THE PRODUCTION VERSION (msg 235: "it should say in
    /// the code, this should not make it into the production version. This
    /// is just for the developer"): what title `word` is called on screen,
    /// after any rename he tried.
    pub fn named(&self, word: &str) -> String {
        self.dev_names.get(word).unwrap_or(word).to_string()
    }

    /// DEV ONLY — NOT FOR THE PRODUCTION VERSION: a double click on title
    /// `word` opens its rename modal.
    pub fn dev_rename(&mut self, word: &str) {
        if !crate::cauldron::designs::DEV_RENAME {
            return;
        }
        self.modal = Some(Modal::DevRename { key: word.to_string(), name: self.named(word) });
    }

    // ── live sync with other faces on this environment ─────────────────────

    /// Adopt a draft another face wrote, read any new sends, and re-read
    /// whatever else another face changed — the clusters made and the repos
    /// registered (the soup), the settings, the ripple policy and its runs,
    /// the human actions' steps — keeping where he is. Called by the live loop on every
    /// quiet tick; tests call it to stand in for the tick.
    pub fn sync(&mut self) {
        self.gc_tick();
        self.poll_seeding();
        self.look_for_orphans();
        self.poll_git();
        if let Err(e) = self.sends.poll() {
            self.error = Some(format!("reading sends: {e:#}"));
        }
        match self.sends.adopt_old_log() {
            Ok((0, 0)) => {}
            Ok((n, bad)) => {
                let lost = if bad > 0 { format!(" ({bad} line(s) it could not read stay in sends.jsonl)") } else { String::new() };
                self.notice = Some(format!("{n} dump(s) an older face sent came into the log{lost}"));
            }
            Err(e) => self.error = Some(format!("taking in an older face's dumps: {e:#}")),
        }
        match self.draft.poll(&self.composer.text(), crate::cauldron::dump::caret_offset(&self.composer)) {
            Ok(merged) => self.take_merged(merged),
            Err(e) => self.error = Some(format!("reading the bucket: {e:#}")),
        }
        // What another face put in, and where from (8e4fca10 msg 385).
        if let Err(e) = self.refs.poll(&self.composer.text()) {
            self.error = Some(format!("reading the references' index: {e:#}"));
        }
        // An older face still running writes the old buckets (0217 B1): what
        // it wrote comes into this one.
        if let Ok(dir) = store::soup_dir(&self.env) {
            let now = store::Sig::of(&store::legacy_paths(&dir));
            if now != self.watch.legacy {
                self.watch.legacy = now;
                if self.readopt_legacy() {
                    self.notice = Some("text an older face wrote came into the bucket".into());
                }
            }
        }
        if let Err(e) = self.dev_names.poll() {
            self.error = Some(format!("reading the dev names: {e:#}"));
        }
        let Ok(dir) = store::soup_dir(&self.env) else { return };
        // Each part is re-read only when its files changed; a part that fails
        // to read (another face mid-way through a multi-file write) keeps its
        // old signature, so the next tick tries again.
        let now = store::Sig::of(&store::soup_paths(&dir, &store::global_dir(&self.env)));
        if now != self.watch.soup {
            match self.adopt_soup() {
                Ok(()) => self.watch.soup = now,
                Err(e) => self.error = Some(format!("re-reading the clusters: {e:#}")),
            }
        }
        let now = store::Sig::of(&store::settings_paths(&dir, &store::global_dir(&self.env)));
        if now != self.watch.settings {
            match self.reload_settings() {
                Ok(()) => self.watch.settings = now,
                Err(e) => self.error = Some(format!("re-reading the settings: {e:#}")),
            }
        }
        let now = store::Sig::of(&store::ripples_paths(&dir));
        if now != self.watch.ripples {
            match ripples::Store::open(&self.seeds, &dir, self.persist) {
                Ok(s) => {
                    self.rstore = s;
                    self.watch.ripples = now;
                }
                Err(e) => self.error = Some(format!("re-reading the ripples: {e:#}")),
            }
        }
        let now = store::Sig::of(&store::tickets_paths(&dir));
        if now != self.watch.tickets {
            match self.tickets.reload() {
                Ok(()) => {
                    self.watch.tickets = now;
                    self.say_stale();
                }
                Err(e) => self.error = Some(format!("re-reading the human actions' steps: {e:#}")),
            }
        }
    }

    /// The one quiet line for state this environment kept that names items
    /// no longer here (REL-3.S2b): `tickets.json` entries of no human action
    /// in the soup, and seed repos `repos.json` unregisters that no seed
    /// registers; and (REL-3.S2c) a cluster's repo claims, focus and edges
    /// that name a repo or node no registered repo holds; and (REL-3.S2d)
    /// every other reference to an item that is not there. `None` when
    /// there is none. Nothing is dropped from disk.
    pub fn stale_notice(&self) -> Option<String> {
        let mut parts: Vec<String> = self.tickets.dangling_notice(&self.forest).into_iter().collect();
        match self.repos.stale_unregistered.len() {
            0 => {}
            1 => parts.push("1 unregistered repo no longer exists".into()),
            n => parts.push(format!("{n} unregistered repos no longer exist")),
        }
        match self.repos.stale_links.len() {
            0 => {}
            1 => parts.push("1 cluster link to a repo no longer exists".into()),
            n => parts.push(format!("{n} cluster links to repos no longer exist")),
        }
        // REL-3.S2d: every other reference the soup or a repo's blueprint
        // makes to an item that is not there — a region, a cluster made
        // under one, a task, a milestone, a question, a human action, an
        // objective, a directive, a blueprint node.
        match self.forest.stale.len() + self.corpus.stale.len() + self.repos.stale_refs.len() {
            0 => {}
            1 => parts.push("1 reference to an item that no longer exists".into()),
            n => parts.push(format!("{n} references to items that no longer exist")),
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    /// After a re-read: say the stale-state line when it differs from the
    /// one last said, never over a notice already standing.
    fn say_stale(&mut self) {
        let now = self.stale_notice();
        if now != self.stale_said && self.notice.is_none() {
            self.notice = now.clone();
            self.stale_said = now;
        }
    }

    /// Take into the bucket whatever the old buckets hold that it does not
    /// ([`store::readopt`]), with their chips' bodies, and record what was
    /// taken in once the bucket is saved. Returns whether text came in.
    /// The old files are never touched.
    pub fn readopt_legacy(&mut self) -> bool {
        let Ok(dir) = store::soup_dir(&self.env) else { return false };
        // With no bucket yet, every old text comes in whole, whatever an
        // earlier record says.
        let record = if self.draft.exists() { store::Adopted::load(&dir) } else { store::Adopted::default() };
        let text = self.composer.text();
        let (add, glued, next) = store::readopt(&dir, &text, &record);
        let came = add.is_some();
        if let Some(add) = add {
            if self.persist {
                if let Err(e) = crate::cauldron::dump::adopt_chips(&dir) {
                    self.error = Some(format!("old bucket NOT taken in: {e:#}"));
                    return false;
                }
            }
            if text.trim().is_empty() {
                self.composer = Composer::from_text(&add);
            } else {
                let at = crate::cauldron::dump::caret_offset(&self.composer);
                // An older face typing on goes on the line it was typing on
                // (0225 F5); anything else is a paragraph of its own.
                let joined = if glued {
                    let add = if text.ends_with(char::is_whitespace) { add.trim_start() } else { add.as_str() };
                    format!("{text}{add}")
                } else {
                    format!("{}\n\n{add}", text.trim_end_matches('\n'))
                };
                self.composer = Composer::from_text(&joined);
                crate::cauldron::dump::set_caret(&mut self.composer, at);
            }
            match self.draft.save(&self.composer.text(), crate::cauldron::dump::caret_offset(&self.composer)) {
                Ok(merged) => self.take_merged(merged),
                Err(e) => {
                    self.error = Some(format!("bucket NOT saved: {e:#}"));
                    return came;
                }
            }
        }
        if self.persist && next != record {
            if let Err(e) = next.save(&dir) {
                self.error = Some(format!("recording the old buckets taken in: {e:#}"));
            }
        }
        came
    }

    /// The worktrees from git, for a face that reads it (`live_git`): one
    /// read of every registered repo at a time, OFF the UI thread, at most
    /// once a minute; the frame shows the last result and never waits on it
    /// (design 0146 §5). A repo git cannot read keeps its seeded worktrees.
    pub fn poll_git(&mut self) {
        if !self.live_git {
            return;
        }
        if let Some(rx) = &self.git_rx {
            match rx.try_recv() {
                Ok(read) => {
                    for (repo, wts) in read {
                        self.worktrees.retain(|w| w.repo != repo);
                        self.worktrees.extend(wts);
                    }
                    self.git_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.git_rx = None,
            }
            return;
        }
        if self.git_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(60)) {
            return;
        }
        self.git_at = Some(std::time::Instant::now());
        let repos: Vec<(String, String)> = self.repos.repos.iter().filter(|r| Path::new(&r.path).join(".git").exists()).map(|r| (r.name.clone(), r.path.clone())).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        self.git_rx = Some(rx);
        std::thread::spawn(move || {
            let read: GitRead = repos.into_iter().filter_map(|(n, p)| crate::cauldron::artifacts::read_worktrees(&n, &p, "main").map(|w| (n, w))).collect();
            let _ = tx.send(read);
        });
    }

    /// Re-read the settings files (the cauldron's, the global ones, the
    /// accounts and pools added here, the flip log).
    pub fn reload_settings(&mut self) -> Result<()> {
        let dir = store::soup_dir(&self.env)?;
        let services = self.dash.mesh.iter().flat_map(|d| d.services.iter()).map(|s| (s.name.clone(), (s.version.clone(), s.addr.clone()))).collect();
        let mut s = Settings::open(&dir, &store::global_dir(&self.env), &self.env.name, settings::seed_path(&self.seeds, &self.env.name).as_deref(), settings::global_seed_path(&self.seeds).as_deref(), self.persist, services)?;
        // An endpoint being added is in no file: it lives in this face
        // across a re-read (one this face's own save set off, too).
        let mid = format!(".{}.", settings::DRAFT_EP);
        s.global.vals.extend(self.settings.global.vals.iter().filter(|(k, _)| k.contains(&mid)).map(|(k, v)| (k.clone(), v.clone())));
        s.cauldron.vals.extend(self.settings.cauldron.vals.iter().filter(|(k, _)| k.contains(&mid)).map(|(k, v)| (k.clone(), v.clone())));
        std::mem::swap(&mut self.settings, &mut s);
        self.settings.pool_ids = self.pool_ids();
        self.settings.root = self.env.root.display().to_string();
        Ok(())
    }

    fn save_draft(&mut self) {
        self.save_draft_now();
    }

    /// Take back the edit a click made ([`ClickInsert`]) from the bucket as
    /// it is NOW: a three-way merge with the text it made as the base, so
    /// whatever another face typed since — merged in by that click's own save
    /// or after — stays, and only the reference (and what it replaced) moves.
    fn undo_click(&mut self, c: ClickInsert) {
        let (text, at) = store::merge3(&c.made, &c.before, &self.composer.text(), c.caret);
        self.composer = Composer::from_text(&text);
        crate::cauldron::dump::set_caret(&mut self.composer, at);
        self.save_draft();
    }

    // ── the tree ───────────────────────────────────────────────────────────

    /// The tree rows on screen: pre-order, skipping anything under a
    /// collapsed cluster.
    pub fn visible(&self) -> Vec<ClusterId> {
        let mut out = Vec::new();
        let mut skip_deeper_than: Option<usize> = None;
        for (id, g) in self.forest.clusters.iter().enumerate() {
            if let Some(d) = skip_deeper_than {
                if g.depth > d {
                    continue;
                }
                skip_deeper_than = None;
            }
            out.push(id);
            if self.collapsed.contains(&id) && !g.children.is_empty() {
                skip_deeper_than = Some(g.depth);
            }
        }
        out
    }

    pub fn is_expanded(&self, id: ClusterId) -> bool {
        !self.forest.get(id).children.is_empty() && !self.collapsed.contains(&id)
    }

    pub fn select(&mut self, id: ClusterId) {
        self.on_add_root = false;
        self.on_below = false;
        if id != self.selected {
            self.selected = id;
            self.scroll = 0;
            self.item = 0;
            self.reset_page();
            self.field = None;
        }
        // Selecting a cluster inside a collapsed one opens the way to it.
        let mut cur = self.forest.get(id).parent;
        while let Some(p) = cur {
            self.collapsed.remove(&p);
            cur = self.forest.get(p).parent;
        }
    }

    /// Move the sidebar's cursor. The row above the first cluster is the
    /// `+` in the header (msg 165: "in the surfaces sidebar header, we have
    /// the word surfaces on the left, and then on the right, we have a
    /// plus"). False when it could not move (it was at that edge).
    ///
    /// The row after the last cluster is the switch at the sidebar's foot,
    /// `Aggregated clusters` (b964d571 msg 23).
    fn tree_move(&mut self, delta: isize) -> bool {
        let vis = self.visible();
        let at = if self.on_add_root {
            0
        } else if self.on_below {
            vis.len() + 1
        } else {
            vis.iter().position(|&g| g == self.selected).map_or(1, |p| p + 1)
        } as isize;
        let to = (at + delta).clamp(0, vis.len() as isize + 1) as usize;
        if to as isize == at {
            return false;
        }
        match to.checked_sub(1).map(|i| vis.get(i)) {
            Some(Some(&g)) => self.select(g),
            Some(None) => {
                self.on_add_root = false;
                self.on_below = true;
            }
            None => {
                self.on_below = false;
                self.on_add_root = true;
            }
        }
        true
    }

    /// ⌥↑ / ⌥↓ in the sidebar: the previous / next ROOT cluster — the sidebar's
    /// sections.
    fn tree_section(&mut self, delta: isize) {
        let vis = self.visible();
        let at = if self.on_add_root || self.on_below { None } else { vis.iter().position(|&g| g == self.selected) };
        let sd = self.forest.section_depth();
        let roots: Vec<usize> = (0..vis.len()).filter(|&i| self.forest.get(vis[i]).depth == sd).collect();
        let to = match (delta < 0, at) {
            (_, None) if delta > 0 => roots.first().copied(),
            (_, None) => None,
            (true, Some(at)) => roots.iter().rev().find(|&&i| i < at).copied(),
            (false, Some(at)) => roots.iter().find(|&&i| i > at).copied(),
        };
        if let Some(i) = to {
            self.select(vis[i]);
        }
    }

    fn tree_home(&mut self, last: bool) {
        let vis = self.visible();
        let pick = if last { vis.last() } else { vis.first() };
        if let Some(&g) = pick {
            self.select(g);
        }
    }

    pub fn toggle(&mut self, id: ClusterId) {
        if self.forest.get(id).children.is_empty() {
            return;
        }
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
    }

    /// Right (and Enter): open a collapsed cluster; on an open one (or a leaf),
    /// step INTO its content; on the header's `+`, name a new root.
    fn tree_right(&mut self) {
        if self.on_add_root {
            return self.open_new_cluster(None);
        }
        if self.on_below {
            return self.toggle_aggregate();
        }
        let id = self.selected;
        if !self.forest.get(id).children.is_empty() && self.collapsed.contains(&id) {
            self.collapsed.remove(&id);
        } else {
            self.focus = Focus::Content;
            self.reveal = true;
        }
    }

    /// Left: close an open cluster; on a closed one (or a leaf), go to its parent.
    /// At a closed root it stays: ← never leaves the sidebar's root.
    fn tree_left(&mut self) {
        if self.on_add_root || self.on_below {
            return;
        }
        let id = self.selected;
        if self.is_expanded(id) {
            self.collapsed.insert(id);
        } else if let Some(p) = self.forest.get(id).parent {
            self.select(p);
        }
    }

    /// `contents below` (^G, `n below`, the switch at the foot of the
    /// sidebar): every cluster's page takes in what the clusters below it
    /// hold, and the WHOLE CAULDRON appears as the root of every cluster —
    /// the soup's every question in one view (b964d571 msg 23) — and goes
    /// again when it is off. The selection and what is collapsed stay, by
    /// key.
    pub fn toggle_aggregate(&mut self) {
        self.aggregate = !self.aggregate;
        self.scroll = 0;
        self.item = 0;
        // The log stays open: ^G widens what it covers (anything opened on it closes).
        let log = self.trace.first().copied().filter(|t| t.0 == Item::Log);
        if let Err(e) = self.sync_whole() {
            self.error = Some(format!("the whole cauldron: {e:#}"));
        }
        self.reset_page();
        self.trace.extend(log);
    }

    /// [`App::toggle_aggregate`] to `on`.
    pub fn set_aggregate(&mut self, on: bool) {
        if self.aggregate != on {
            self.toggle_aggregate();
        }
    }

    /// Show the whole cauldron as the root exactly while `contents below`
    /// is on: the soup is read again only when the two disagree.
    pub(crate) fn sync_whole(&mut self) -> Result<()> {
        if self.forest.whole().is_some() == self.aggregate {
            return Ok(());
        }
        self.load_soup_keeping(true)
    }

    /// Close everything opened on the page: traces, repos, a field.
    fn reset_page(&mut self) {
        self.trace.clear();
        self.rnav.clear();
    }

    // ── the cluster page ──────────────────────────────────────────────────────

    /// The clusters the page covers: the focused one, plus its subtree when
    /// aggregating.
    pub fn scope(&self) -> Vec<ClusterId> {
        if self.aggregate {
            self.forest.subtree(self.selected)
        } else {
            vec![self.selected]
        }
    }

    /// Every selectable item on the current cluster tab, in the order the page
    /// draws them. The page is the one source: these are its items.
    pub fn items(&self) -> Vec<Item> {
        if self.ctab != CTab::Cauldron {
            return Vec::new();
        }
        crate::cauldron::pages::page(self, 100).ids
    }

    /// Whether page item `i` is the selected one.
    pub fn item_selected(&self, i: usize) -> bool {
        self.item == i
    }

    /// The item the trace stack's top page is about, if a trace is open.
    pub fn trace_top(&self) -> Option<Item> {
        self.trace.last().map(|t| t.0)
    }

    /// The ripples the Status tab lists, in state order: running, parked,
    /// pending — the cluster's own (with aggregate, its subtree's, in order).
    /// Done ripples are counted, not listed.
    pub fn listed_ripples(&self) -> Vec<crate::cauldron::model::Ripple> {
        let all = self.forest.ripples(self.selected, self.aggregate);
        let mut out = Vec::new();
        for st in [RippleState::Running, RippleState::Parked, RippleState::Pending] {
            out.extend(all.iter().filter(|r| r.state == st).copied());
        }
        out
    }

    /// Whether band `b` of the cluster page is open.
    pub fn band_open(&self, b: Band) -> bool {
        b.fixed() || b.open_by_default() != self.bands.contains(&b)
    }

    fn set_band(&mut self, b: Band, open: bool) {
        if b.fixed() {
            return;
        }
        if self.band_open(b) != open && !self.bands.remove(&b) {
            self.bands.insert(b);
        }
    }

    /// ⌥↑ / ⌥↓ on the page: the previous / next section head.
    fn item_section(&mut self, delta: isize) {
        let heads: Vec<usize> = crate::cauldron::pages::page(self, 100).heads;
        let to = if delta < 0 {
            heads.iter().rev().find(|&&i| i < self.item).copied()
        } else {
            heads.iter().find(|&&i| i > self.item).copied()
        };
        if let Some(i) = to {
            self.item = i;
            self.reveal = true;
        }
    }

    /// The active questions of the focused cluster (its own, or with
    /// aggregate on its subtree's): its questions, the machine nags on the
    /// repos it claims, and the questions linked here.
    pub fn active_questions(&self) -> usize {
        let scope = self.scope();
        self.forest.questions(self.selected, self.aggregate).len()
            + scope.iter().map(|&g| crate::cauldron::pages::focus_nags(self, g).len()).sum::<usize>()
            + if self.aggregate { 0 } else { self.forest.linked_questions(self.selected).len() }
    }

    /// The active human actions of the focused cluster: open, or drafted.
    pub fn active_actions(&self) -> usize {
        self.scope().iter().map(|&g| self.forest.get(g).open_actions()).sum()
    }

    /// A cluster tab's title. Questions and Actions each carry their own
    /// count (8e4fca10 msg 385, flattening Human): its active questions, its
    /// active human actions. No other tab carries a number.
    pub fn ktab_title(&self, t: KTab) -> String {
        match t {
            KTab::Questions => format!("{} {}", t.label(), self.active_questions()),
            KTab::Actions => format!("{} {}", t.label(), self.active_actions()),
            t => t.label().to_string(),
        }
    }

    /// The repos on the focused cluster's Artifacts tab.
    pub fn cluster_repos(&self) -> Vec<ClusterRepo> {
        self.repos.cluster_repos(&self.scope())
    }

    /// The regions on the focused cluster's Nodes tab: its own claims, or with
    /// aggregate on, every claim in its subtree, each once, with its origins.
    pub fn cluster_regions(&self) -> Vec<ClusterRegion> {
        self.corpus.cluster_regions(&self.scope())
    }

    /// The Nodes tab's list under the search query: each listed region the
    /// query reaches, with the member nodes it matched (none when the query is
    /// empty).
    pub fn region_hits(&self) -> Vec<(RegionId, Vec<NodeId>)> {
        let hits = self.corpus.search(&self.search);
        self.cluster_regions()
            .into_iter()
            .filter_map(|g| hits.iter().find(|h| h.region == g.region).map(|h| (h.region, h.nodes.clone())))
            .collect()
    }

    /// Put the selection on `it` in the page as it now is (after something
    /// opened or closed above it).
    fn select_item(&mut self, it: Item) {
        if let Some(i) = self.items().iter().position(|x| *x == it) {
            self.item = i;
        }
        self.reveal = true;
    }

    /// Push a trace page (or an in-place expansion on one) for `it`.
    fn push_trace(&mut self, it: Item) {
        self.trace.push((it, self.item, self.scroll));
        self.item = 0;
        self.scroll = 0;
        if !is_trace_root(it) {
            // An expansion: the page is the one below; stay on the row.
            self.select_item(it);
        }
        self.reveal = true;
    }

    /// The selected item's next item, if it is inside the selected one's
    /// band or box (→ on an open band, or a box with parts, steps in).
    fn step_in(&mut self) -> bool {
        let n = self.items().len();
        if self.item + 1 < n {
            self.item += 1;
            self.reveal = true;
            return true;
        }
        false
    }

    /// → on the page of a cluster tab other than Nodes.
    fn open_on_page(&mut self, item: Item) -> bool {
        if !self.trace.is_empty() {
            // A trace page: a node row opens ITS trace page; a question, a
            // region, a soup node or a blueprint entry opens in place.
            if self.trace_top() == Some(item) {
                return false;
            }
            return match item {
                Item::Task { .. } | Item::Milestone { .. } | Item::Objective { .. } | Item::Directive { .. } | Item::Ha { .. } => {
                    self.push_trace(item);
                    true
                }
                // A run on the log opens its run page; ← there comes back
                // to the log, on the row he left.
                Item::Run(ix) => match self.rstore.run_at(ix).map(|r| r.id.clone()) {
                    Some(id) => {
                        self.open_bus(crate::cauldron::bus::BusPage::Run(id));
                        true
                    }
                    None => false,
                },
                Item::Question { .. } | Item::Region(_) | Item::Bp { .. } | Item::Node(_) | Item::Sent(_) => {
                    if self.trace.iter().any(|t| t.0 == item) {
                        return false;
                    }
                    // One expansion at a time: replace a sibling's.
                    if self.trace_top().is_some_and(|t| !is_trace_root(t)) {
                        self.trace.pop();
                    }
                    self.push_trace(item);
                    true
                }
                _ => false,
            };
        }
        match (self.ktab, item) {
            (_, Item::Footer(Footer::Below)) => {
                self.toggle_aggregate();
                true
            }
            (_, Item::Footer(Footer::Log)) => {
                self.push_trace(Item::Log);
                true
            }
            // A band opens; on an open one, → steps to its first row.
            (_, Item::Band(b)) => {
                if self.band_open(b) {
                    self.step_in()
                } else {
                    self.set_band(b, true);
                    self.select_item(item);
                    true
                }
            }
            (_, Item::Directive { .. } | Item::Task { .. } | Item::Milestone { .. } | Item::Objective { .. }) => {
                self.push_trace(item);
                true
            }
            (KTab::Status, Item::Ripple { cluster, serial }) => {
                self.push_trace(Item::Task { cluster, serial });
                true
            }
            // A background run opens its execution in the bus modal.
            (KTab::Status, Item::Background { cluster, ix }) => match self.background_run(cluster, ix) {
                Some(run) => {
                    self.open_bus(crate::cauldron::bus::BusPage::Run(run));
                    true
                }
                _ => false,
            },
            // A human action's box steps into its first part; a part does
            // what it is (a field takes keys, `done` marks it done, a copy
            // copies, an open opens).
            (_, Item::Ha { .. }) => {
                let next = self.items().get(self.item + 1).copied();
                matches!(next, Some(Item::HaPart { .. })) && self.step_in()
            }
            // A cluster made here is renamed, moved or deleted from the foot
            // of its Details.
            (_, Item::Manage(k)) => {
                let key = self.forest.get(self.selected).key.clone();
                match k {
                    // An interrupted seeding's two acts (IS-32).
                    3 => self.reseed_cluster(&key),
                    4 => self.clear_seed(&key),
                    _ => self.start_act(match k {
                        0 => crate::cauldron::manage::Act::RenameCluster(key),
                        1 => crate::cauldron::manage::Act::MoveCluster(key),
                        _ => crate::cauldron::manage::Act::DeleteCluster(key),
                    }),
                }
                true
            }
            (_, Item::HaPart { cluster, serial, part }) => {
                self.ha_act(cluster, serial, part);
                true
            }
            // An inserted artifact opens in a modal (or, not text, in `open`).
            (_, Item::QArt { cluster, serial, ix }) => {
                let Some(a) = self.forest.get(cluster).question(serial).and_then(|q| q.artifacts.get(ix)).cloned() else { return false };
                self.open_artifact(&a);
                true
            }
            // A question's box steps to its first option.
            (_, Item::Question { .. }) => {
                let next = self.items().get(self.item + 1).copied();
                matches!(next, Some(Item::Option { .. })) && self.step_in()
            }
            _ => false,
        }
    }

    /// ← on such a page: close a trace level; close an open band; climb from
    /// inside a band or a box to its header. False at the top.
    fn close_on_page(&mut self) -> bool {
        if let Some((_, item, scroll)) = self.trace.pop() {
            self.item = item;
            self.scroll = scroll;
            self.reveal = true;
            return true;
        }
        let page = crate::cauldron::pages::page(self, 100);
        let Some(&it) = page.ids.get(self.item) else { return false };
        if let Item::Band(b) = it {
            // An always-expanded band does not close: ← goes up, as from a row.
            if !b.fixed() && self.band_open(b) {
                self.set_band(b, false);
                self.select_item(it);
                return true;
            }
        }
        match page.parents.get(self.item).copied().flatten() {
            Some(up) => {
                self.select_item(up);
                true
            }
            None => false,
        }
    }

    /// → on the Artifacts tab (design 0146; rule 4): a closed band opens in
    /// place, and on an open one → steps to its first row; a worktree or an
    /// artifact opens its detail rows in place; a directory lists itself;
    /// `n more ▸` shows the rest. Under BLUEPRINT a collapsed module expands
    /// in place, and an expanded module or any other entry opens its page.
    fn open_repos(&mut self, item: Item) -> bool {
        if let Some((RNav::Entry(r, n), ..)) = self.rnav.last() {
            if item == (Item::Bp { repo: *r, node: *n }) {
                return false;
            }
        }
        match item {
            Item::Repo(_) | Item::Art(_) | Item::Region(_) | Item::Node(_) => {
                let Some(row) = self.art_row(item) else { return false };
                if !row.opens {
                    return false;
                }
                if row.key.starts_with("more:") {
                    self.art_toggled.insert(row.key);
                    self.select_item(item);
                    return true;
                }
                let page = crate::cauldron::pages::page(self, 100);
                let i = page.ids.iter().position(|x| *x == item);
                let has_child = i.is_some_and(|i| page.parents.get(i + 1).copied().flatten() == Some(item));
                let leaf = row.key.starts_with("wt:") || row.key.starts_with("art:") || row.key.starts_with("node:");
                if has_child && !leaf {
                    return self.step_in();
                }
                if leaf && self.art_is_open(&row.key) {
                    return false;
                }
                if !self.art_toggled.remove(&row.key) {
                    self.art_toggled.insert(row.key);
                }
                self.select_item(item);
                true
            }
            Item::Bp { repo, node } => {
                let module = self.repos.repos[repo].nodes[node].kind == "module";
                if self.rnav.is_empty() && module && !self.rexpanded.contains(&(repo, node)) {
                    self.rexpanded.insert((repo, node));
                    self.select_item(item);
                    return true;
                }
                self.rnav.push((RNav::Entry(repo, node), self.item, self.scroll));
                self.item = 0;
                self.scroll = 0;
                self.reveal = true;
                true
            }
            _ => false,
        }
    }

    /// Whether the Artifacts row `key` is drawn open now.
    fn art_is_open(&self, key: &str) -> bool {
        let page = crate::cauldron::pages::page(self, 100);
        page.arts.iter().find(|r| r.key == key).is_some_and(|r| r.open)
    }

    /// ← on the Artifacts tab: an entry's page goes back; an expanded module
    /// collapses; an open band or leaf closes; a row climbs to its band.
    fn close_repos(&mut self) -> bool {
        if let Some((_, item, scroll)) = self.rnav.pop() {
            self.item = item;
            self.scroll = scroll;
            self.reveal = true;
            return true;
        }
        let page = crate::cauldron::pages::page(self, 100);
        let Some(&item) = page.ids.get(self.item) else { return false };
        if let Item::Bp { repo, node } = item {
            if self.rexpanded.remove(&(repo, node)) {
                self.select_item(item);
                return true;
            }
        }
        if let Some(row) = page.arts.iter().find(|r| r.item == item) {
            if row.opens && !row.key.starts_with("more:") && self.art_is_open(&row.key) {
                if !self.art_toggled.remove(&row.key) {
                    self.art_toggled.insert(row.key.clone());
                }
                self.select_item(item);
                return true;
            }
        }
        match page.parents.get(self.item).copied().flatten() {
            Some(parent) => {
                self.select_item(parent);
                true
            }
            None => false,
        }
    }

    /// `c` and `o` on the Artifacts tab: copy the selected repo's path, or
    /// `open` it (msg 165: "if I click on the path, I want it to
    /// automatically copy it to my clipboard ... that should literally just
    /// call the open bash command").
    pub fn artifact_path(&self) -> Option<String> {
        match self.items().get(self.item)? {
            Item::Repo(r) => Some(self.repos.repos[*r].path.clone()),
            Item::Bp { repo, .. } => Some(self.repos.repos[*repo].path.clone()),
            it @ Item::Art(_) => self.art_row(*it).and_then(|r| r.path),
            Item::QArt { cluster, serial, ix } => {
                let a = self.forest.get(*cluster).question(*serial)?.artifacts.get(*ix)?.clone();
                Some(crate::cauldron::pages::q_artifact(self, &a).1)
            }
            _ => None,
        }
    }

    /// What Artifacts row `it` is, as the page drew it.
    pub fn art_row(&self, it: Item) -> Option<crate::cauldron::artpage::ArtRow> {
        crate::cauldron::pages::page(self, 100).arts.into_iter().find(|r| r.item == it)
    }

    /// A click on a path's copy or open target selects its row too.
    fn select_art_path(&mut self, path: &str) {
        let page = crate::cauldron::pages::page(self, 100);
        if let Some(r) = page.arts.iter().find(|r| r.path.as_deref() == Some(path)) {
            if let Some(i) = page.ids.iter().position(|x| *x == r.item) {
                self.item = i;
                self.focus = Focus::Content;
            }
        }
    }

    pub fn copy_path(&mut self, path: String) {
        self.notice = Some(format!("copying {}", path.replace(&std::env::var("HOME").unwrap_or_default(), "~")));
        self.flash = Some(path.clone());
        self.outbox.push(Outbox::Copy(path));
    }

    /// Open an inserted artifact: a text file in the artifact modal, anything
    /// else (or a file not there) handed to `open`.
    pub fn open_artifact(&mut self, a: &str) {
        let (name, path, ..) = crate::cauldron::pages::q_artifact(self, a);
        let text_ext = ["md", "txt", "yaml", "yml", "json", "toml", "rs", "log", "csv", "sh", "py"];
        let is_text = std::path::Path::new(&path).extension().and_then(|e| e.to_str()).is_some_and(|e| text_ext.contains(&e.to_ascii_lowercase().as_str()));
        match std::fs::read_to_string(&path).ok().filter(|_| is_text) {
            Some(text) => {
                let lines = text.lines().take(5000).map(str::to_string).collect();
                self.modal = Some(Modal::Artifact { title: name, path, lines, scroll: 0 });
            }
            None => self.open_path(path),
        }
    }

    pub fn open_path(&mut self, path: String) {
        // A path that does not exist is `open`'s to say; nothing here checks.
        self.notice = Some(format!("open {}", path.replace(&std::env::var("HOME").unwrap_or_default(), "~")));
        self.outbox.push(Outbox::Open(path));
    }

    /// A human action's link as it opens here: `{soup}` is this cauldron's
    /// soup folder, wherever its root is (0217 B11: a seed's link named his
    /// real ~/.cauldron whatever `CAULDRON_HOME` said).
    pub fn ha_url(&self, url: &str) -> String {
        match store::soup_dir(&self.env) {
            Ok(d) if url.contains("{soup}") => url.replace("{soup}", &d.to_string_lossy()),
            _ => url.to_string(),
        }
    }

    /// A human action and its ticket, read in the human-action format.
    pub fn ticket(&self, cluster: ClusterId, serial: u32) -> Option<(&crate::cauldron::model::HumanAction, crate::cauldron::tickets::Ticket)> {
        let h = self.forest.get(cluster).human_action(serial)?;
        Some((h, crate::cauldron::tickets::parse(&h.body)))
    }

    /// Whether step `step` of a human action is done: marked done here, or
    /// (a value step) every field submitted here, or the ticket resolved.
    pub fn step_done(&self, cluster: ClusterId, serial: u32, t: &crate::cauldron::tickets::Ticket, step: usize) -> bool {
        let key = &self.forest.get(cluster).key;
        let Some(st) = t.steps.get(step) else { return false };
        let resolved = self.forest.get(cluster).human_action(serial).is_some_and(|h| h.status == crate::cauldron::model::HaStatus::Resolved);
        resolved || self.tickets.is_done(key, serial, &st.slug)
    }

    /// Enter (or →) on part `part` of a human action's steps.
    fn ha_act(&mut self, cluster: ClusterId, serial: u32, part: usize) {
        use crate::cauldron::model::HaStatus;
        use crate::cauldron::tickets::Part;
        let Some((h, t)) = self.ticket(cluster, serial) else { return };
        let status = h.status;
        let Some(p) = t.parts().into_iter().nth(part) else { return };
        let key = self.forest.get(cluster).key.clone();
        // A draft is not ready for him: nothing on it acts until it is open
        // (the format's rule); a retired one is withdrawn, a resolved one done.
        let live = status == HaStatus::Open;
        match p {
            Part::Copy { text, label, .. } => {
                self.notice = Some(format!("copying {label}"));
                self.outbox.push(Outbox::Copy(text));
            }
            Part::Open { url, .. } => self.open_path(self.ha_url(&url)),
            _ if !live => {
                self.notice = Some(match status {
                    HaStatus::Draft => "a draft: it is waiting on a value before you can act on it".into(),
                    HaStatus::Retired => "retired: withdrawn, nothing to do".into(),
                    _ => "done".into(),
                });
            }
            // 0217 B2: a value is never taken here. This face has no
            // destination for one (the human-action contract writes a
            // submitted value to its PROJECT's registered write target, and
            // a cluster's ticket names none), so a box that took it would
            // only pretend to: it said `✓ submitted` and dropped the key.
            Part::Field { field, .. } => {
                self.notice = Some(format!("{}: not taken here — this face has nowhere to store a value; give it in the human-actions app, then mark the step done", field.label));
            }
            Part::Done { step } => {
                let st = &t.steps[step];
                let on = !self.tickets.is_done(&key, serial, &st.slug);
                match self.tickets.set_done(&key, serial, &st.slug, on) {
                    Ok(()) => self.notice = Some(format!("{} {}", if st.title.is_empty() { "the step" } else { &st.title }, if on { "done" } else { "not done" })),
                    Err(e) => self.error = Some(format!("the step NOT marked: {e:#}")),
                }
            }
        }
    }

    /// A click on the selected page item, when it is a BAND: it opens if it
    /// is shut and shuts if it is open, where it is (msg 235). False when it
    /// is not a band.
    fn toggle_item_band(&mut self) -> bool {
        let Some(&item) = self.items().get(self.item) else { return false };
        match item {
            Item::Band(b) => {
                let open = self.band_open(b);
                self.set_band(b, !open);
                self.select_item(item);
                true
            }
            Item::Repo(_) | Item::Art(_) | Item::Region(_) | Item::Node(_) if self.ktab == KTab::Nodes && self.trace.is_empty() => {
                let Some(row) = self.art_row(item) else { return false };
                if !row.opens || row.key.starts_with("more:") {
                    return false;
                }
                if !self.art_toggled.remove(&row.key) {
                    self.art_toggled.insert(row.key);
                }
                self.select_item(item);
                true
            }
            Item::Bp { repo, node } if self.ktab == KTab::Nodes && self.rnav.is_empty() && self.repos.repos[repo].nodes[node].kind == "module" => {
                if !self.rexpanded.remove(&(repo, node)) {
                    self.rexpanded.insert((repo, node));
                }
                self.select_item(item);
                true
            }
            _ => false,
        }
    }

    /// A click on the selected Status or Config row, when it is a BAND: it
    /// opens or shuts (msg 235). False when it is not a band.
    fn toggle_row_band(&mut self) -> bool {
        match self.ctab {
            CTab::Status => {
                let v = crate::cauldron::status::view(self, 120);
                let Some(it) = v.items.get(self.rows[0]) else { return false };
                if it.act != crate::cauldron::status::SAct::Band {
                    return false;
                }
                if !self.stnav.remove(&it.key) {
                    self.stnav.insert(it.key.clone());
                }
                true
            }
            CTab::Config => {
                let Some(band) = self.selectable_flat().into_iter().nth(self.settings_sel()).and_then(|f| f.entry.band()) else { return false };
                let open = self.snav.is_open(&band);
                self.snav.set_open(band, !open);
                true
            }
            CTab::Cauldron | CTab::Agents => false,
        }
    }

    /// The label of sub-tab `i` of the strip on the cauldron tab shown.
    fn strip_label(&self, i: usize) -> Option<String> {
        match self.ctab {
            CTab::Status => crate::cauldron::status::StatusTab::ALL.get(i).map(|t| t.label().to_string()),
            CTab::Config => crate::cauldron::menu::CfgTab::ALL.get(i).map(|t| self.cfg_label(*t)),
            CTab::Cauldron | CTab::Agents => None,
        }
    }

    /// Open the event bus's modal on `page` (msg 235).
    pub fn open_bus(&mut self, page: crate::cauldron::bus::BusPage) {
        self.modal = Some(Modal::Bus { stack: vec![(page, 0, 0)] });
    }

    /// Open the selected item one level. False when there is nothing to open.
    pub fn open_selected(&mut self) -> bool {
        let Some(&item) = self.items().get(self.item) else { return false };
        // `n below` opens the aggregate view on every tab, so it is routed
        // before Nodes hands → to their own navigation.
        if self.ctab == CTab::Cauldron && item == Item::Footer(Footer::Below) {
            self.toggle_aggregate();
            return true;
        }
        if self.ctab == CTab::Cauldron && self.ktab == KTab::Nodes && self.trace.is_empty() {
            return self.open_repos(item);
        }
        self.ctab == CTab::Cauldron && self.open_on_page(item)
    }

    /// Close one level. False when the list is already at the top.
    pub fn close_level(&mut self) -> bool {
        if self.ctab != CTab::Cauldron {
            return false;
        }
        if self.ktab == KTab::Nodes && self.trace.is_empty() {
            return self.close_repos();
        }
        self.close_on_page()
    }

    /// Open a region (and optionally one of its nodes, its body shown) by
    /// id in the Nodes tab's REGIONS band, as → would, and select it. A node
    /// given alone opens in the first of its regions this cluster lists.
    pub fn open_ids(&mut self, region: Option<&str>, node: Option<&str>) -> Result<()> {
        self.set_ctab(CTab::Cauldron);
        self.set_ktab(KTab::Nodes);
        self.set_search("");
        let listed: Vec<RegionId> = self.cluster_regions().into_iter().map(|g| g.region).collect();
        let n = match node {
            Some(id) => Some(self.corpus.find_node(id).with_context(|| format!("no node `{id}`"))?),
            None => None,
        };
        let r = match region {
            Some(id) => Some(self.corpus.find_region(id).with_context(|| format!("no region `{id}`"))?),
            None => n.and_then(|n| {
                let below: Vec<RegionId> = self.corpus.cluster_regions(&self.forest.subtree(self.selected)).into_iter().map(|g| g.region).collect();
                let ins = &self.corpus.nodes[n].in_region;
                ins.iter().copied().find(|r| listed.contains(r)).or_else(|| ins.iter().copied().find(|r| below.contains(r)))
            }),
        };
        let Some(r) = r else { anyhow::bail!("no region of `{}` is on {}'s Nodes", node.unwrap_or("?"), self.forest.get(self.selected).name) };
        if !listed.contains(&r) {
            // Claimed below it: the page covers what is below (as ^G).
            if !self.aggregate && self.corpus.cluster_regions(&self.forest.subtree(self.selected)).iter().any(|g| g.region == r) {
                self.toggle_aggregate();
            } else if !self.aggregate || !self.cluster_regions().iter().any(|g| g.region == r) {
                anyhow::bail!("{} is not on {}'s Nodes", self.corpus.regions[r].id, self.forest.get(self.selected).name);
            }
        }
        self.art_toggled.remove("regions");
        self.art_toggled.insert(format!("region:{}", self.corpus.regions[r].id));
        let mut at = Item::Region(r);
        if let Some(n) = n {
            self.art_toggled.insert(format!("node:{}", self.corpus.nodes[n].id));
            at = Item::Node(n);
        }
        self.focus = Focus::Content;
        self.select_item(at);
        Ok(())
    }

    /// Replace the search query; the list goes back to its top.
    pub fn set_search(&mut self, q: &str) {
        self.search = q.to_string();
        self.item = 0;
        self.scroll = 0;
    }

    /// ^F: the focused cluster's Nodes tab, with the search field taking keys.
    pub fn start_search(&mut self) {
        self.set_ctab(CTab::Cauldron);
        self.set_ktab(KTab::Nodes);
        self.focus = Focus::Content;
        self.searching = true;
    }

    /// The search field. Like every field: Enter or Esc leaves it and what is
    /// typed is KEPT (^U clears it); ↑ ↓ leave it and move.
    fn search_key(&mut self, key: KeyEvent, ctrl: bool) {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => self.searching = false,
            KeyCode::Char('f') if ctrl => self.searching = false,
            KeyCode::Char('u') if ctrl => self.set_search(""),
            KeyCode::Backspace => {
                let mut q = self.search.clone();
                q.pop();
                self.set_search(&q);
            }
            KeyCode::Char(c) if !ctrl => {
                let q = format!("{}{c}", self.search);
                self.set_search(&q);
            }
            KeyCode::Up | KeyCode::Down => {
                self.searching = false;
                self.key(key);
            }
            _ => {}
        }
    }

    /// The tag `i` quotes for an item. A settings row, a footer, a band and
    /// a background run that took in no message have none.
    pub fn tag_for(&self, item: Item) -> Option<Tag> {
        let key = |g: ClusterId| self.forest.get(g).key.clone();
        Some(match item {
            Item::Description(g) => Tag::thing("cluster", &key(g)),
            Item::LastDump(g) => {
                let k = key(g);
                let last = self.sends.sends.iter().rev().find(|s| s.targets.contains(&k))?;
                Tag::thing("dump", &last.seq.to_string())
            }
            // A soup item, bare like a question (task 0209 D16).
            Item::Directive { cluster, ix } => Tag::item(&self.forest.get(cluster).directives[ix].id),
            Item::Constraint { cluster, ix } => Tag::item(&self.forest.get(cluster).constraints[ix].id),
            Item::Question { serial, .. } | Item::Closed { serial, .. } => Tag::item(format!("Q-{serial}")),
            Item::QArt { cluster, serial, ix } => {
                let a = self.forest.get(cluster).question(serial)?.artifacts.get(ix)?.clone();
                match crate::cauldron::pages::q_artifact(self, &a) {
                    (_, _, _, Some(id)) => Tag::thing("node", &id),
                    (_, path, ..) => Tag::thing("path", &crate::cauldron::artpage::tilde(&path)),
                }
            }
            Item::Option { serial, opt, .. } => {
                Tag::item(format!("Q-{serial}.opt{opt}"))
            }
            Item::Objective { serial, .. } => Tag::item(format!("OBJ-{serial}")),
            Item::Milestone { serial, .. } => Tag::item(format!("M-{serial}")),
            Item::Task { serial, .. } => Tag::item(format!("T-{serial}")),
            Item::Ha { serial, .. } => Tag::item(format!("HA-{serial}")),
            // A part of a step quotes its step.
            Item::HaPart { cluster, serial, part } => {
                let (_, t) = self.ticket(cluster, serial)?;
                let step = match t.parts().into_iter().nth(part)? {
                    crate::cauldron::tickets::Part::Field { step, .. }
                    | crate::cauldron::tickets::Part::Done { step }
                    | crate::cauldron::tickets::Part::Copy { step, .. }
                    | crate::cauldron::tickets::Part::Open { step, .. } => step,
                };
                Tag::item(format!("HA-{serial}.step{}", step + 1))
            }
            Item::Region(r) => Tag::thing("region", &self.corpus.regions[r].id),
            Item::Node(n) => Tag::thing("node", &self.corpus.nodes[n].id),
            Item::Repo(r) => Tag::thing("repo", &self.repos.repos[r].name),
            Item::Art(_) => return self.art_row(item).and_then(|r| r.tag),
            Item::Bp { repo, node } => Tag::thing("node", &self.repos.repos[repo].nodes[node].id),
            // A ripple is its task's run: it is tagged as the task.
            Item::Ripple { serial, .. } => Tag::item(format!("T-{serial}")),
            Item::Background { cluster, ix } => Tag::thing("msg", self.forest.get(cluster).background[ix].cite.as_deref()?),
            Item::Message(i) => Tag::thing("msg", &self.messages[i].0),
            Item::Sent(seq) => Tag::thing("dump", &seq.to_string()),
            Item::Run(ix) => Tag::thing("run", &self.rstore.run_at(ix)?.id),
            Item::Footer(_) | Item::Log | Item::Band(_) => return None,
            Item::Manage(_) => return None,
        })
    }

    /// The clusters a tag lands on when sent: a soup item the cluster that
    /// holds it (and every cluster it is linked to); a cluster tag its
    /// cluster; a region tag every cluster that claims it; a soup node every
    /// cluster that claims a region it attests (a directive or a constraint:
    /// the cluster that holds it); a repo, or an entry of its blueprint,
    /// every cluster that claims the repo. A message lands on no cluster.
    pub fn clusters_of(&self, tag: &Tag) -> Vec<String> {
        let keys = |gs: &[ClusterId]| gs.iter().map(|&g| self.forest.get(g).key.clone()).collect();
        match tag {
            // A directive or a constraint lands where its node tag does.
            Tag::Item { item } if item.starts_with("DIR-") || item.starts_with("CON-") => self.clusters_of(&Tag::thing("node", item)),
            Tag::Item { item } => {
                let base = item.split('.').next().unwrap_or(item);
                let Some(l) = crate::cauldron::model::Local::parse(base) else { return Vec::new() };
                let Some(o) = self.forest.owner(l) else { return Vec::new() };
                let mut gs = vec![o];
                gs.extend((0..self.forest.clusters.len()).filter(|&g| g != o && self.forest.get(g).links.iter().any(|x| x == base)));
                keys(&gs)
            }
            // The whole cauldron is the soup: a dump tagged with it lands on
            // no one cluster, as an untagged one does (b964d571 msg 23).
            Tag::Thing { kind, name } if kind == "cluster" => {
                self.forest.find(name).filter(|&g| !self.forest.get(g).whole).map_or_else(Vec::new, |g| keys(&[g]))
            }
            Tag::Thing { kind, name } if kind == "repo" => {
                self.repos.find_repo(name).map_or_else(Vec::new, |r| keys(&self.repos.claimants[r]))
            }
            Tag::Thing { kind, name } if kind == "region" => {
                self.corpus.find_region(name).map_or_else(Vec::new, |r| keys(&self.corpus.claimants[r]))
            }
            Tag::Thing { kind, name } if kind == "node" => {
                if let Some((g, _)) = self.forest.directive(name) {
                    return keys(&[g]);
                }
                if let Some(g) = self.forest.clusters.iter().position(|x| x.constraints.iter().any(|c| c.id == *name)) {
                    return keys(&[g]);
                }
                if let Some((r, _)) = self.repos.locate(name) {
                    return keys(&self.repos.claimants[r]);
                }
                let Some(n) = self.corpus.find_node(name) else { return Vec::new() };
                let mut gs: Vec<ClusterId> = Vec::new();
                for &r in &self.corpus.nodes[n].in_region {
                    for &g in &self.corpus.claimants[r] {
                        if !gs.contains(&g) {
                            gs.push(g);
                        }
                    }
                }
                gs.sort_unstable();
                keys(&gs)
            }
            // A run lands on the cluster it ran on; an event on the cluster
            // it happened on; a dump where that dump landed.
            Tag::Thing { kind, name } if kind == "run" => {
                let id = name.split('/').next().unwrap_or(name);
                self.rstore.run(id).and_then(|r| self.forest.find(&r.cluster)).map_or_else(Vec::new, |g| keys(&[g]))
            }
            Tag::Thing { kind, name } if kind == "event" => {
                self.rstore.events().into_iter().find(|e| e.id == *name).and_then(|e| self.forest.find(&e.cluster)).map_or_else(Vec::new, |g| keys(&[g]))
            }
            Tag::Thing { kind, name } if kind == "send" || kind == "dump" => {
                let seq = name.split('.').next().unwrap_or(name);
                self.sends.sends.iter().find(|s| s.seq.to_string() == seq).map_or_else(Vec::new, |s| s.targets.clone())
            }
            // A chip is the bucket's own: it points nowhere else.
            Tag::Thing { .. } | Tag::Chip { .. } => Vec::new(),
        }
    }

    /// Move the page's selection. False when it could not move (it was at
    /// that edge, or the page lists nothing).
    fn item_move(&mut self, delta: isize) -> bool {
        let n = self.items().len();
        if n == 0 {
            return false;
        }
        let to = (self.item as isize + delta).clamp(0, n as isize - 1) as usize;
        let moved = to != self.item;
        self.item = to;
        self.reveal = true;
        moved
    }

    pub fn set_ktab(&mut self, t: KTab) {
        if t != self.ktab {
            self.ktab = t;
            self.item = 0;
            self.scroll = 0;
            self.reset_page();
        }
        if t != KTab::Nodes {
            self.searching = false;
        }
    }

    fn step_ktab(&mut self, delta: isize) {
        let i = KTab::ALL.iter().position(|t| *t == self.ktab).unwrap_or(0) as isize;
        let n = KTab::ALL.len() as isize;
        self.set_ktab(KTab::ALL[((i + delta) % n + n) as usize % n as usize]);
    }

    /// Whether the TUI band's `preserve <tab> expansion` is on.
    pub fn preserve(&self, tab: &str) -> bool {
        self.sget(&Bound::new(crate::cauldron::menu::preserve_key(tab), settings::Ctx::default())).val.as_bool()
    }

    pub fn set_ctab(&mut self, t: CTab) {
        if t != self.ctab {
            self.searching = false;
            self.field = None;
            // What he leaves resets, unless he keeps it (msg 165: "default
            // off: leaving and returning resets the expansion"). The sub-tab
            // he was on is not expansion: it stays.
            match self.ctab {
                CTab::Config if !self.preserve("config") => {
                    self.snav = Nav::cauldron();
                    self.rows[crate::cauldron::menu::SETTINGS_ROW] = 0;
                }
                CTab::Status if !self.preserve("status") => {
                    self.stnav.clear();
                    self.bus_pages.clear();
                    self.rows[0] = 0;
                }
                _ => {}
            }
        }
        self.ctab = t;
        // The bucket is only on Surface: off it, the rows hold the selection.
        if self.focus != Focus::Input || t != CTab::Cauldron {
            self.focus = if t == CTab::Cauldron { Focus::Tree } else { Focus::Content };
        }
    }

    fn step_ctab(&mut self, delta: isize) {
        let n = CTab::ALL.len() as isize;
        let i = self.ctab.index() as isize;
        self.set_ctab(CTab::ALL[((i + delta) % n + n) as usize % n as usize]);
    }

    fn tab(&mut self, k: TabKey) {
        // A value being typed is kept, as leaving any field keeps it.
        self.commit_field(true);
        match k {
            TabKey::Cauldron(i) => {
                if let Some(&t) = CTab::ALL.get(i) {
                    self.set_ctab(t);
                }
            }
            TabKey::CauldronStep(d) => self.step_ctab(d),
            // A cluster tab is on Surface: go there first.
            TabKey::Cluster(i) => {
                if let Some(&t) = KTab::ALL.get(i) {
                    self.set_ctab(CTab::Cauldron);
                    self.set_ktab(t);
                }
            }
            TabKey::ClusterStep(d) => {
                self.set_ctab(CTab::Cauldron);
                self.step_ktab(d);
            }
        }
    }

    // ── Status and Config ──────────────────────────────────────────────────

    fn dash_index(&self) -> Option<usize> {
        match self.ctab {
            CTab::Status => Some(0),
            CTab::Config => Some(crate::cauldron::menu::SETTINGS_ROW),
            CTab::Cauldron | CTab::Agents => None,
        }
    }

    /// The selectable rows of the tab on screen, in display order.
    pub fn dash_rows(&self) -> Vec<Row> {
        match self.ctab {
            CTab::Cauldron | CTab::Agents => Vec::new(),
            CTab::Status => (0..crate::cauldron::status::view(self, 120).items.len()).map(Row::Status).collect(),
            CTab::Config => self.settings_rows(),
        }
    }

    /// The selected row on the tab on screen.
    pub fn row(&self) -> usize {
        self.dash_index().map_or(0, |i| self.rows[i])
    }

    /// Move the tab's selection. False at the edge.
    fn row_move(&mut self, delta: isize) -> bool {
        let n = self.dash_rows().len();
        let (Some(i), true) = (self.dash_index(), n > 0) else { return false };
        let to = (self.rows[i] as isize + delta).clamp(0, n as isize - 1) as usize;
        let moved = to != self.rows[i];
        self.rows[i] = to;
        moved
    }

    /// ⌥↑ / ⌥↓ on a tab: its section heads.
    fn row_section(&mut self, delta: isize) {
        let heads = match self.ctab {
            CTab::Config => self.settings_heads(),
            CTab::Status => crate::cauldron::status::view(self, 120).heads,
            CTab::Cauldron | CTab::Agents => return,
        };
        let Some(d) = self.dash_index() else { return };
        let at = self.rows[d];
        let to = if delta < 0 { heads.iter().rev().find(|&&i| i < at) } else { heads.iter().find(|&&i| i > at) };
        if let Some(&i) = to {
            self.rows[d] = i;
        }
    }

    /// → / Enter on the Status tab: a band opens (on an open one, → steps to
    /// its first row); a row does what it says.
    /// Whether the selected Status row acts at one click: `show more`, a
    /// `config ▸` jump, or a row that opens a page — a bus event, trigger,
    /// ripple or execution, a task's run (0211 R6).
    pub fn status_is_button(&self) -> bool {
        use crate::cauldron::status::SAct;
        let v = crate::cauldron::status::view(self, 120);
        v.items.get(self.rows[0]).is_some_and(|it| matches!(it.act, SAct::More(_) | SAct::Config { .. } | SAct::Endpoint { .. } | SAct::Bus(_) | SAct::Task { .. } | SAct::Service { .. } | SAct::Exec { .. }))
    }

    pub fn status_open(&mut self) -> bool {
        use crate::cauldron::status::SAct;
        let v = crate::cauldron::status::view(self, 120);
        let Some(it) = v.items.get(self.rows[0]).cloned() else { return false };
        match it.act {
            SAct::Strip => {
                let i = crate::cauldron::status::StatusTab::ALL.iter().position(|t| *t == self.sttab).unwrap_or(0);
                match crate::cauldron::status::StatusTab::ALL.get(i + 1) {
                    Some(&t) => self.set_sttab(t),
                    None if v.items.len() > 1 => self.rows[0] = 1,
                    None => return false,
                }
            }
            SAct::Band => {
                if crate::cauldron::status::is_open(self, &it.key) {
                    if self.rows[0] + 1 < v.items.len() {
                        self.rows[0] += 1;
                    }
                } else if !self.stnav.remove(&it.key) {
                    self.stnav.insert(it.key);
                }
            }
            SAct::Copy(path) => self.copy_path(path),
            SAct::NoLog(name) => self.notice = Some(format!("no log is recorded for {name}")),
            SAct::Config { pool, account } => self.open_account_settings(&pool, &account),
            SAct::Endpoint { here, id } => self.open_endpoint_settings(here, &id),
            // A task's run opens on its cluster's Status tab, on its row.
            SAct::Task { cluster, serial } => {
                self.set_ctab(CTab::Cauldron);
                self.select(cluster);
                self.set_ktab(KTab::Status);
                let rb = match self.forest.task(cluster, serial).map(|t| self.forest.get(cluster).task_ripple(t)) {
                    Some(RippleState::Parked) => RBand::Parked,
                    Some(RippleState::Pending) => RBand::Pending,
                    _ => RBand::Active,
                };
                self.set_band(Band::Ripples, true);
                self.set_band(Band::RippleState(rb), true);
                self.focus = Focus::Content;
                self.select_item(Item::Ripple { cluster, serial });
            }
            // The bus (msg 235): an event, a trigger, a ripple or an
            // execution opens in the bus modal; `show more` adds a page.
            SAct::Bus(page) => self.open_bus(page),
            // A ripple on the Ledger (msg 457): explored in the bus modal.
            SAct::Exec { seq, path, .. } => self.open_bus(crate::cauldron::bus::BusPage::Exec { seq, path }),
            SAct::More(class) => {
                *self.bus_pages.entry(class).or_insert(0) += 1;
            }
            // A service's control (msg 403): asked first, in the confirm.
            SAct::Service { device, service, op } => self.start_act(crate::cauldron::manage::Act::Service { device, service, op }),
            SAct::Show => return false,
        }
        true
    }

    /// Space on the Status tab: an account's band flips it, as on Config
    /// (0142 §8: "so he can turn an account on from Status when he sees it
    /// is the reason things are parked").
    pub fn status_flip(&mut self) -> bool {
        let v = crate::cauldron::status::view(self, 120);
        let Some(it) = v.items.get(self.rows[0]).cloned() else { return false };
        let Some((pool, id)) = it.key.strip_prefix("acct:").and_then(|k| k.split_once('/')) else { return false };
        let (pool, id) = (pool.to_string(), id.to_string());
        let b = Bound::new("pools.{pool}.accounts.{account}.enabled", settings::Ctx::account(&pool, &id));
        // On ⇄ off, whatever it inherits: its state word says which it is.
        let on = self.sget(&b).val.as_bool();
        self.set_option(&b, if on { 2 } else { 1 });
        true
    }

    /// The Status tab's sub-tab `t`, the selection on its strip.
    pub fn set_sttab(&mut self, t: crate::cauldron::status::StatusTab) {
        self.sttab = t;
        self.rows[0] = 0;
    }

    /// A click on sub-tab `i` of the strip on the cauldron tab shown.
    pub fn pick_strip(&mut self, i: usize) {
        match self.ctab {
            CTab::Status => {
                if let Some(&t) = crate::cauldron::status::StatusTab::ALL.get(i) {
                    self.set_sttab(t);
                }
            }
            CTab::Config => {
                if let Some(&t) = crate::cauldron::menu::CfgTab::ALL.get(i) {
                    self.set_cfg_tab(t);
                }
            }
            CTab::Cauldron | CTab::Agents => {}
        }
    }

    /// → and ← on a ripple of the Ledger that ran others (msg 457: expand and
    /// collapse follow the band standard): → opens what it ran, and on an
    /// open one steps to the first of them; ← shuts an open one. Enter and a
    /// click explore it instead. False on any other row, and for ← on a shut
    /// one or one that ran nothing, which climbs as every row does.
    fn ledger_turn(&mut self, right: bool) -> bool {
        use crate::cauldron::status::SAct;
        let v = crate::cauldron::status::view(self, 120);
        let Some(it) = v.items.get(self.rows[0]).cloned() else { return false };
        let SAct::Exec { inside, .. } = it.act else { return false };
        let open = crate::cauldron::status::is_open(self, &it.key);
        match (right, open) {
            (true, _) if inside == 0 => return false,
            (true, true) => self.rows[0] = (self.rows[0] + 1).min(v.items.len().saturating_sub(1)),
            (false, false) => return false,
            (false, true) if inside == 0 => return false,
            _ => {
                if !self.stnav.remove(&it.key) {
                    self.stnav.insert(it.key);
                }
            }
        }
        true
    }

    /// ← on the Status tab: an open band closes; a row climbs to its band;
    /// on the strip, the sub-tab before.
    pub fn status_back(&mut self) -> bool {
        let v = crate::cauldron::status::view(self, 120);
        let Some(it) = v.items.get(self.rows[0]).cloned() else { return false };
        if it.act == crate::cauldron::status::SAct::Strip {
            let i = crate::cauldron::status::StatusTab::ALL.iter().position(|t| *t == self.sttab).unwrap_or(0);
            if i == 0 {
                return false;
            }
            self.set_sttab(crate::cauldron::status::StatusTab::ALL[i - 1]);
            return true;
        }
        if crate::cauldron::status::is_open(self, &it.key) && matches!(it.act, crate::cauldron::status::SAct::Band) {
            if !self.stnav.remove(&it.key) {
                self.stnav.insert(it.key);
            }
            return true;
        }
        match it.parent.and_then(|p| v.items.iter().position(|x| x.key == p)) {
            Some(i) => {
                self.rows[0] = i;
                true
            }
            None => false,
        }
    }

    fn row_tag(&self) -> Option<Tag> {
        match *self.dash_rows().get(self.row())? {
            Row::Status(i) => crate::cauldron::status::view(self, 120).items.into_iter().nth(i).and_then(|it| it.tag),
            Row::Setting(i) => self.setting_tag(i),
        }
    }

    // ── the input ─────────────────────────────────────────────────────────

    /// The REFERENCE at the bucket's caret, on its line, as a byte range
    /// with the one space after it when it has one: a reference the caret is
    /// inside or at either end of, or one that ends ONE space before it — the
    /// space a click leaves after what it puts in. Two spaces part it from
    /// the caret: that is how he keeps several (b964d571 msg 23: "if there's
    /// already an item at the position of the caret then let's replace that
    /// item. If we want multiple then we can separate by a space"). A paste
    /// chip is never one: it is what he put in, and a click never takes it.
    pub fn ref_at_caret(&self) -> Option<std::ops::Range<usize>> {
        let cur = self.composer.cursor();
        let line = &self.composer.lines()[cur.line];
        let b = cur.byte;
        tags::scan(line)
            .into_iter()
            .filter(|(_, t)| !matches!(t, Tag::Chip { .. }))
            .map(|(r, _)| r)
            .find(|r| (r.start <= b && b <= r.end) || (r.end + 1 == b && line.as_bytes()[r.end] == b' '))
            .map(|r| if line[r.end..].starts_with(' ') { r.start..r.end + 1 } else { r })
    }

    /// Put `tag` into the draft at the caret, spaced off from its neighbours —
    /// in place of the reference at the caret, when there is one
    /// ([`App::ref_at_caret`]).
    pub fn insert_tag(&mut self, tag: &Tag) {
        self.put_tag(tag);
        self.save_draft();
    }

    /// [`App::insert_tag`] without the save: the reference it replaced, if
    /// it replaced one.
    fn put_tag(&mut self, tag: &Tag) -> Option<String> {
        // The anchors follow what he typed before this insert lands.
        self.refs.follow_to(&self.composer.text());
        let out = self.put_tag_text(tag);
        // The caret sits after the reference and the one space after it.
        let at = crate::cauldron::dump::caret_offset(&self.composer).saturating_sub(tag.render().len() + 1);
        self.note_origin(tag, at);
        out
    }

    /// Where a reference put in from here came from: the cluster, the
    /// cauldron tab and its sub-tab on screen (8e4fca10 msg 385).
    pub fn origin_here(&self) -> crate::cauldron::refs::Origin {
        let tab = match self.ctab {
            CTab::Status => self.sttab.label().to_ascii_lowercase(),
            CTab::Config => self.cfg_tab.slug().to_string(),
            _ => self.ktab.label().to_ascii_lowercase(),
        };
        crate::cauldron::refs::Origin { cluster: self.forest.get(self.selected).key.clone(), ctab: self.ctab.label().to_ascii_lowercase(), tab, aggregate: self.aggregate }
    }

    /// Record that `tag`, at byte `at` of the bucket, came from here. A
    /// paste chip is the bucket's own and has no origin.
    fn note_origin(&mut self, tag: &Tag, at: usize) {
        if matches!(tag, Tag::Chip { .. }) {
            return;
        }
        let origin = self.origin_here();
        self.refs.add(&self.composer.text(), at, &tag.render(), origin);
    }

    fn put_tag_text(&mut self, tag: &Tag) -> Option<String> {
        // The reference he went by stays: the caret steps to its end, and
        // the new one goes beside it.
        if let Some((then, at, raw)) = self.went_by.take() {
            // Where it is now, after whatever another face typed (0305 K2).
            let now = self.composer.text();
            let mut e = vec![crate::cauldron::refs::Entry { id: String::new(), tag: raw.clone(), at, origin: Default::default() }];
            crate::cauldron::refs::follow(&then, &now, &mut e);
            let caret = crate::cauldron::dump::caret_offset(&self.composer);
            let line_start = caret - self.composer.cursor().byte;
            let still = e.first().map(|e| e.at).filter(|_| e.len() == 1);
            if let (Some(p), Some(r)) = (still, self.ref_at_caret()) {
                if line_start + r.start == p {
                    let at = crate::cauldron::dump::caret_offset(&self.composer) - self.composer.cursor().byte + r.end;
                    crate::cauldron::dump::set_caret(&mut self.composer, at);
                    let line = &self.composer.lines()[self.composer.cursor().line];
                    let s = if line[..self.composer.cursor().byte].ends_with(' ') { format!(" {} ", tag.render()) } else { format!("  {} ", tag.render()) };
                    self.composer.insert_paste(&s);
                    return None;
                }
            }
        }
        if let Some(r) = self.ref_at_caret() {
            let line = self.composer.cursor().line;
            let old = self.composer.lines()[line][r.clone()].to_string();
            self.composer.replace_span(line, r, &format!("{} ", tag.render()));
            return Some(old);
        }
        // Inside a word — one another face is typing, as its merge left the
        // caret — the reference goes to the nearest word boundary, never
        // into the middle of it (8e4fca10 mail #68, H6).
        let cur = self.composer.cursor();
        let line = &self.composer.lines()[cur.line];
        // Inside a paste chip — a merge can leave the caret in its label —
        // the reference goes to the chip's end, never into it (0316 K6).
        if let Some((r, _)) = tags::scan(line).into_iter().find(|(r, t)| matches!(t, Tag::Chip { .. }) && r.start < cur.byte && cur.byte < r.end) {
            let at = crate::cauldron::dump::caret_offset(&self.composer) - cur.byte + r.end;
            crate::cauldron::dump::set_caret(&mut self.composer, at);
        }
        let cur = self.composer.cursor();
        let line = &self.composer.lines()[cur.line];
        let (b, word) = (cur.byte, |c: char| !c.is_whitespace());
        if line[..b].chars().next_back().is_some_and(word) && line[b..].chars().next().is_some_and(word) {
            let left = line[..b].rfind(char::is_whitespace).map_or(0, |i| i + line[i..].chars().next().map_or(1, char::len_utf8));
            let right = line[b..].find(char::is_whitespace).map_or(line.len(), |i| b + i);
            let to = if line[left..b].chars().count() < line[b..right].chars().count() { left } else { right };
            let at = crate::cauldron::dump::caret_offset(&self.composer) - b + to;
            crate::cauldron::dump::set_caret(&mut self.composer, at);
        }
        let cur = self.composer.cursor();
        let line = &self.composer.lines()[cur.line];
        let before = &line[..cur.byte];
        let mut s = String::new();
        if !before.is_empty() && !before.ends_with(char::is_whitespace) {
            s.push(' ');
        }
        s.push_str(&tag.render());
        // Exactly one space after it: when one already follows, the caret
        // steps over it instead of a second going in (0316 K5).
        let spaced = line[cur.byte..].starts_with(' ');
        if !spaced {
            s.push(' ');
        }
        self.composer.insert_paste(&s);
        if spaced {
            let at = crate::cauldron::dump::caret_offset(&self.composer) + 1;
            crate::cauldron::dump::set_caret(&mut self.composer, at);
        }
        None
    }

    /// The reference in the bucket under the caret a click just placed: the
    /// reference whose characters the click landed on (a paste chip is the
    /// bucket's own and is none), as the byte of its `[` in the whole text
    /// and the reference.
    pub fn ref_under_caret(&self) -> Option<(usize, Tag)> {
        self.ref_under_caret_raw().map(|(at, _, t)| (at, t))
    }

    /// [`App::ref_under_caret`], with the reference as WRITTEN in the bucket.
    pub fn ref_under_caret_raw(&self) -> Option<(usize, String, Tag)> {
        let cur = self.composer.cursor();
        let line = &self.composer.lines()[cur.line];
        let (r, tag) = tags::scan(line).into_iter().filter(|(_, t)| !matches!(t, Tag::Chip { .. })).find(|(r, _)| r.start <= cur.byte && cur.byte < r.end)?;
        let at = crate::cauldron::dump::caret_offset(&self.composer) - (cur.byte - r.start);
        Some((at, line[r].to_string(), tag))
    }

    /// **A click on a reference in the bucket goes to it** (8e4fca10 msg 385):
    /// to the cluster, the tab and the item it was put in from, as the index
    /// recorded it ([`crate::cauldron::refs`]); with no entry — typed by hand,
    /// or put in by an older face — to where its soup id lives. The page
    /// scrolls to the item and selects it; a closed band that holds it opens.
    /// The bucket is never touched, and on Surface it keeps the keys, so what
    /// he types next still goes into it. True when it found the item.
    pub fn goto_ref(&mut self, at: usize, tag: &Tag) -> bool {
        let text = self.composer.text();
        let entry = self.refs.at(&text, at).filter(|e| e.tag == tag.render());
        let focus = self.focus;
        let found = match &entry {
            Some(e) => self.goto_origin(tag, &e.origin) || self.goto_home(tag),
            None => self.goto_home(tag),
        };
        // On Surface the bucket keeps the focus a click in it gave.
        if self.ctab == CTab::Cauldron && focus == Focus::Input {
            self.focus = Focus::Input;
        }
        debug_assert_eq!(self.composer.text(), text, "navigation never changes the bucket");
        let r = tag.render();
        self.notice = Some(match (found, &entry) {
            // Status, Config and Agents are not under a cluster: their path
            // is the tab's alone (0316 D2).
            (true, _) if self.ctab == CTab::Cauldron => format!("{r} — {} › {}", self.forest.get(self.selected).name, self.where_label()),
            (true, _) => format!("{r} — {}", self.where_label()),
            (false, _) => format!("{r} is nowhere in this cauldron now"),
        });
        found
    }

    /// The tab on screen, as the notice names it.
    fn where_label(&self) -> String {
        match self.ctab {
            CTab::Cauldron => self.named(self.ktab.label()),
            CTab::Status => format!("Status › {}", self.sttab.label()),
            CTab::Config => format!("Config › {}", self.cfg_tab.label()),
            CTab::Agents => "Agents".into(),
        }
    }

    /// Go to `tag` where `o` says it was put in from.
    fn goto_origin(&mut self, tag: &Tag, o: &crate::cauldron::refs::Origin) -> bool {
        let Some(g) = self.forest.find(&o.cluster) else { return false };
        match CTab::stored(&o.ctab) {
            Some(CTab::Status) => {
                self.set_ctab(CTab::Status);
                if let Some(t) = crate::cauldron::status::StatusTab::parse(&o.tab) {
                    self.set_sttab(t);
                }
                self.select(g);
                self.select_row_tag(tag)
            }
            Some(CTab::Config) => {
                self.set_ctab(CTab::Config);
                if let Some(t) = crate::cauldron::menu::CfgTab::parse(&o.tab) {
                    self.set_cfg_tab(t);
                }
                self.select_row_tag(tag)
            }
            _ => {
                let Some(t) = KTab::parse(&o.tab) else { return false };
                self.set_ctab(CTab::Cauldron);
                self.select(g);
                self.set_aggregate(o.aggregate);
                self.set_ktab(t);
                self.select_tag_on_page(tag)
            }
        }
    }

    /// Go to `tag` by its soup id alone: the cluster that holds it, and the
    /// tab it lives on.
    fn goto_home(&mut self, tag: &Tag) -> bool {
        // A cluster, a dump and a message have a home too, typed by hand or
        // with their entry lost (0305 K4: `[X-1]` and `[dump:11]` said they
        // were nowhere though both exist).
        match tag {
            // A cluster: its Details, on its own description.
            Tag::Thing { kind, name } if kind == "cluster" => {
                let Some(g) = self.forest.find(name) else { return false };
                self.set_ctab(CTab::Cauldron);
                self.select(g);
                self.set_ktab(KTab::Details);
                self.select_tag_on_page(tag);
                return true;
            }
            // A dump (or a note of one, `[dump:11.2]`): its band on Status ›
            // Ledger, which lists every dump (8e4fca10 msg 457).
            Tag::Thing { kind, name } if kind == "dump" || kind == "send" => {
                let seq = name.split('.').next().unwrap_or(name).to_string();
                let dump = Tag::thing("dump", &seq);
                // Only to a row that is there: a dump that is not stays put,
                // and is said to be nowhere where he is (0316 D2).
                if !self.status_has_row(crate::cauldron::status::StatusTab::Ledger, &dump) {
                    return false;
                }
                self.set_ctab(CTab::Status);
                self.set_sttab(crate::cauldron::status::StatusTab::Ledger);
                return self.select_row_tag(&dump);
            }
            // A message: the background item that cites it, in whichever
            // cluster holds it. Background runs live on the cluster's STATUS
            // tab, in a band that may be shut (0316 D1: searching Details
            // alone never found one).
            Tag::Thing { kind, .. } if kind == "msg" => {
                let (sel, ctab, ktab) = (self.selected, self.ctab, self.ktab);
                for g in 0..self.forest.clusters.len() {
                    for t in [KTab::Status, KTab::Details] {
                        self.set_ctab(CTab::Cauldron);
                        self.select(g);
                        self.set_ktab(t);
                        if self.select_tag_on_page(tag) {
                            return true;
                        }
                    }
                }
                self.set_ctab(ctab);
                self.select(sel);
                self.set_ktab(ktab);
                return false;
            }
            _ => {}
        }
        let tab = match tag {
            Tag::Item { item } => {
                let base = item.split('.').next().unwrap_or(item);
                match base.split('-').next().unwrap_or("") {
                    "Q" => Some(KTab::Questions),
                    "HA" => Some(KTab::Actions),
                    "T" => Some(KTab::Tasks),
                    "M" | "OBJ" | "DIR" | "CON" | "X" => Some(KTab::Details),
                    _ => Some(KTab::Nodes),
                }
            }
            Tag::Thing { kind, .. } if matches!(kind.as_str(), "region" | "node" | "repo" | "path") => Some(KTab::Nodes),
            Tag::Thing { kind, .. } if matches!(kind.as_str(), "ripple" | "run" | "event" | "service" | "device") => None,
            Tag::Thing { kind, .. } if kind == "setting" => None,
            _ => return false,
        };
        let home = self.clusters_of(tag).first().and_then(|k| self.forest.find(k));
        match (tab, tag) {
            (Some(t), _) => {
                let g = home.unwrap_or(self.selected);
                self.set_ctab(CTab::Cauldron);
                self.select(g);
                self.set_ktab(t);
                if self.select_tag_on_page(tag) {
                    return true;
                }
                // A region or a soup node: open it in its REGIONS band.
                match tag {
                    Tag::Thing { kind, name } if kind == "region" || kind == "node" => {
                        let focus = self.focus;
                        let ok = if kind == "region" { self.open_ids(Some(name), None) } else { self.open_ids(None, Some(name)) }.is_ok();
                        self.focus = focus;
                        ok
                    }
                    Tag::Item { .. } if !self.aggregate => {
                        self.set_aggregate(true);
                        self.select_tag_on_page(tag)
                    }
                    _ => false,
                }
            }
            (None, Tag::Thing { kind, .. }) if kind == "setting" => {
                self.set_ctab(CTab::Config);
                crate::cauldron::menu::CfgTab::ALL.into_iter().any(|t| {
                    self.set_cfg_tab(t);
                    self.select_row_tag(tag)
                })
            }
            (None, _) => {
                let Some(t) = crate::cauldron::status::StatusTab::ALL.into_iter().find(|&t| self.status_has_row(t, tag)) else { return false };
                self.set_ctab(CTab::Status);
                self.set_sttab(t);
                self.select_row_tag(tag)
            }
        }
    }

    /// Whether Status › `t` has a row whose reference is `tag`, looked at
    /// without going there: nothing he is on changes.
    fn status_has_row(&mut self, t: crate::cauldron::status::StatusTab, tag: &Tag) -> bool {
        let (ctab, sttab) = (self.ctab, self.sttab);
        (self.ctab, self.sttab) = (CTab::Status, t);
        let want = tag.render();
        let found = crate::cauldron::status::view(self, 120).items.into_iter().any(|it| it.tag.is_some_and(|t| t.render() == want));
        (self.ctab, self.sttab) = (ctab, sttab);
        found
    }

    /// Select the row of the Status or Config tab on screen whose reference
    /// is `tag`.
    fn select_row_tag(&mut self, tag: &Tag) -> bool {
        let Some(ix) = self.dash_index() else { return false };
        let want = tag.render();
        let n = self.dash_rows().len();
        let tags: Vec<Option<String>> = match self.ctab {
            CTab::Status => crate::cauldron::status::view(self, 120).items.into_iter().map(|it| it.tag.map(|t| t.render())).collect(),
            _ => (0..n).map(|i| self.setting_tag(i).map(|t| t.render())).collect(),
        };
        let Some(i) = tags.iter().position(|t| t.as_deref() == Some(want.as_str())) else { return false };
        self.rows[ix] = i;
        self.reveal = true;
        true
    }

    /// Select the item of the cluster page on screen whose reference is
    /// `tag`, scrolled to. When a closed band holds it, the bands it sits in
    /// open, and only those.
    fn select_tag_on_page(&mut self, tag: &Tag) -> bool {
        let want = tag.render();
        let is = |a: &App, it: Item| a.tag_for(it).is_some_and(|t| t.render() == want);
        if let Some(it) = self.items().into_iter().find(|&it| is(self, it)) {
            self.select_item(it);
            return true;
        }
        let kept = self.bands.clone();
        let mut closable: Vec<Band> = Vec::new();
        closable.extend([QBand::Active, QBand::Assumed, QBand::Complete, QBand::Superseded, QBand::Archived].map(Band::Questions));
        closable.extend([HaBand::Active, HaBand::Done, HaBand::Archived].map(Band::Actions));
        closable.extend([RBand::Active, RBand::Parked, RBand::Pending, RBand::Background].map(Band::RippleState));
        closable.extend([Band::Ripples, Band::Engine, Band::EngineAcross]);
        closable.extend((0..self.forest.clusters.len()).map(Band::EngineCluster));
        for &b in &closable {
            self.set_band(b, true);
        }
        let page = crate::cauldron::pages::page(self, 100);
        let Some(i) = page.ids.iter().position(|&it| is(self, it)) else {
            self.bands = kept;
            return false;
        };
        let it = page.ids[i];
        // The bands it sits in, up its parents.
        let mut need = Vec::new();
        let mut up = page.parents.get(i).copied().flatten();
        while let Some(p) = up {
            if let Item::Band(b) = p {
                need.push(b);
            }
            up = page.ids.iter().position(|x| *x == p).and_then(|k| page.parents.get(k).copied().flatten());
        }
        self.bands = kept;
        for b in need {
            self.set_band(b, true);
        }
        self.select_item(it);
        true
    }

    /// The tag of whatever is selected in the pane that has the focus (the
    /// page's item, or the tab's row). The tree's cluster has none.
    pub fn selected_tag(&self) -> Option<Tag> {
        match (self.ctab, self.focus) {
            (CTab::Cauldron, Focus::Tree) => None,
            (CTab::Cauldron, _) => self.items().get(self.item).and_then(|&i| self.tag_for(i)),
            _ => self.row_tag(),
        }
    }

    /// `i`: QUOTE the selected item into the bucket. His macro (8e4fca10 msg
    /// 82): `[⌥↓, newline, newline, "> [tag]"]` — see [`App::quote_tag`]. On
    /// Surface the bucket takes the focus so he can go on writing; the
    /// bucket is only on Surface (msg 165), so on another tab the quote goes
    /// into it and he stays where he is.
    pub fn insert_selected(&mut self) {
        let tag = self.selected_tag();
        if self.ctab == CTab::Cauldron {
            self.focus_input();
        }
        if let Some(t) = tag {
            self.quote_tag(&t);
            if self.ctab != CTab::Cauldron {
                self.notice = Some(format!("quoted {} into the bucket", t.render()));
            }
        }
    }

    /// The `i` macro. ⌥↓ is pressed until it stops — the end of the draft,
    /// by paragraph, so the quote never lands in the middle of what he wrote
    /// — then two new lines, then `> ` and the tag. An EMPTY draft (no
    /// character at all) skips the ⌥↓ and the two new lines: the draft
    /// becomes exactly `> [tag]`, with nothing above it. Nothing is added
    /// after the tag: the caret sits right after its `]`.
    pub fn quote_tag(&mut self, tag: &Tag) {
        let before = self.composer.text();
        let c = &mut self.composer;
        // Blank lines (and spaces) at the end of the draft go first, so the
        // quote always sits exactly two new lines below the last text (F53),
        // and a draft of nothing but blanks takes the quote alone.
        let text = c.text();
        let trail = text.chars().rev().take_while(|ch| ch.is_whitespace()).count();
        if trail > 0 {
            c.buffer_end();
            for _ in 0..trail {
                c.backspace();
            }
        }
        if !c.text().is_empty() {
            while c.para_down() {}
            c.buffer_end();
            c.newline();
            c.newline();
        }
        self.refs.follow_to(&before);
        c.insert_paste(&format!("> {}", tag.render()));
        // Nothing follows the tag: the caret sits right after its `]`.
        let at = crate::cauldron::dump::caret_offset(&self.composer).saturating_sub(tag.render().len());
        self.note_origin(tag, at);
        self.save_draft();
    }

    /// Send the dump — the whole of it (Enter): append it to the
    /// environment's dump log, its notes and chips read off it, and clear
    /// it everywhere.
    pub fn submit(&mut self) {
        if self.composer.is_blank() {
            return;
        }
        let text = self.composer.text();
        if self.send_text(&text) {
            self.composer = Composer::new();
            self.save_draft();
        }
    }

    /// Send `text` as one dump into the soup: a dump record (its number, the
    /// text, its notes with their anchors, its chips' bodies by id — design
    /// 0138 §4.5), with the cluster he was looking at as where it was sent
    /// from. It lands on the clusters its tags point into, and an untagged
    /// dump on none: it goes into the one soup (msg 235: "We're filling up
    /// our bucket and we're dumping it into the cauldron"). False when it
    /// was not stored.
    pub fn send_text(&mut self, text: &str) -> bool {
        let from = self.forest.get(self.selected).key.clone();
        let env = self.env.name.clone();
        // Resolve every tag against the soup first: the log is borrowed
        // mutably for the append.
        let resolved: Vec<(Tag, Vec<String>)> = tags::scan(text)
            .into_iter()
            .map(|(_, t)| {
                let gs = self.clusters_of(&t);
                (t, gs)
            })
            .collect();
        let clusters_of =
            |t: &Tag| resolved.iter().find(|(x, _)| x == t).map(|(_, g)| g.clone()).unwrap_or_default();
        let artifacts = self.artifact_refs(text);
        match self.sends.append(&env, &from, text, artifacts, clusters_of) {
            Ok(send) => {
                let names: Vec<String> = send
                    .targets
                    .iter()
                    .map(|k| self.forest.find(k).map_or(k.clone(), |g| self.forest.get(g).name.clone()))
                    .collect();
                let n = send.notes.len();
                let to = if names.is_empty() { "the soup".to_string() } else { names.join(", ") };
                self.notice = Some(format!("dump {} · {n} note{} → {to}", send.seq, if n == 1 { "" } else { "s" }));
                true
            }
            Err(e) => {
                self.error = Some(format!("dump NOT stored: {e:#}"));
                false
            }
        }
    }

    /// Type a string and submit it, exactly as keystrokes would.
    pub fn dump(&mut self, text: &str) {
        self.composer.insert_paste(text);
        self.save_draft();
        self.submit();
    }

    /// Type into the draft without sending.
    pub fn type_text(&mut self, text: &str) {
        self.composer.insert_paste(text);
        self.save_draft();
    }

    /// Give the bucket the selection. It is on Surface only (msg 165): from
    /// another tab, this goes to Surface first.
    pub fn focus_input(&mut self) {
        if self.ctab != CTab::Cauldron {
            self.set_ctab(CTab::Cauldron);
        }
        if self.focus != Focus::Input {
            self.back_to = self.focus;
            self.focus = Focus::Input;
        }
        self.searching = false;
    }

    /// Hand the selection back to the pane the dump was entered from.
    fn leave_input(&mut self) {
        self.focus = if self.ctab == CTab::Cauldron { self.back_to } else { Focus::Content };
        if self.focus == Focus::Input {
            self.focus = Focus::Tree;
        }
    }

    // ── the divider ───────────────────────────────────────────────────────

    /// The sidebar's width inside `width` columns: the dragged share, or the
    /// default (a third, 16 to 36 columns). Always leaves the page room.
    pub fn sidebar_width(&self, width: u16) -> u16 {
        const MIN_SIDEBAR: u16 = 12;
        const MIN_PAGE: u16 = 24;
        let w = match self.layout.sidebar_permille {
            Some(p) => ((u32::from(width) * u32::from(p) + 500) / 1000) as u16,
            None => (width / 3).clamp(16, 36),
        };
        w.min(width.saturating_sub(MIN_PAGE)).max(MIN_SIDEBAR).min(width)
    }

    /// The bucket's text rows (msg 235: "We should have a minimum boundary
    /// size, a minimum like height, like content height for the bucket of
    /// three lines and a maximum of six lines. But if you drag past the
    /// maximum, that's okay"): as many as it holds, three to six; dragged,
    /// as many as the divider was dragged to, three at least. Never so many
    /// that the soup above it loses its room (`room`).
    pub fn bucket_rows(&self, width: usize, room: u16) -> u16 {
        let rows = match self.layout.bucket_rows {
            Some(n) => n.max(BUCKET_MIN),
            None => (self.composer.visual_rows(width.max(1)) as u16).clamp(BUCKET_MIN, BUCKET_MAX),
        };
        rows.min(room.max(BUCKET_MIN))
    }

    /// The bucket's divider is at row `row`: the bucket's box starts there.
    fn drag_bucket(&mut self, row: u16) {
        let area = self.surface_area;
        if area.height == 0 {
            return;
        }
        // The box's bottom border is the area's last row, its top the
        // divider: the text rows are the rows between them.
        let rows = area.bottom().saturating_sub(row).saturating_sub(2);
        self.layout.bucket_rows = Some(rows.max(BUCKET_MIN));
    }

    /// The divider is at column `col`: the sidebar ends there.
    fn drag_divider(&mut self, col: u16) {
        let area = self.clusters_area;
        if area.width == 0 {
            return;
        }
        // The divider is the sidebar's last column, so the sidebar is one
        // column wider than the offset of the column the pointer is on.
        let w = col.saturating_sub(area.x).saturating_add(1);
        let w = w.clamp(1, area.width);
        self.layout.sidebar_permille = Some(((u32::from(w) * 1000 + u32::from(area.width) / 2) / u32::from(area.width)) as u16);
    }

    pub(crate) fn save_layout(&mut self) {
        if let Err(e) = self.layout_store.save(&self.layout) {
            self.error = Some(format!("layout NOT saved: {e:#}"));
        }
    }

    // ── events ─────────────────────────────────────────────────────────────

    pub fn handle(&mut self, event: Event) {
        // A row's modal stays under what is asked from it (a picker, a
        // name, an explanation), and comes back when that closes — unless
        // what it was the row of is gone.
        let row = self.modal.clone().filter(|m| matches!(m, Modal::Settings { .. }));
        if let (Event::Paste(s), Some(Modal::Settings { .. }), Some(f)) = (&event, &self.modal, &mut self.field) {
            f.text.push_str(s.trim());
            return;
        }
        self.handle_event(event);
        if row.is_some() && self.modal.as_ref().is_some_and(|m| !matches!(m, Modal::Settings { .. })) {
            self.modal_back = row;
        }
        if self.modal.is_none() {
            if let Some(Modal::Settings { page, sel }) = self.modal_back.take() {
                if self.row_stands(&page) {
                    self.modal = Some(Modal::Settings { page, sel });
                }
            }
        }
        // The endpoint being added lives while its add modal does: closed
        // any way but `add endpoint`, it is forgotten (design 0199 §3).
        let adding = |m: &Option<Modal>| matches!(m, Some(Modal::Settings { page: crate::cauldron::menu::SPage::Endpoint { id, .. }, .. }) if id == crate::cauldron::settings::DRAFT_EP);
        if !adding(&self.modal) && !adding(&self.modal_back) {
            self.settings.discard_draft();
        }
    }

    fn handle_event(&mut self, event: Event) {
        // A notice, like the copied cell's flash, lasts until the next key,
        // press or scroll: the release of the click that raised it is part of
        // that click, and a motion is not an act (0214 A2: a mouse-raised
        // notice used to be wiped by its own release, and showed nothing).
        if matches!(event, Event::Key(_) | Event::Paste(_) | Event::Mouse(MouseEvent { kind: MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown, .. })) {
            self.notice = None;
        }
        if matches!(event, Event::Key(_) | Event::Mouse(MouseEvent { kind: MouseEventKind::Down(_), .. })) {
            self.flash = None;
        }
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
            Event::Paste(s) => match &mut self.modal {
                Some(Modal::NewCluster { name, boundaries, at, caret, .. }) => {
                    let (field, text) = if *at == 0 { (name, s.trim().replace('\n', " ")) } else { (boundaries, s.replace('\r', "")) };
                    let c = (*caret).min(field.len());
                    field.insert_str(c, &text);
                    *caret = c + text.len();
                }
                Some(
                    Modal::NewCauldron { name, .. }
                    | Modal::AddAccount { name, .. }
                    | Modal::AddItem { name, .. }
                    | Modal::Name { name, .. }
                    | Modal::DevRename { name, .. },
                ) => name.push_str(s.trim()),
                Some(Modal::Chip { editor, .. }) => editor.insert_paste(&s),
                // A modal with no field to take it (a confirm — a service's
                // restart, msg 403 — a picker, a page): the paste is not
                // dropped without a word. It stays on the clipboard; he is
                // told where it goes (task 0263's adversarial pass: his
                // dictation arrives as a paste).
                Some(_) if !s.trim().is_empty() => self.notice = Some("not pasted: this has no field — Esc, then paste into the bucket".into()),
                Some(_) => {}
                None if self.ctab == CTab::Agents => crate::cauldron::threads::paste(self, &s),
                None if self.field.is_some() => {
                    if let Some(f) = &mut self.field {
                        f.text.push_str(s.trim());
                    }
                }
                None if self.searching => {
                    let q = format!("{}{}", self.search, s.replace(['\n', '\r'], " "));
                    self.set_search(&q);
                }
                // The bucket takes no free keys outside itself (msg 165), a
                // paste included: it lands only where the selection is — as
                // a PASTE chip, whatever it is (msg 235: "a paste should be
                // inserted as like a paste like artifact"), which is also
                // how a speech-to-text tool's words arrive ("it just comes
                // in as a paste").
                None if self.focus == Focus::Input => self.paste_into_dump(&s),
                None => self.notice = Some("the bucket is not open: `a` opens it, then paste".into()),
            },
            Event::Mouse(m) => self.mouse(m),
            _ => {}
        }
    }

    fn hit_at(&self, col: u16, row: u16) -> Option<Hit> {
        // Last recorded wins: overlays are drawn (and recorded) after what
        // they cover.
        self.hits
            .iter()
            .rev()
            .find(|(r, _)| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height)
            .map(|(_, h)| *h)
    }

    fn mouse(&mut self, m: MouseEvent) {
        let hit = self.hit_at(m.column, m.row);
        if self.modal.is_some() || self.dropdown.is_some() {
            return self.modal_mouse(m, hit);
        }
        // A press is remembered: the second of two on the same thing,
        // inside DOUBLE_CLICK, is a double click.
        let double = match (m.kind, hit) {
            (MouseEventKind::Down(MouseButton::Left), Some(h)) => {
                let now = std::time::Instant::now();
                let d = self.last_click.is_some_and(|(t, x)| x == h && now.duration_since(t) <= DOUBLE_CLICK);
                self.last_click = if d { None } else { Some((now, h)) };
                d
            }
            (MouseEventKind::Down(_), _) => {
                self.last_click = None;
                false
            }
            _ => false,
        };
        // DEV ONLY — NOT FOR THE PRODUCTION VERSION (msg 235: "if I like
        // double click on one of these things, just let me just edit it in a
        // modal"): a double click on a title or a tab renames it on screen.
        if double {
            let word = match hit {
                Some(Hit::Title(t)) => Some(t.word().to_string()),
                Some(Hit::CTab(t)) => Some(t.label().to_string()),
                Some(Hit::KTab(t)) => Some(t.label().to_string()),
                Some(Hit::Strip(i)) => self.strip_label(i as usize),
                _ => None,
            };
            if let Some(w) = word {
                return self.dev_rename(&w);
            }
        }
        if self.ctab == CTab::Agents && !matches!(hit, Some(Hit::CTab(_) | Hit::Env)) {
            return crate::cauldron::threads::mouse(self, m, hit);
        }
        if matches!(m.kind, MouseEventKind::Down(_)) {
            // A click leaves a value being typed, keeping it.
            self.commit_field(true);
            self.item_press = None;
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => match hit {
                Some(Hit::CTab(t)) => self.set_ctab(t),
                Some(Hit::Env) => self.open_cauldrons(),
                Some(Hit::Strip(i)) => {
                    self.focus = Focus::Content;
                    self.pick_strip(i as usize);
                }
                Some(Hit::KTab(t)) => {
                    self.set_ktab(t);
                    self.focus = Focus::Content;
                }
                Some(Hit::Disclosure(g)) => {
                    self.select(g);
                    self.toggle(g);
                    self.focus = Focus::Tree;
                }
                // A click selects; a click on the selected cluster opens it, as
                // → does: expands it, or steps into its page (rule 9, F32).
                Some(Hit::TreeRow(g)) => {
                    let again = self.selected == g && self.focus == Focus::Tree && !self.on_add_root && !self.on_below;
                    self.select(g);
                    self.focus = Focus::Tree;
                    if again {
                        self.tree_right();
                    }
                }
                // The `+`s ask at once: a root, or one under that cluster.
                // The switch at the sidebar's foot: its name flips it, its
                // `on` and `off` set it.
                Some(Hit::Below) => {
                    self.on_add_root = false;
                    self.on_below = true;
                    self.focus = Focus::Tree;
                    self.toggle_aggregate();
                }
                Some(Hit::BelowOpt(on)) => {
                    self.on_add_root = false;
                    self.on_below = true;
                    self.focus = Focus::Tree;
                    self.set_aggregate(on);
                }
                Some(Hit::AddRoot) => {
                    self.on_below = false;
                    self.on_add_root = true;
                    self.focus = Focus::Tree;
                    self.open_new_cluster(None);
                }
                Some(Hit::AddChild(g)) => {
                    self.select(g);
                    self.focus = Focus::Tree;
                    self.open_new_cluster(Some(g));
                }
                Some(Hit::Divider) => self.dragging = true,
                Some(Hit::BucketDivider) => self.dragging_bucket = true,
                Some(Hit::Item(i)) => self.item_press = Some((i, double)),
                Some(Hit::Row(i)) => {
                    let again = self.row() == i && self.focus == Focus::Content;
                    if let Some(d) = self.dash_index() {
                        self.rows[d] = i;
                    }
                    self.focus = Focus::Content;
                    // A double click quotes what has a reference (msg 186);
                    // the band its first click turned is turned back.
                    if double {
                        if self.click_toggled.take().is_some() {
                            self.toggle_row_band();
                        }
                        if self.selected_tag().is_some() {
                            self.insert_selected();
                        }
                        return;
                    }
                    // A table's row opens its modal at ONE click (msg 235:
                    // "when you click on the row, you go to modal with all
                    // the settings").
                    // So does a value row's picker or editor (0217 B4).
                    if self.ctab == CTab::Config {
                        if let Some(e) = self.settings_entry().filter(|e| matches!(e, crate::cauldron::menu::Entry::Row(_)) || self.is_value_row(e)) {
                            self.open_entry(e);
                            return;
                        }
                    }
                    // A band opens or closes at ONE click (msg 235: "anywhere
                    // that we have a band that can be expanded ... they all
                    // need to toggle by single click").
                    if self.toggle_row_band() {
                        self.click_toggled = Some(i);
                        return;
                    }
                    // A click on the selected row opens it — once.
                    match self.ctab {
                        // `show more` and `config ▸` act at ONE click: they
                        // are buttons, not rows (task 0209 D18).
                        CTab::Status if again || self.status_is_button() => {
                            self.status_open();
                        }
                        // A button presses only on itself (`Hit::Press`).
                        // A switch's label only selects (0217 B17).
                        CTab::Config if again && !self.settings_entry().is_some_and(|e| e.is_button() || self.is_switch(&e)) => {
                            self.settings_open();
                        }
                        _ => {}
                    }
                }
                // An option of a switch on a Config row: picked, at once.
                Some(Hit::Opt { row, cell, opt }) => {
                    self.rows[crate::cauldron::menu::SETTINGS_ROW] = row as usize;
                    self.focus = Focus::Content;
                    if let Some(e) = self.settings_entry() {
                        self.pick_cell(e, cell as usize, opt as usize);
                    }
                }
                // A button on a Config row: pressed, at once.
                Some(Hit::Press(row)) => {
                    self.rows[crate::cauldron::menu::SETTINGS_ROW] = row as usize;
                    self.focus = Focus::Content;
                    if let Some(e) = self.settings_entry() {
                        self.open_entry(e);
                    }
                }
                // The ↺ on a Config row: back to its default.
                Some(Hit::Revert(row)) => {
                    self.rows[crate::cauldron::menu::SETTINGS_ROW] = row as usize;
                    self.focus = Focus::Content;
                    if let Some(e) = self.settings_entry() {
                        self.clear_entry(e);
                    }
                }
                // The ⓘ on a Config row: what it means, in a modal.
                Some(Hit::Tip(i)) => {
                    if self.ctab == CTab::Config {
                        if let Some(f) = self.entries().into_iter().nth(i) {
                            self.help_entry(&f.entry);
                        }
                    }
                }
                Some(Hit::CopyAt(i)) => {
                    if let Some(p) = self.hotspot_paths.get(i as usize).cloned() {
                        self.select_art_path(&p);
                        self.copy_path(p);
                    }
                }
                Some(Hit::OpenAt(i)) => {
                    if let Some(p) = self.hotspot_paths.get(i as usize).cloned() {
                        self.select_art_path(&p);
                        self.open_path(p);
                    }
                }
                // A click in the text puts the caret where it landed (0220
                // E6); on the border or padding it only takes the focus.
                Some(Hit::Input) => {
                    self.focus_input();
                    let (r, top) = self.input_at;
                    if r.contains(ratatui::layout::Position { x: m.column, y: m.row }) {
                        self.composer.place(top + (m.row - r.y) as usize, (m.column - r.x) as usize, self.input_w);
                        // A click on a reference goes to where it came from
                        // (8e4fca10 msg 385).
                        if let Some((at, raw, tag)) = self.ref_under_caret_raw() {
                            self.goto_ref(at, &tag);
                            // The caret is left AFTER the chip, so the next
                            // click goes beside it (8e4fca10 mail #68, J7) —
                            // after the reference as WRITTEN, which for a long
                            // form (`[cluster:X-2]`) is longer than what it
                            // renders as (0305 K3).
                            crate::cauldron::dump::set_caret(&mut self.composer, at + raw.len());
                            self.went_by = Some((self.composer.text(), at, raw));
                        }
                    }
                }
                Some(Hit::BucketCopy) => self.copy_bucket(),
                // A paste chip in the bucket opens at one click (0138 §4.2).
                Some(Hit::Chip(i)) => {
                    self.focus_input();
                    if let Some(t) = self.chip_hits.get(i as usize).cloned() {
                        self.open_chip(&t);
                    }
                }
                Some(
                    Hit::Title(_)
                    | Hit::Modal
                    | Hit::Drop(_)
                    | Hit::Choice(_)
                    | Hit::ChipButton(_)
                    | Hit::ConfirmButton(_)
                    | Hit::BusLink(_)
                    | Hit::TList(_)
                    | Hit::TChat(_)
                    | Hit::TAgent(_)
                    | Hit::TInput
                    | Hit::MRow(_)
                    | Hit::MOpt { .. }
                    | Hit::MRevert(_)
                    | Hit::MTip(_)
                    | Hit::MPress(_),
                )
                | None => {}
            },
            MouseEventKind::Drag(MouseButton::Left) if self.dragging => self.drag_divider(m.column),
            MouseEventKind::Drag(MouseButton::Left) if self.dragging_bucket => self.drag_bucket(m.row),
            // A page item acts at the release of a press on it, over it.
            MouseEventKind::Up(MouseButton::Left) if self.item_press.is_some() => {
                if let (Some((i, double)), Some(Hit::Item(j))) = (self.item_press.take(), hit) {
                    if i == j {
                        self.click_item(i, double);
                    }
                }
            }
            MouseEventKind::Up(_) if self.dragging => {
                self.dragging = false;
                self.drag_divider(m.column);
                self.save_layout();
            }
            MouseEventKind::Up(_) if self.dragging_bucket => {
                self.dragging_bucket = false;
                self.drag_bucket(m.row);
                self.save_layout();
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let d = if m.kind == MouseEventKind::ScrollUp { -1 } else { 1 };
                match hit {
                    Some(Hit::TreeRow(_) | Hit::Disclosure(_) | Hit::AddChild(_)) => {
                        self.tree_move(d);
                    }
                    Some(Hit::Item(_)) => self.scroll = (self.scroll as isize + 3 * d).max(0) as usize,
                    Some(Hit::Row(_) | Hit::Tip(_) | Hit::Opt { .. } | Hit::Revert(_)) => {
                        self.row_move(d);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// A left press on page item `i` (msg 235):
    ///
    /// - a BAND opens or closes at one click ("they all need to toggle by
    ///   single click");
    /// - an item with a reference — anything `i` would quote — puts its
    ///   reference at the bucket's caret ("a single click just inserts it at
    ///   the cursor and the bucket"), the page keeping the selection;
    /// - a second, slower click on the selected item opens it, as before;
    /// - a DOUBLE click takes back what its first press did and quotes the
    ///   item on a new line instead ("Double click makes it a new line with
    ///   a quote").
    fn click_item(&mut self, i: usize, double: bool) {
        let again = self.item == i && self.focus == Focus::Content;
        self.item = i;
        self.focus = Focus::Content;
        self.searching = false;
        // A human action's part is a control (`[ done ]`, a field, a copy, an
        // open): it presses at ONE click and never inserts a reference, and
        // the second click of a double is not a second press. Its reference
        // is the card's to insert (0214 A1).
        if matches!(self.items().get(i), Some(Item::HaPart { .. })) {
            if !double {
                self.click_inserted = None;
                self.click_prior = None;
                self.click_toggled = None;
                self.open_selected();
            }
            return;
        }
        if double {
            if self.click_toggled.take().is_some() {
                self.toggle_item_band();
            }
            // The first press is taken back, so what it replaced is put back
            // at its anchor: nothing he wrote is lost (0258 J2). The item a
            // click JUST put at the caret goes with it (b964d571 msg 23: "Same
            // with double click, it should also clear the item at the current
            // caret and then do what it does"): when that press replaced a
            // CLICK's reference, its `before` already holds the bucket
            // without that click's edit either, so neither comes back — and a
            // reference that click had itself replaced stays gone (0305 K1:
            // click A, click B, double click C left A, never B, in his
            // bucket). Only a reference he typed, or a quote put in, returns.
            self.click_prior = None;
            if let Some(c) = self.click_inserted.take() {
                self.undo_click(c);
            }
            if self.selected_tag().is_some() {
                self.insert_selected();
            }
            return;
        }
        let prior = self.click_inserted.take();
        self.click_prior = None;
        self.click_toggled = None;
        if self.toggle_item_band() {
            self.click_toggled = Some(i);
            return;
        }
        // A button presses at ONE click (task 0209 D18) — `n below` is one
        // too (0217 B4).
        if again || matches!(self.items().get(i), Some(Item::Manage(_) | Item::Footer(Footer::Below))) {
            self.open_selected();
            return;
        }
        if let Some(tag) = self.selected_tag() {
            // What another face wrote comes in first, so the reference at the
            // caret is judged against the bucket as it is (0255 H1).
            match self.draft.poll(&self.composer.text(), crate::cauldron::dump::caret_offset(&self.composer)) {
                Ok(merged) => self.take_merged(merged),
                Err(e) => self.error = Some(format!("reading the bucket: {e:#}")),
            }
            let (mut before, mut caret) = (self.composer.text(), crate::cauldron::dump::caret_offset(&self.composer));
            let replaced = self.put_tag(&tag);
            self.click_prior = prior.filter(|p| replaced.as_deref().map(str::trim) == Some(p.tag.as_str()));
            // It replaced a click's reference: what taking THIS click back
            // restores is the bucket without that click's edit too — read off
            // the bucket as it is, so whatever another face typed in between
            // stays (0305 K1).
            if let Some(p) = &self.click_prior {
                (before, caret) = store::merge3(&p.made, &p.before, &before, p.caret);
            }
            self.click_inserted = Some(ClickInsert { before, caret, made: self.composer.text(), tag: tag.render() });
            self.save_draft();
            self.notice = Some(match replaced {
                Some(old) if old.trim() != tag.render() => format!("replaced {} with {} at the bucket's caret", old.trim(), tag.render()),
                _ => format!("{} at the bucket's caret", tag.render()),
            });
        }
    }

    /// A mouse event while a modal (or the dropdown) is open. Inside it, it
    /// acts on it; a press OUTSIDE it closes it and is swallowed — whatever
    /// was under the click is not touched.
    fn modal_mouse(&mut self, m: MouseEvent, hit: Option<Hit>) {
        let inside = matches!(hit, Some(Hit::Modal | Hit::Drop(_) | Hit::Choice(_) | Hit::ChipButton(_) | Hit::ConfirmButton(_) | Hit::BusLink(_) | Hit::MRow(_) | Hit::MOpt { .. } | Hit::MRevert(_) | Hit::MTip(_) | Hit::MPress(_)));
        // The second press of a double click on the row whose first press
        // opened this modal is part of that click: it never shuts what it
        // opened (0316 D6: a double click on a Ledger ripple explored it and
        // shut it again at once).
        if let (MouseEventKind::Down(MouseButton::Left), Some(h @ Hit::Row(_))) = (m.kind, hit) {
            let now = std::time::Instant::now();
            if self.last_click.is_some_and(|(t, x)| x == h && now.duration_since(t) <= DOUBLE_CLICK) {
                self.last_click = None;
                return;
            }
        }
        match m.kind {
            MouseEventKind::Down(_) if !inside => {
                // The cauldrons dropdown opens at ONE click, over a modal too
                // (0214 D18); a click on it while it is open closes it.
                let reopen = matches!(hit, Some(Hit::Env)) && self.dropdown.is_none();
                self.dropdown = None;
                self.close_modal();
                if reopen && self.modal.is_none() {
                    self.modal_back = None;
                    self.open_cauldrons();
                }
            }
            MouseEventKind::Down(MouseButton::Left) if matches!(hit, Some(Hit::ChipButton(_))) => {
                if let Some(Hit::ChipButton(i)) = hit {
                    self.chip_click(i);
                }
            }
            // A confirm's buttons press at one click (0211 R10), and so do a
            // form's (0214 A6): what Enter and Esc do there.
            MouseEventKind::Down(MouseButton::Left) if matches!(hit, Some(Hit::ConfirmButton(_))) => match (hit, &mut self.modal) {
                (Some(Hit::ConfirmButton(0)), Some(Modal::Confirm { .. })) => self.finish_confirm(),
                (_, Some(Modal::Confirm { .. })) => self.modal = None,
                (Some(Hit::ConfirmButton(b)), _) => {
                    if let Some(Modal::NewCluster { at, caret, .. }) = &mut self.modal {
                        if b == 0 && *at == 0 {
                            *at = 1;
                            *caret = usize::MAX;
                        }
                    }
                    let code = if b == 0 { KeyCode::Enter } else { KeyCode::Esc };
                    self.key(KeyEvent::new(code, KeyModifiers::NONE));
                }
                _ => {}
            },
            // A list follows rule 9 too (F32): a click selects the row, and a
            // click on the selected row acts on it.
            MouseEventKind::Down(MouseButton::Left) => {
                // A cauldron in the dropdown is picked at ONE click (task
                // 0209 D18): a menu, not a list to browse.
                if let Some(Hit::Drop(i)) = hit {
                    if let Some(d) = &mut self.dropdown {
                        d.selected = i;
                        self.pick_cauldron(i);
                    }
                }
                // A picker's option is picked at ONE click (0214 A4).
                if let Some(Hit::Choice(i)) = hit {
                    if let Some(Modal::Picker { selected, .. }) = &mut self.modal {
                        *selected = i;
                        self.pick(i);
                    }
                }
                if let Some(Hit::BusLink(i)) = hit {
                    self.bus_click(i as usize);
                }
                // A row's modal: a click selects a row (a button presses at
                // once, a second click acts), picks a switch's option,
                // reverts, or explains.
                if let Some(Modal::Settings { page, sel }) = self.modal.clone() {
                    let set_sel = |app: &mut App, i: usize| {
                        if let Some(Modal::Settings { sel, .. }) = &mut app.modal {
                            *sel = i;
                        }
                    };
                    match hit {
                        Some(Hit::MRow(i)) => {
                            let i = i as usize;
                            self.commit_field(true);
                            set_sel(self, i);
                            if let Some(e) = self.modal_entry() {
                                // A value row opens at once (0217 B4); a
                                // switch's label only selects (B17).
                                if (sel == i || self.is_value_row(&e)) && !e.is_button() && !self.is_switch(&e) {
                                    self.open_entry(e);
                                }
                            }
                        }
                        Some(Hit::MPress(i)) => {
                            self.commit_field(true);
                            set_sel(self, i as usize);
                            if let Some(e) = self.modal_entry() {
                                self.open_entry(e);
                            }
                        }
                        Some(Hit::MOpt { row, cell, opt }) => {
                            self.commit_field(true);
                            set_sel(self, row as usize);
                            if let Some(e) = self.modal_entry() {
                                self.pick_cell(e, cell as usize, opt as usize);
                            }
                        }
                        Some(Hit::MRevert(row)) => {
                            self.commit_field(true);
                            set_sel(self, row as usize);
                            if let Some(e) = self.modal_entry() {
                                self.clear_entry(e);
                            }
                        }
                        Some(Hit::MTip(i)) => {
                            if let Some(f) = self.modal_entries(&page).into_iter().nth(i as usize) {
                                self.help_entry(&f.entry);
                            }
                        }
                        _ => {}
                    }
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if inside && matches!(self.modal, Some(Modal::Bus { .. })) => {
                self.bus_scroll(if m.kind == MouseEventKind::ScrollUp { -1 } else { 1 });
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if inside => {
                let code = if m.kind == MouseEventKind::ScrollUp { KeyCode::Up } else { KeyCode::Down };
                if self.dropdown.is_some() {
                    self.dropdown_key(KeyEvent::new(code, KeyModifiers::NONE));
                } else {
                    self.modal_key(KeyEvent::new(code, KeyModifiers::NONE));
                }
            }
            _ => {}
        }
    }

    /// Every key. The order IS the philosophy (`docs/navigation.md`): quit,
    /// then a modal (or the dropdown) takes everything, then the tab keys,
    /// then a field, then the region that holds the selection.
    fn key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // A bare letter: no ctrl, ⌥ or ⌘ held.
        let bare = !ctrl && !alt && !key.modifiers.contains(KeyModifiers::SUPER);

        // ^C quits from anywhere, a modal included.
        if key.code == KeyCode::Char('c') && ctrl {
            self.quit = true;
            return;
        }
        // An F-key jumps tabs from a modal that only shows (a row's
        // settings, an explanation, the bus, an artifact): the modal closes,
        // a value being typed kept (task 0209 D19 — after Status' `config ▸`
        // opened a row's modal, F1 did nothing until Esc).
        if matches!(key.code, KeyCode::F(_)) && matches!(self.modal, Some(Modal::Settings { .. } | Modal::Help { .. } | Modal::Bus { .. } | Modal::Artifact { .. })) {
            self.commit_field(true);
            self.close_modal();
        }
        if self.modal.is_some() {
            return self.modal_key(key);
        }
        if self.dropdown.is_some() {
            return self.dropdown_key(key);
        }
        // A click just put a reference in the bucket: what he types next is
        // text, there — a space to keep several (b964d571 msg 23), a word —
        // never a page command (0255 H2: `q` quit the face mid-sentence).
        if self.click_inserted.is_some() && self.focus == Focus::Content && self.ctab == CTab::Cauldron && bare && !self.searching && self.field.is_none() && matches!(key.code, KeyCode::Char(_)) {
            self.click_inserted = None;
            self.focus_input();
        }
        // In the bucket a character that came with no ⌥ is a character he
        // TYPED (0220 E10): £ is ⇧3 on a UK keyboard, å a Nordic key, § an
        // ISO one, and a shortcut there would swallow his text. A ⌥ the
        // terminal reports (kitty, Option-as-Meta) still acts.
        let typed_here = self.focus == Focus::Input && !alt && !ctrl && matches!(key.code, KeyCode::Char(c) if !c.is_ascii());
        if let Some(k) = tab_key(&key).filter(|_| !typed_here) {
            return self.tab(k);
        }
        // The Threads tab is the threads app (msg 235): its own keys.
        if self.ctab == CTab::Agents {
            return crate::cauldron::threads::key(self, key);
        }
        if self.searching {
            return self.search_key(key, ctrl);
        }
        if self.field.is_some() {
            return self.field_key(key, ctrl);
        }
        // ^T: the cauldron's switch (it asks first), from anywhere.
        if key.code == KeyCode::Char('t') && ctrl {
            return self.toggle_cauldron();
        }
        // ⌥A copies the whole bucket (msg 186) — in the bucket and out of
        // it. In iTerm's default Option mode it arrives as `å` (outside the
        // bucket only: in it, `å` is a letter he typed, 0220 E10); Cmd-A never
        // reaches a terminal app at all. There is no dictation (msg 235: "I
        // actually don't want to support transcription directly"): ⌥M is
        // not bound.
        if !ctrl && !typed_here && matches!((key.code, alt), (KeyCode::Char('a' | 'A'), true) | (KeyCode::Char('å' | 'Å'), _)) {
            return self.copy_bucket();
        }
        match key.code {
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
                return;
            }
            KeyCode::PageDown => {
                self.scroll += 10;
                return;
            }
            _ => {}
        }

        if self.focus == Focus::Input {
            return self.input_key(key, ctrl, alt);
        }

        // Outside the dump a printable key is a command, never text, and
        // ^P / ^N are ↑ / ↓.
        let code = match key.code {
            KeyCode::Char('p') if ctrl => KeyCode::Up,
            KeyCode::Char('n') if ctrl => KeyCode::Down,
            c => c,
        };
        let on_work = self.ctab == CTab::Cauldron;
        match code {
            KeyCode::Char('/' | '7' | '_') if ctrl => return self.open_palette(),
            KeyCode::Char('e') if ctrl => return self.open_cauldrons(),
            KeyCode::Char('g') if ctrl => return self.toggle_aggregate(),
            KeyCode::Char('f') if ctrl => return self.start_search(),
            KeyCode::Char('?') => return self.open_palette(),
            KeyCode::Char('q') if bare => {
                self.quit = true;
                return;
            }
            KeyCode::Char('i') if bare => return self.insert_selected(),
            KeyCode::Char('a') if bare => return self.focus_input(),
            KeyCode::Tab if on_work => return self.step_ktab(1),
            // Esc, outside the bucket, gives the bucket the selection — on
            // Surface, where the bucket is.
            KeyCode::Esc if on_work => return self.focus_input(),
            KeyCode::Esc => return,
            _ => {}
        }
        // The Config tab: open, back, flip, clear, nudge, explain. Moving is
        // the tab's own, below.
        if self.ctab == CTab::Config && !alt && !ctrl && self.settings_key(code) {
            return;
        }
        // The Status tab: bands open and close.
        if self.ctab == CTab::Status && !alt && !ctrl {
            if matches!(code, KeyCode::Right | KeyCode::Left) && self.ledger_turn(code == KeyCode::Right) {
                return;
            }
            match code {
                KeyCode::Right | KeyCode::Enter => {
                    self.status_open();
                    return;
                }
                KeyCode::Left => {
                    self.status_back();
                    return;
                }
                KeyCode::Char(' ') if self.status_flip() => return,
                _ => {}
            }
        }
        // `+` in the sidebar: on the header's `+`, a new ROOT cluster; on a
        // cluster, a new cluster UNDER it (msg 112).
        if on_work && self.focus == Focus::Tree && matches!(code, KeyCode::Char('+')) {
            let parent = (!self.on_add_root && !self.on_below).then_some(self.selected);
            return self.open_new_cluster(parent);
        }
        // `c` and `o` on an artifact: copy its path, open it.
        if on_work && self.focus == Focus::Content && matches!(self.ktab, KTab::Nodes | KTab::Questions | KTab::Actions) && bare {
            if let (KeyCode::Char(k @ ('c' | 'o')), Some(path)) = (code, self.artifact_path()) {
                return if k == 'c' { self.copy_path(path) } else { self.open_path(path) };
            }
        }
        match (code, self.focus, on_work) {
            // ── the sidebar ──
            (KeyCode::Up, Focus::Tree, true) if alt => self.tree_section(-1),
            (KeyCode::Down, Focus::Tree, true) if alt => self.tree_section(1),
            (KeyCode::Up, Focus::Tree, true) => {
                self.tree_move(-1);
            }
            // Past the last cluster, ↓ crosses into the dump below.
            (KeyCode::Down, Focus::Tree, true) => {
                if !self.tree_move(1) {
                    self.focus_input();
                }
            }
            (KeyCode::Right | KeyCode::Enter, Focus::Tree, true) => self.tree_right(),
            // Space flips the switch at the sidebar's foot.
            (KeyCode::Char(' '), Focus::Tree, true) if self.on_below => self.toggle_aggregate(),
            (KeyCode::Left, Focus::Tree, true) => self.tree_left(),
            (KeyCode::Home, Focus::Tree, true) => self.tree_home(false),
            (KeyCode::End, Focus::Tree, true) => self.tree_home(true),
            // ── the cluster page ──
            (KeyCode::Up, Focus::Content, true) if alt => self.item_section(-1),
            (KeyCode::Down, Focus::Content, true) if alt => self.item_section(1),
            (KeyCode::Up, Focus::Content, true) => {
                self.item_move(-1);
            }
            (KeyCode::Down, Focus::Content, true) => {
                if !self.item_move(1) {
                    self.focus_input();
                }
            }
            (KeyCode::Right | KeyCode::Enter, Focus::Content, true) => {
                self.open_selected();
            }
            // ← closes one level, and at the top of the page goes back to the
            // sidebar it was entered from.
            (KeyCode::Left, Focus::Content, true) => {
                if !self.close_level() {
                    self.focus = Focus::Tree;
                }
            }
            (KeyCode::Home, Focus::Content, true) => {
                self.item_move(isize::MIN / 2);
            }
            (KeyCode::End, Focus::Content, true) => {
                self.item_move(isize::MAX / 2);
            }
            // ── the other tabs ──
            (KeyCode::Up, _, false) if alt => self.row_section(-1),
            (KeyCode::Down, _, false) if alt => self.row_section(1),
            (KeyCode::Up, _, false) => {
                self.row_move(-1);
            }
            // The dump is not on these tabs: ↓ stops at the last row.
            (KeyCode::Down, _, false) => {
                self.row_move(1);
            }
            (KeyCode::Home, _, false) => {
                self.row_move(isize::MIN / 2);
            }
            (KeyCode::End, _, false) => {
                self.row_move(isize::MAX / 2);
            }
            _ => {}
        }
    }

    /// Keys while a setting's value is being typed. Like every field: the
    /// letters are text, Backspace edits, ^U clears; Enter keeps it (and stays
    /// open on a refusal); Esc, ←, ↑ and ↓ leave it KEEPING what was typed.
    pub(crate) fn field_key(&mut self, key: KeyEvent, ctrl: bool) {
        let Some(f) = self.field.as_mut() else { return };
        match key.code {
            KeyCode::Enter => self.commit_field(false),
            KeyCode::Esc | KeyCode::Left => self.commit_field(true),
            KeyCode::Up | KeyCode::Down => {
                self.commit_field(true);
                self.key(key);
            }
            KeyCode::Backspace => {
                f.text.pop();
                f.error = None;
            }
            KeyCode::Char('u') if ctrl => f.text.clear(),
            KeyCode::Char(c) if !ctrl => {
                f.text.push(c);
                f.error = None;
            }
            _ => {}
        }
    }

    /// Keys in the dump — the textarea's grammar, the harness composer's
    /// (`docs/navigation.md` rule 5).
    fn input_key(&mut self, key: KeyEvent, ctrl: bool, alt: bool) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let w = self.input_w.max(1);
        let before = self.composer.text();
        let c = &mut self.composer;
        match key.code {
            KeyCode::Enter if alt || shift => c.newline(),
            KeyCode::Char('j') if ctrl => c.newline(),
            KeyCode::Enter => return self.submit(),
            // Esc hands the selection back to the page (0138 Q2).
            KeyCode::Esc if crate::cauldron::designs::ESC_LEAVES_DUMP => return self.leave_input(),
            // ^S sends the caret's note alone (0138 Q3).
            KeyCode::Char('s') if ctrl && crate::cauldron::designs::SEND_ONE_NOTE => return self.send_note(),
            // ^O opens the chip at the caret.
            KeyCode::Char('o') if ctrl => return self.open_chip_at_caret(),
            // A chip (or a reference) is ONE unit to the caret (0138 §4.2).
            KeyCode::Left if !ctrl && !alt && crate::cauldron::dump::tag_at(c, true).is_some_and(|(r, _)| r.end == crate::cauldron::dump::caret_offset(c)) => {
                let (r, _) = crate::cauldron::dump::tag_at(c, true).expect("checked");
                crate::cauldron::dump::set_caret(c, r.start);
            }
            KeyCode::Right if !ctrl && !alt && crate::cauldron::dump::tag_at(c, false).is_some_and(|(r, _)| r.start == crate::cauldron::dump::caret_offset(c)) => {
                let (r, _) = crate::cauldron::dump::tag_at(c, false).expect("checked");
                crate::cauldron::dump::set_caret(c, r.end);
            }
            // ← at the very start of the draft leaves the input.
            KeyCode::Left if !ctrl && !alt && c.cursor().line == 0 && c.cursor().byte == 0 => {
                return self.leave_input()
            }
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
            // ↑ on the top line leaves the input for the pane above.
            KeyCode::Up => {
                if !c.up(w) {
                    return self.leave_input();
                }
            }
            KeyCode::Down => {
                c.down(w);
            }
            KeyCode::Home if alt => c.buffer_start(),
            KeyCode::End if alt => c.buffer_end(),
            KeyCode::Home => c.row_start(w),
            KeyCode::End => c.row_end(w),
            KeyCode::Char('a') if ctrl => c.row_start(w),
            KeyCode::Char('e') if ctrl => c.row_end(w),
            KeyCode::Backspace if alt => c.kill_word_back(),
            // A reference — and a chip — goes WHOLE (msg 165: "it should
            // delete the entire reference. It should not just delete the last
            // square bracket"): Backspace right after or inside one, Delete
            // right before or inside one.
            KeyCode::Backspace | KeyCode::Char('h') if key.code == KeyCode::Backspace || ctrl => match crate::cauldron::dump::tag_at(c, true) {
                Some((r, _)) => crate::cauldron::dump::replace_range(c, r, ""),
                None => c.backspace(),
            },
            KeyCode::Delete | KeyCode::Char('d') if key.code == KeyCode::Delete || ctrl => match crate::cauldron::dump::tag_at(c, false) {
                Some((r, _)) => crate::cauldron::dump::replace_range(c, r, ""),
                None => c.delete(),
            },
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
            // ^P / ^N move the page one row, as they move the transcript.
            KeyCode::Char('p') if ctrl => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Char('n') if ctrl => self.scroll += 1,
            KeyCode::Char('/' | '7' | '_') if ctrl => return self.open_palette(),
            KeyCode::Char('f') if ctrl => return self.start_search(),
            KeyCode::Char('g') if ctrl => return self.toggle_aggregate(),
            // A hard tab is never stored.
            KeyCode::Tab => c.insert_paste("    "),
            // ⌥ with a letter is a chord, never the letter (iTerm's default
            // Option mode sends a character of its own instead, `å`).
            KeyCode::Char(ch) if !ctrl && !alt && !key.modifiers.contains(KeyModifiers::SUPER) => c.insert_char(ch),
            _ => {}
        }
        if self.composer.text() != before {
            // He edited the bucket himself: the reference he went by is his
            // to replace again, as msg 23 says (0258 J7).
            self.went_by = None;
            self.save_draft();
        }
    }
}

/// Whether a trace entry is a PAGE of its own (a node's trace) rather than
/// an expansion in place on the page below it.
pub fn is_trace_root(it: Item) -> bool {
    matches!(it, Item::Task { .. } | Item::Milestone { .. } | Item::Objective { .. } | Item::Directive { .. } | Item::Ha { .. } | Item::Log)
}

/// Resolve the environment for `--env` (or the process environment when it
/// is absent), exactly as every `cauldron` verb would.
pub fn resolve_env(flag: Option<&str>) -> Result<Environment> {
    let base = Environment::resolve().context("resolving the cauldron environment")?;
    match flag {
        Some(name) => base.for_name(name),
        None => Ok(base),
    }
}

#[cfg(test)]
mod seed_orphan_tests {
    //! IS-32 in the face: a seeding whose face died shows `interrupted`, its
    //! cluster offers `reseed` and `clear seeding`, and a live seeding in
    //! another face is never touched.
    use super::*;
    use crate::cauldron::store::{SeedLock, SeedState, SeededBy};

    fn face(root: &Path) -> App {
        let env = Environment {
            root: root.to_path_buf(),
            root_source: env::paths::Source::CauldronHome,
            name: "prod".into(),
            name_source: env::paths::Source::Default,
            data_root: root.join("data").join("prod"),
        };
        let mut a = App::open(env, store::seed_dir(), true).expect("seed loads");
        // The stand-in answers: no type-one child is ever spawned here.
        let i = a.rstore.live.index(crate::cauldron::seeding::SEED_RIPPLE).unwrap();
        a.rstore.live.file.ripple[i].model.model = None;
        a.rstore.live.file.ripple[i].model.trusted = Some("none".into());
        a
    }

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("seed-orphan-face-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Face A's half: the cluster written pending, as `create_cluster`
    /// writes it, with A's lock (dropping the lock is A dying).
    fn face_a_seeds(b: &App, id: &str, boundaries: &str) -> SeedLock {
        let dir = store::soup_dir(&b.env).unwrap();
        let lock = SeedLock::take(&dir, id, "r-a").unwrap().unwrap();
        CreatedClusters::new(&dir)
            .add(
                CreatedCluster {
                    id: id.into(),
                    name: "Orphan".into(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    parent: None,
                    boundaries: boundaries.into(),
                    regions: Vec::new(),
                    repos: Vec::new(),
                    links: Vec::new(),
                    seeded_by: Some(SeededBy { run: "r-a".into(), line: "seeding · asking jev over 9 candidates".into(), at: "12:00".into(), pending: true }),
                    unowned: false,
                },
                |_| false,
            )
            .unwrap();
        lock
    }

    fn seed_row(a: &App, id: &str) -> crate::cauldron::model::Background {
        let g = a.forest.find(id).expect("loaded");
        a.forest.get(g).background.iter().find(|b| b.kind == "seed").cloned().expect("a seed row")
    }

    fn manage(a: &mut App, id: &str) -> Vec<u8> {
        let g = a.forest.find(id).unwrap();
        a.select(g);
        a.set_ktab(KTab::Details);
        a.items().into_iter().filter_map(|i| if let Item::Manage(k) = i { Some(k) } else { None }).collect()
    }

    #[test]
    fn a_dead_faces_seeding_is_interrupted_after_its_one_second_look_and_reseeding_lands_claims() {
        let root = scratch();
        let mut b = face(&root);
        // Boundaries naming a real region, so the stand-in claims it.
        let (region_id, text) = (b.corpus.regions[0].id.clone(), format!("{} {}", b.corpus.regions[0].handle, b.corpus.regions[0].boundary));
        let lock = face_a_seeds(&b, "X-90", &text);
        // The file watch takes in A's write (and nothing after it changes
        // the file).
        b.sync();
        assert_eq!(seed_row(&b, "X-90").state, "running", "face A is alive");
        assert_eq!(manage(&mut b, "X-90"), vec![0, 1, 2], "nothing to recover while A seeds");
        drop(lock); // face A dies
        // clusters.json did not change: only the look notices, once its one
        // second has passed.
        b.orphans_looked = std::time::Instant::now();
        b.sync();
        assert_eq!(seed_row(&b, "X-90").state, "running", "the look runs at most once a second");
        b.orphans_looked = std::time::Instant::now() - std::time::Duration::from_secs(2);
        b.sync();
        let row = seed_row(&b, "X-90");
        assert_eq!(row.state, "interrupted", "{row:?}");
        assert!(row.line.contains("is gone"), "{}", row.line);
        assert_eq!(manage(&mut b, "X-90"), vec![0, 1, 2, 3, 4], "reseed and clear are offered");
        // `reseed`, pressed as the button is.
        b.item = b.items().iter().position(|i| *i == Item::Manage(3)).unwrap();
        b.open_selected();
        assert!(b.notice.as_deref().unwrap_or("").starts_with("reseeding Orphan"), "{:?} / {:?}", b.notice, b.error);
        let dir = store::soup_dir(&b.env).unwrap();
        let now = CreatedClusters::new(&dir).list().unwrap().into_iter().find(|c| c.id == "X-90").unwrap();
        let by = now.seeded_by.clone().unwrap();
        assert!(by.pending && by.run != "r-a", "a new run: {by:?}");
        assert!(by.line.starts_with("seeding · asking local matcher over "), "the same line as a create: {}", by.line);
        assert!(b.wait_seeding(std::time::Duration::from_secs(20)), "the reseed landed");
        let row = seed_row(&b, "X-90");
        assert_eq!(row.state, "done", "{row:?}");
        let g = b.forest.find("X-90").unwrap();
        let claimed: Vec<&str> = b.corpus.claims[g].iter().map(|&r| b.corpus.regions[r].id.as_str()).collect();
        assert!(claimed.contains(&region_id.as_str()), "claims landed: {claimed:?}");
        assert_eq!(manage(&mut b, "X-90"), vec![0, 1, 2]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_live_seeding_in_another_face_is_never_touched() {
        let root = scratch();
        let mut b = face(&root);
        let lock = face_a_seeds(&b, "X-91", "dreaming");
        b.reload_soup().unwrap();
        let path = CreatedClusters::new(&store::soup_dir(&b.env).unwrap()).path;
        let before = std::fs::read(&path).unwrap();
        b.reseed_cluster("X-91");
        assert!(b.error.as_deref().unwrap_or("").contains("live in another face"), "{:?}", b.error);
        b.error = None;
        b.clear_seed("X-91");
        assert!(b.error.as_deref().unwrap_or("").contains("live in another face"), "{:?}", b.error);
        assert_eq!(std::fs::read(&path).unwrap(), before, "the file is untouched");
        assert!(b.seeding.is_empty(), "nothing was started");
        drop(lock);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clear_seeding_drops_the_mark_and_says_so() {
        let root = scratch();
        let mut b = face(&root);
        drop(face_a_seeds(&b, "X-92", "dreaming"));
        b.reload_soup().unwrap();
        assert_eq!(seed_row(&b, "X-92").state, "interrupted");
        b.item = { manage(&mut b, "X-92"); b.items().iter().position(|i| *i == Item::Manage(4)).unwrap() };
        b.open_selected();
        assert!(b.notice.as_deref().unwrap_or("").starts_with("cleared Orphan's interrupted seeding"), "{:?} / {:?}", b.notice, b.error);
        let dir = store::soup_dir(&b.env).unwrap();
        let c = CreatedClusters::new(&dir).list().unwrap().into_iter().find(|c| c.id == "X-92").unwrap();
        assert_eq!(c.seed_state(&dir, chrono::Utc::now()), SeedState::None);
        let g = b.forest.find("X-92").unwrap();
        assert!(b.forest.get(g).background.iter().all(|r| r.kind != "seed"), "no seed row is left");
        // A second clear has nothing to clear, and says so.
        b.clear_seed("X-92");
        assert!(b.error.as_deref().unwrap_or("").contains("not interrupted"), "{:?}", b.error);
        let _ = std::fs::remove_dir_all(&root);
    }
}
