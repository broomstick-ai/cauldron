//! **The cauldron's Status tab** (8e4fca10 msg 165; design 0142, option A):
//! what is RUNNING, PARKED and PENDING, the devices, the services, and the
//! token sources — every one a collapsible band whose header carries its
//! metadata, so the tab reads collapsed and opens where he wants detail.
//!
//! The three words, and design 0142's one test — "if nothing changes but
//! the clock and the work ahead of it, will it start?":
//!
//! - **running** ▶ — a run alive on one account;
//! - **parked** ◆ — a run the admission gate REFUSED, with a cause; it starts
//!   only on an event (a reset, a lend, a toggle, a fix);
//! - **pending** ◇ — a run that is wanted and that nothing refused: it waits
//!   its turn and starts by itself.
//!
//! Work the graph holds (■ Q-2, ← T-3) is none of the three
//! (`designs::PARKED_IS_GATE_REFUSED_ONLY`): it is on a cluster's engine.
//!
//! **A count goes only where it partitions** (0142 "the rule"). Running is
//! counted per account, per token source and for the cauldron, and they
//! nest. Parked is ONE cauldron number, broken down by cause, listed in the
//! PARKED band; a parked row carries the sources it could run on as a LABEL
//! (`designs::PARKED_ROW_CARRIES_ELIGIBLE`), never a per-source number.
//! Pending is a cauldron number and a cluster number. Every count here and
//! on every cluster's Status tab is a reading of ONE list, [`census`], so a
//! cluster's three numbers summed over the clusters, plus the background
//! runs no cluster owns, ARE the cauldron's strip.
//!
//! Its sub-tabs (8e4fca10 msg 235: "you've got devices, which we can shorten
//! to just be the mesh. And then we have services are actually running on
//! the mesh. So it should be organized. We should see which services are
//! running on each device in the mesh. Then we have tokens ... Let's show
//! soup ... Or let's merge events and triggers into event bus"): MESH — every
//! device a band, the services running on it bands inside it; TOKENS — one
//! band per token source, no band around them; SOUP —
//! what no cluster claims; BUS — the event bus: EVENTS by type, TRIGGERS,
//! RIPPLES and EXECUTIONS (what is running, parked and pending, and the runs
//! recorded), each row opening its page in the bus modal (`bus`). A
//! service's band header says its status, uptime, and how many warnings and
//! errors were seen; inside it is a way to COPY THE LINK to its log, never
//! the log itself ("I don't like looking at logs"). The strip carries no
//! metadata (msg 235: "Don't have any metadata in the tab header").
//!
//! LEDGER, first (8e4fca10 msg 457: "let's make it the first tab inside of
//! status ... It'll have the entire history of execution"): every dump a
//! band, newest first, and inside it the ripples it ran, nested, drawn as
//! the Tasks tab draws its graph (`ledger`).
//!
//! The keys are the bands' (`docs/navigation.md` rule 12): → (or Enter)
//! opens a band, and on an open one steps to its first row; ← closes it, or
//! climbs to the band above; Space on an account flips it. Like every page
//! here, [`view`] lays out lines and ITEMS and the keys act on exactly those
//! items.

use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tui_render::palette;

use crate::cauldron::app::App;
use crate::cauldron::designs;
use crate::cauldron::look::{self, Card, Tone};
use crate::cauldron::menu::{pool_label, CLAUDE_CODE};
use crate::cauldron::model::{Parked, RippleState, ClusterId, TaskState};
use crate::cauldron::tags::Tag;
use crate::cauldron::view::{crop, display_width, fit, plural};

/// The Status tab's sub-tabs (msg 186 made them; msg 235 re-cut them):
/// Ledger · Tokens · Mesh · Soup · Bus — Tokens was first (b964d571 msg 23:
/// "make Tokens the first Status sub-tab") until the Ledger went before it
/// (8e4fca10 msg 457: "So it'll be in status. It'll be the first tab").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTab {
    Ledger,
    Tokens,
    Mesh,
    Soup,
    Bus,
}

impl StatusTab {
    pub const ALL: [StatusTab; 5] = [StatusTab::Ledger, StatusTab::Tokens, StatusTab::Mesh, StatusTab::Soup, StatusTab::Bus];
    pub fn label(self) -> &'static str {
        match self {
            StatusTab::Ledger => "Ledger",
            StatusTab::Mesh => "Mesh",
            StatusTab::Tokens => "Tokens",
            StatusTab::Soup => "Soup",
            StatusTab::Bus => "Bus",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.label().eq_ignore_ascii_case(s))
    }
}

/// What a row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SAct {
    /// The sub-tab strip: → and ← step the tabs, a click picks one.
    Strip,
    /// A band's header: → opens it.
    Band,
    /// Copy this text (a log's path) to the clipboard.
    Copy(String),
    /// Nothing to copy: said on the row, once pressed.
    NoLog(String),
    /// Open an account's Config.
    Config { pool: String, account: String },
    /// Open an inference endpoint's Config: a global one's on Providers, one
    /// of this cauldron's own on its tab.
    Endpoint { here: bool, id: String },
    /// A task's run: → opens it on its cluster's Status tab.
    Task { cluster: ClusterId, serial: u32 },
    /// A page of the bus: an event, a trigger, a ripple, an execution.
    Bus(crate::cauldron::bus::BusPage),
    /// `show more` under an event type (its class), or under the recorded
    /// executions (`runs`): one more page of rows.
    More(String),
    /// A service's control on Mesh (msg 403): `restart`, `stop` or
    /// `rollback`, asked first in the confirm modal.
    Service { device: String, service: String, op: crate::cauldron::supervise::Op },
    /// One ripple's execution on the Ledger: Enter or a click explores it
    /// (the bus modal's `Exec` page); → and ← open and shut what it ran.
    Exec { seq: u64, path: Vec<usize>, inside: usize },
    /// A row that only shows (a device, a background run with no page).
    Show,
}

/// One selectable row.
#[derive(Debug, Clone)]
pub struct SItem {
    pub range: Range<usize>,
    /// Its identity: a band's key is what is kept open.
    pub key: String,
    /// The band it sits in.
    pub parent: Option<String>,
    pub act: SAct,
    pub tag: Option<Tag>,
}

pub struct SView {
    pub lines: Vec<Line<'static>>,
    pub items: Vec<SItem>,
    /// Items that begin a section (⌥↑ / ⌥↓): the top-level bands.
    pub heads: Vec<usize>,
    /// Click hotspots inside rows: (line, column, width, what a click does).
    pub hotspots: Vec<(usize, usize, usize, crate::cauldron::app::Hit)>,
}

// ── the census ─────────────────────────────────────────────────────────────

/// What a run in the census is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    /// The run carrying task `serial` of its cluster.
    Task(u32),
    /// A ripple's run (its id; it opens in the bus modal).
    Run(String),
    /// A run of the cauldron's own on no cluster (`dash.status.background`).
    Background(usize),
}

/// One run: where it is in the three words, the cluster it belongs to (none:
/// the cauldron's own), what it is, and what it is placed on or why not.
#[derive(Debug, Clone)]
pub struct Entry {
    pub state: RippleState,
    pub cluster: Option<ClusterId>,
    pub what: What,
    /// What it is about, in a few words.
    pub title: String,
    /// The account a RUNNING run is on, as the work records it.
    pub account: Option<String>,
    /// Why a PARKED run was refused.
    pub parked: Option<Parked>,
    /// `2/3` when a pending run is a retry.
    pub retry: Option<String>,
}

/// EVERY run in the cauldron that is running, parked or pending — every
/// cluster's tasks, every ripple run, and the background runs no cluster
/// owns. The cauldron's strip counts all of it; a cluster's Status tab counts
/// the entries whose cluster is in its scope. Nothing else counts runs.
pub fn census(app: &App) -> Vec<Entry> {
    let f = &app.forest;
    let mut out = Vec::new();
    for (gi, g) in f.clusters.iter().enumerate() {
        let mut tasks: Vec<_> = g.tasks.iter().collect();
        tasks.sort_by_key(|t| t.serial);
        for t in tasks {
            let state = g.task_ripple(t);
            if !matches!(state, RippleState::Running | RippleState::Parked | RippleState::Pending) {
                continue;
            }
            out.push(Entry {
                state,
                cluster: Some(gi),
                what: What::Task(t.serial),
                title: t.title.clone(),
                account: t.account.clone().filter(|_| t.state == TaskState::Active),
                parked: t.parked.clone().filter(|_| state == RippleState::Parked),
                retry: t.retry.clone().filter(|_| state == RippleState::Pending),
            });
        }
    }
    for (_, r) in app.rstore.indexed_runs() {
        let state = match r.state.as_str() {
            "running" => RippleState::Running,
            "parked" => RippleState::Parked,
            _ => continue,
        };
        out.push(Entry {
            state,
            cluster: f.find(&r.cluster),
            what: What::Run(r.id.clone()),
            title: format!("{} · {}", r.ripple, r.line),
            account: Some(r.account.clone()).filter(|a| !a.is_empty() && state == RippleState::Running),
            parked: None,
            retry: None,
        });
    }
    for (i, b) in app.dash.status.background.iter().enumerate() {
        let state = match b.state.as_str() {
            "parked" => RippleState::Parked,
            "pending" => RippleState::Pending,
            "running" => RippleState::Running,
            _ => continue,
        };
        out.push(Entry { state, cluster: None, what: What::Background(i), title: b.line.clone(), account: None, parked: b.parked.clone(), retry: None });
    }
    out
}

/// `(running, parked, pending)` over the entries whose cluster `inside`
/// accepts (`None`: the cauldron's own).
pub fn counts_where(app: &App, inside: impl Fn(Option<ClusterId>) -> bool) -> (usize, usize, usize) {
    let c = census(app);
    let n = |s: RippleState| c.iter().filter(|e| e.state == s && inside(e.cluster)).count();
    (n(RippleState::Running), n(RippleState::Parked), n(RippleState::Pending))
}

/// The cauldron's `(running, parked, pending)`.
pub fn counts(app: &App) -> (usize, usize, usize) {
    counts_where(app, |_| true)
}

/// A cluster's `(running, parked, pending)`: its own, or with `aggregate`
/// its subtree's.
pub fn cluster_counts(app: &App, id: ClusterId, aggregate: bool) -> (usize, usize, usize) {
    let scope = if aggregate { app.forest.subtree(id) } else { vec![id] };
    counts_where(app, |s| s.is_some_and(|s| scope.contains(&s)))
}

/// The token source an account (as work records it) belongs to.
fn source_of(app: &App, account: &str) -> Option<String> {
    app.pool_ids().into_iter().find(|p| app.accounts(p).iter().any(|a| a.id == account || a.label == account))
}

/// Running runs on token source `pool` — on one account of it, if given.
fn running_on(app: &App, pool: &str, account: Option<&str>) -> usize {
    census(app)
        .iter()
        .filter(|e| e.state == RippleState::Running)
        .filter_map(|e| e.account.as_deref())
        .filter(|a| source_of(app, a).as_deref() == Some(pool))
        .filter(|a| account.is_none_or(|x| app.accounts(pool).iter().any(|v| v.id == x && (v.id == *a || v.label == *a))))
        .count()
}

/// Running runs on inference endpoint `id` (named `name`): a run records
/// the endpoint it is on as it records an account, by name or id.
fn running_on_endpoint(app: &App, id: &str, name: &str) -> usize {
    census(app).iter().filter(|e| e.state == RippleState::Running).filter_map(|e| e.account.as_deref()).filter(|a| *a == id || *a == name).count()
}

/// Whether the daemon has a pool type for token source `pool`. Codex has
/// none yet (0142 unknowns: `pool.rs` has one variant): its band is greyed,
/// closed and empty.
pub fn source_built(pool: &str) -> bool {
    pool != "codex"
}

/// Whether the threads daemon answers: when it does not, running and parked
/// are UNKNOWN (`—`, never 0) and pending, the cauldron's own, still shows.
pub fn threads_live(app: &App) -> bool {
    app.dash.threads.link == "live"
}

// ── the bands ─────────────────────────────────────────────────────────────

/// Whether band `key` is open: open at first are each DEVICE, each token
/// source that is built, EVENTS, EXECUTIONS and RUNNING, the unclaimed
/// regions and repos; PARKED while anything is parked, the SUPERVISION log
/// once it holds a record; PENDING, RECORDED, a
/// service and an account when asked (0142 recommendation 2). He toggles
/// from the default; the toggles are kept or reset as `ui.preserve.status`
/// says.
pub fn is_open(app: &App, key: &str) -> bool {
    let default = match key {
        "events" | "executions" | "running" | "unregions" | "unrepos" => true,
        "parked" => counts(app).1 > 0,
        // Open once there is a record to show, never on an empty body (0258 J6).
        "supervision" => !app.dash.supervisions.is_empty(),
        k if k.starts_with("dev:") => true,
        // The Ledger: the newest dump open, the rest shut; inside a dump,
        // every ripple shows what it ran until he shuts it.
        k if k.starts_with("ld:") => k[3..].parse::<u64>().ok() == crate::cauldron::ledger::dumps(&app.ledger, &app.sends.sends).first().map(|d| d.seq),
        k if k.starts_with("lx:") => true,
        k => k.strip_prefix("tok:").is_some_and(source_built),
    };
    default != app.stnav.contains(key)
}

struct B<'a> {
    app: &'a App,
    w: usize,
    v: SView,
}

impl B<'_> {
    fn line(&mut self, l: Line<'static>) {
        self.v.lines.push(l);
    }
    fn item(&mut self, start: usize, key: String, parent: Option<String>, act: SAct, tag: Option<Tag>) {
        if parent.is_none() && act != SAct::Strip {
            self.v.heads.push(self.v.items.len());
        }
        let end = self.v.lines.len().max(start + 1);
        self.v.items.push(SItem { range: start..end, key, parent, act, tag });
    }
    /// A band's header at `depth` — THE band line (`look::band_line`):
    /// `▾ Name` and its metadata flush right.
    fn band(&mut self, depth: usize, key: &str, parent: Option<&str>, name: Vec<Span<'static>>, meta: Vec<Span<'static>>, tag: Option<Tag>) -> bool {
        let open = is_open(self.app, key);
        let s = self.v.lines.len();
        let l = look::band_line(depth, open, name, meta, self.w);
        self.line(l);
        self.item(s, key.to_string(), parent.map(str::to_string), SAct::Band, tag);
        open
    }
    /// A band drawn as a CARD (an account, a soup node: msg 186), `depth`
    /// levels in: its top border the band's header. Returns whether it is
    /// open and the first line of its body; the caller fills the body and
    /// calls [`B::card_end`].
    #[allow(clippy::too_many_arguments)]
    fn card_band(&mut self, depth: usize, key: &str, parent: Option<&str>, name: Vec<Span<'static>>, meta: Vec<Span<'static>>, border: Style, tag: Option<Tag>) -> (bool, usize) {
        let open = is_open(self.app, key);
        let s = self.v.lines.len();
        let l = look::card_band_top(Card::Node, open, name, meta, border, self.w.saturating_sub(2 * depth));
        self.line(l);
        self.item(s, key.to_string(), parent.map(str::to_string), SAct::Band, tag);
        (open, self.v.lines.len())
    }
    /// A card that is ALWAYS OPEN (msg 235: "each one of the cards for the
    /// accounts they can just be expanded they don't ever have to be
    /// collapsed"): its top border carries a dot where a chevron would be,
    /// and nothing toggles it. Returns the first line of its body; the
    /// caller fills it and calls [`B::card_end`].
    #[allow(clippy::too_many_arguments)]
    fn card_fixed(&mut self, depth: usize, key: &str, parent: Option<&str>, name: Vec<Span<'static>>, meta: Vec<Span<'static>>, border: Style, tag: Option<Tag>) -> usize {
        let s = self.v.lines.len();
        let mut left = vec![Span::styled("• ", palette::muted())];
        left.extend(name);
        let l = look::card_top(Card::Node, left, meta, border, self.w.saturating_sub(2 * depth));
        self.line(l);
        self.item(s, key.to_string(), parent.map(str::to_string), SAct::Show, tag);
        self.v.lines.len()
    }
    /// Close the card [`B::card_band`] began: frame its body and indent it.
    fn card_end(&mut self, depth: usize, header: usize, body: usize, border: Style) {
        let cw = self.w.saturating_sub(2 * depth);
        look::card_close(Card::Node, &mut self.v.lines, body, border, cw);
        look::indent(&mut self.v.lines, header, 2 * depth);
        // The card is its band's item, whole: selected, all of it shows so.
        let end = self.v.lines.len();
        if let Some(it) = self.v.items.iter_mut().find(|i| i.range.start == header) {
            it.range.end = end;
        }
    }
    /// One selectable row of `left` and `right`.
    fn row(&mut self, key: String, parent: &str, left: Vec<Span<'static>>, right: Vec<Span<'static>>, act: SAct, tag: Option<Tag>) {
        let s = self.v.lines.len();
        let l = lr(left, right, self.w);
        self.line(l);
        self.item(s, key, Some(parent.to_string()), act, tag);
    }
}

fn muted(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), palette::muted())
}
fn ink2(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), palette::ink2())
}
fn bold(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), palette::ink().add_modifier(Modifier::BOLD))
}

/// `left`, then `right` flush right; the left crops so the right stands.
fn lr(left: Vec<Span<'static>>, right: Vec<Span<'static>>, w: usize) -> Line<'static> {
    look::lr(left, right, w)
}

/// `s` padded (or cropped) to exactly `w` columns.
fn col(s: &str, w: usize) -> String {
    let c = crop(s, w);
    let pad = w.saturating_sub(display_width(&c));
    format!("{c}{}", " ".repeat(pad))
}

fn status_style(s: &str) -> Style {
    match s {
        "running" | "online" => palette::code(),
        "failed" | "offline" => palette::red(),
        _ => palette::muted(),
    }
}

fn meter(pct: u32, cells: usize) -> String {
    let filled = ((pct.min(100) as usize) * cells).div_ceil(100);
    format!("▕{}{}▏", "█".repeat(filled), "░".repeat(cells - filled))
}

/// `▶ 9 running · ◆ 3 parked · ◇ 4 pending` — running and parked `—` when
/// the threads daemon does not answer.
pub fn three_words(running: usize, parked: usize, pending: usize, live: bool) -> Vec<Span<'static>> {
    look::three_words(running, parked, pending, live)
}

/// The Status tab at `width`: its sub-tab strip (no metadata on it, msg
/// 235), then the shown sub-tab's bands.
pub fn view(app: &App, width: usize) -> SView {
    let mut b = B { app, w: width, v: SView { lines: Vec::new(), items: Vec::new(), heads: Vec::new(), hotspots: Vec::new() } };
    let labels: Vec<String> = StatusTab::ALL.iter().map(|t| app.named(t.label())).collect();
    let shown = StatusTab::ALL.iter().position(|t| *t == app.sttab).unwrap_or(0);
    let lit = app.focus == crate::cauldron::app::Focus::Content;
    let (strip, hotspots) = look::strip(&labels, shown, lit, lit && app.rows[0] == 0, width);
    let line = b.v.lines.len();
    for (i, (x, w)) in hotspots.into_iter().enumerate() {
        b.v.hotspots.push((line, x, w, crate::cauldron::app::Hit::Strip(i as u8)));
    }
    b.line(strip);
    b.item(line, "strip".into(), None, SAct::Strip, None);
    match app.sttab {
        StatusTab::Ledger => ledger(&mut b),
        StatusTab::Mesh => mesh(&mut b),
        StatusTab::Tokens => tokens(&mut b),
        StatusTab::Soup => {
            unclaimed_regions(&mut b);
            unclaimed_repos(&mut b);
        }
        StatusTab::Bus => {
            events(&mut b);
            triggers(&mut b);
            ripples_band(&mut b);
            executions(&mut b);
        }
    }
    b.v
}

/// LEDGER (8e4fca10 msg 457): every dump a band, newest first — its number
/// and first line, and on its header its state, how many ripples ran for it,
/// when, and how long — and inside it the ripples it ran, nested to any
/// depth, drawn as the Tasks tab draws its graph (`crate::cauldron::ledger`).
fn ledger(b: &mut B) {
    use crate::cauldron::ledger;
    let app = b.app;
    let dumps = ledger::dumps(&app.ledger, &app.sends.sends);
    if dumps.is_empty() {
        b.line(Line::from(muted("  no dump yet")));
        return;
    }
    for d in &dumps {
        let key = format!("ld:{}", d.seq);
        let all = d.all();
        if !ledger_band(b, d, &key) {
            continue;
        }
        if d.ripples.is_empty() {
            let s = b.v.lines.len();
            b.line(Line::from(vec![Span::raw("    "), muted("◇ nothing has run for it yet")]));
            b.item(s, format!("lx:{}:none", d.seq), Some(key.clone()), SAct::Show, None);
            continue;
        }
        // One column for every row's start, so the times line up.
        let start_w = all.iter().map(|e| display_width(&ledger::when(ledger::started(d.at, e)))).max().unwrap_or(8);
        let n = d.ripples.len();
        for (i, e) in d.ripples.iter().enumerate() {
            ledger_rows(b, d, e, vec![i], &[i + 1 == n], &key, start_w);
        }
    }
}

/// A dump's band header, drawn as the Tasks tab draws a row (re-audit 10
/// D3): the TITLE crops, the metadata stands — its state, how many ripples
/// ran, when, how long. Where the whole of it does not fit beside `Dump N`,
/// the time shortens, then the separators; only past that does it crop.
fn ledger_band(b: &mut B, d: &crate::cauldron::ledger::Dump, key: &str) -> bool {
    use crate::cauldron::ledger;
    let open = is_open(b.app, key);
    let state = d.state();
    let (g, st) = crate::cauldron::bus::state_glyph(state);
    let all = d.all();
    let failed = all.iter().filter(|e| e.state == "failed").count();
    let parts = |short: bool| {
        let mut m = vec![Span::styled(format!("{g} {state}"), st), look::word(look::noun(all.len(), "ripple"), Tone::Plain)];
        if failed > 0 && state != "failed" {
            m.push(look::seg("✕", failed, "failed", Tone::Bad));
        }
        m.push(look::word(if short { ledger::when_short(d.at) } else { ledger::when(d.at) }, Tone::Plain));
        if !all.is_empty() {
            m.push(look::word(ledger::took(d.span_ms()), Tone::Plain));
        }
        m
    };
    let tight = |m: Vec<Span<'static>>| {
        let mut out = Vec::new();
        for (i, p) in m.into_iter().enumerate() {
            if i > 0 {
                out.push(Span::raw(" "));
            }
            out.push(p);
        }
        out
    };
    let title = format!("Dump {}", d.seq);
    // What `▾ Dump N` and the gap before the metadata leave it.
    let room = b.w.saturating_sub(2 + display_width(&title) + 2);
    let mw = |m: &[Span<'static>]| m.iter().map(|s| display_width(&s.content)).sum::<usize>();
    let meta = [look::meta(parts(false)), look::meta(parts(true)), tight(parts(true))]
        .into_iter()
        .find(|m| mw(m) <= room)
        .unwrap_or_else(|| look::fit_spans(tight(parts(true)), room));
    let left = vec![look::chevron(open), bold(title), Span::raw("  "), ink2(d.headline())];
    let s = b.v.lines.len();
    b.line(lr(left, meta, b.w));
    b.item(s, key.to_string(), None, SAct::Band, Some(Tag::thing("dump", &d.seq.to_string())));
    open
}

/// One execution's row, then — while it is open — the rows of what it ran.
/// `lasts[l]` says whether the row's ancestor at depth `l` (the row itself,
/// last) is the last of its siblings: the rails the margin draws.
fn ledger_rows(b: &mut B, d: &crate::cauldron::ledger::Dump, e: &crate::cauldron::ledger::Exec, path: Vec<usize>, lasts: &[bool], parent: &str, start_w: usize) {
    use crate::cauldron::ledger;
    let depth = path.len() - 1;
    let key = format!("lx:{}:{}", d.seq, ledger::path_key(&path));
    let open = is_open(b.app, &key);
    let mut margin = String::new();
    for l in 1..depth {
        margin.push_str(if lasts[l] { "  " } else { "│ " });
    }
    if depth > 0 {
        margin.push_str(if lasts[depth] { "╰─" } else { "├─" });
    }
    let (g, st) = crate::cauldron::bus::state_glyph(&e.state);
    let g = if e.state == "pending" { "◇" } else { g };
    let mut left = vec![Span::raw("    "), muted(margin), Span::styled(format!("{g} "), st), Span::styled(format!("{} ", ledger::type_glyph(e.kind)), palette::code()), bold(e.ripple.clone())];
    if !e.children.is_empty() && !open {
        left.push(muted(format!("  ▸ {} inside", look::noun(e.children.len(), "ripple"))));
    }
    left.push(muted(format!("  {}", e.line)));
    let right = vec![
        Span::styled(format!("{}  ", col(&e.state, 7)), status_style_exec(&e.state)),
        ink2(format!("{}  ", col(&ledger::when(ledger::started(d.at, e)), start_w))),
        ink2(format!("{:>7}", ledger::took(e.ms))),
    ];
    b.row(key.clone(), parent, left, right, SAct::Exec { seq: d.seq, path: path.clone(), inside: e.children.len() }, Some(Tag::thing("ripple", &e.ripple)));
    if !open {
        return;
    }
    let n = e.children.len();
    for (i, c) in e.children.iter().enumerate() {
        let mut p = path.clone();
        p.push(i);
        let mut l = lasts.to_vec();
        l.push(i + 1 == n);
        ledger_rows(b, d, c, p, &l, &key, start_w);
    }
}

/// An execution's state word in its colour: running green, failed red,
/// parked amber, the rest muted.
fn status_style_exec(s: &str) -> Style {
    match s {
        "parked" => palette::amber(),
        s => status_style(s),
    }
}

/// MESH (msg 235: "We should see which services are running on each device
/// in the mesh"): every device a band — its state, its load, how many of its
/// services run — and inside it a table of its readings, then each service
/// running on it, a band of its own. The threads daemon is a service of
/// this machine, and heads its services.
fn mesh(b: &mut B) {
    let app = b.app;
    let m = &app.dash.mesh;
    for (di, n) in m.iter().enumerate() {
        let key = format!("dev:{}", n.node);
        let up = n.services.iter().filter(|x| x.status == "running").count();
        let warns: u32 = n.services.iter().map(|x| x.warnings).sum();
        let errs: u32 = n.services.iter().map(|x| x.errors).sum();
        let mut meta = vec![look::seg("▶", up, &format!("of {} running", look::noun(n.services.len(), "service")), if up > 0 { Tone::Good } else { Tone::Plain })];
        if warns > 0 {
            meta.push(look::seg("⚠", warns, "warnings", Tone::Warn));
        }
        if errs > 0 {
            meta.push(look::seg("✕", errs, "errors", Tone::Bad));
        }
        let name = vec![bold(format!("{}  ", n.node)), Span::styled(format!("{}  ", n.status), status_style(&n.status)), muted(n.role.clone())];
        if !b.band(0, &key, None, name, look::meta(meta), Some(Tag::thing("device", &n.node))) {
            continue;
        }
        let rows = vec![
            ("cpu".to_string(), vec![ink2(format!("{}%", n.cpu_pct))]),
            ("memory".to_string(), vec![ink2(format!("{}%", n.mem_pct))]),
            ("load".to_string(), vec![ink2(format!("{:.2} {:.2} {:.2}", n.load[0], n.load[1], n.load[2]))]),
            ("seen".to_string(), vec![ink2(n.last_seen.clone())]),
        ];
        for l in look::table(rows, 4, b.w) {
            b.line(l);
        }
        if di == 0 {
            daemon(b, &key);
        }
        services(b, n, &key);
    }
    supervision(b);
}

/// The threads daemon: a service of this machine, a band like every other
/// (audit 0177 V3) — its link, when it last answered and its warnings in the
/// header, a table of them inside.
fn daemon(b: &mut B, device: &str) {
    let app = b.app;
    let live = threads_live(app);
    let board = &app.dash.threads;
    let link = if live { "live" } else { "offline" };
    let link_style = if live { palette::green() } else { palette::red() };
    let mut meta = vec![Span::styled(format!("● {link}"), link_style)];
    let answered = crate::cauldron::supervise::answered(board);
    if let Some(age) = &answered {
        meta.push(muted(format!("answered {age} ago")));
    }
    if board.warnings > 0 {
        meta.push(look::seg("⚠", board.warnings, &format!("warning{}", plural(board.warnings as usize)), Tone::Warn));
    }
    if b.band(1, "daemon", Some(device), vec![Span::styled("threads daemon", palette::ink())], look::meta(meta), Some(Tag::thing("service", "threads-daemon"))) {
        let rows = vec![
            ("link".to_string(), vec![Span::styled(link.to_string(), link_style)]),
            ("answered".to_string(), vec![ink2(answered.as_ref().map_or("never".to_string(), |a| format!("{a} ago")))]),
            ("warnings".to_string(), vec![Span::styled(board.warnings.to_string(), if board.warnings > 0 { palette::amber() } else { palette::ink2() })]),
        ];
        for l in look::table(rows, 6, b.w) {
            b.line(l);
        }
        let node = device.strip_prefix("dev:").unwrap_or(device).to_string();
        controls(b, &node, crate::cauldron::supervise::DAEMON, "daemon");
    }
}

/// A service's controls (8e4fca10 msg 403, Q5: "service level"):
/// `restart`, `stop` and `rollback`, a row each, each asked first in the
/// confirm modal and recorded in the supervision log as requested; the right
/// of each says what it would do, or why it cannot.
fn controls(b: &mut B, device: &str, service: &str, band: &str) {
    use crate::cauldron::supervise::{plan, stopped, Op};
    let down = stopped(&b.app.dash.mesh, &b.app.dash.threads, device, service);
    for op in Op::ALL {
        let said = match plan(&b.app.dash.mesh, &b.app.dash.threads, device, service, op) {
            Ok((from, to)) if op == Op::Rollback => Span::styled(format!("to {to}, from {from}"), palette::muted()),
            Ok(_) => muted(match op {
                Op::Restart if down => "start it again",
                Op::Restart => "stop and start it again",
                _ => "stop it until it is restarted",
            }),
            Err(why) => muted(why),
        };
        let key = format!("ctl:{device}/{service}/{}", op.word());
        b.row(key, band, vec![Span::raw("      "), Span::styled(op.word(), palette::code())], vec![said], SAct::Service { device: device.to_string(), service: service.to_string(), op }, None);
    }
}

fn services(b: &mut B, n: &crate::cauldron::dash::Node, device: &str) {
    // The services are rows of a table: name, status, uptime lined up.
    let cells: Vec<Vec<String>> = n.services.iter().map(|x| vec![x.name.clone(), x.status.clone(), crate::cauldron::supervise::uptime(x)]).collect();
    let wd = look::widths(&cells);
    for (x, c) in n.services.iter().zip(&cells) {
        let skey = format!("svc:{}/{}", n.node, x.name);
        let name = vec![Span::styled(format!("{}  ", look::cell(&c[0], wd[0])), palette::ink()), Span::styled(format!("{}  ", look::cell(&c[1], wd[1])), status_style(&x.status)), muted(c[2].clone())];
        let mut meta = Vec::new();
        if x.warnings > 0 {
            meta.push(look::seg("⚠", x.warnings, if x.warnings == 1 { "warning" } else { "warnings" }, Tone::Warn));
        }
        if x.errors > 0 {
            meta.push(look::seg("✕", x.errors, if x.errors == 1 { "error" } else { "errors" }, Tone::Bad));
        }
        let tag = Some(Tag::thing("service", &x.name.replace(' ', "-")));
        if !b.band(1, &skey, Some(device), name, look::meta(meta), tag) {
            continue;
        }
        // Opened, a TABLE of its fields (msg 186: "If I've opened up one of
        // these services ... I'm looking for a table").
        let mut rows = vec![
            ("status".to_string(), vec![Span::styled(x.status.clone(), status_style(&x.status))]),
            ("uptime".to_string(), vec![ink2(c[2].clone())]),
            ("pid".to_string(), vec![ink2(x.pid.map_or("—".to_string(), |p| p.to_string()))]),
            ("address".to_string(), vec![ink2(x.addr.clone())]),
            ("version".to_string(), vec![ink2(x.version.clone())]),
            ("warnings".to_string(), vec![Span::styled(x.warnings.to_string(), if x.warnings > 0 { palette::amber() } else { palette::ink2() })]),
            ("errors".to_string(), vec![Span::styled(x.errors.to_string(), if x.errors > 0 { palette::red() } else { palette::ink2() })]),
        ];
        // The versions proven good: a reading, so Status's — never Config's
        // (msg 235: "that would be stuff that's in status").
        let keep = b.app.sget(&crate::cauldron::menu::Bound::new("storage.pins.good_history", crate::cauldron::settings::Ctx::default())).val.as_int().unwrap_or(10).max(0) as usize;
        let proven: Vec<String> = x.proven.iter().take(keep).map(|p| format!("{} ({})", p.version, p.at)).collect();
        rows.push(("proven good".to_string(), vec![ink2(if proven.is_empty() { "none recorded".to_string() } else { proven.join(" · ") })]));
        if let Some(note) = &x.note {
            rows.push(("note".to_string(), vec![Span::styled(note.clone(), palette::amber())]));
        }
        for l in look::table(rows, 6, b.w) {
            b.line(l);
        }
        let (act, shown) = match &x.log {
            Some(p) => (SAct::Copy(p.clone()), p.replace(&std::env::var("HOME").unwrap_or_default(), "~")),
            None => (SAct::NoLog(x.name.clone()), "no log recorded".to_string()),
        };
        b.row(format!("log:{}/{}", n.node, x.name), &skey, vec![Span::raw("      "), Span::styled("copy log link", palette::code())], vec![muted(shown)], act, None);
        controls(b, &n.node, &x.name, &skey);
    }
}

/// SUPERVISION (msg 403): the supervision log — what was asked of the
/// mesh's services, newest first: when, the act, the service and its
/// device, who asked and why.
fn supervision(b: &mut B) {
    let recs = &b.app.dash.supervisions;
    let meta = look::meta([look::seg("", recs.len(), if recs.len() == 1 { "record" } else { "records" }, Tone::Plain)]);
    if !b.band(0, "supervision", None, vec![bold("Supervision")], meta, None) {
        return;
    }
    let n = shown(b.app, "supervision").min(recs.len());
    let cells: Vec<Vec<String>> = recs.iter().rev().take(n).map(|r| vec![r.at.clone(), r.op.word().to_string(), format!("{} on {}", r.service, r.device)]).collect();
    let wd = look::widths(&cells);
    for (i, (r, c)) in recs.iter().rev().take(n).zip(&cells).enumerate() {
        let mut right = format!("{} · {}", r.who, r.why);
        if r.op == crate::cauldron::supervise::Op::Rollback {
            right = format!("{} → {} · {right}", r.from, r.to);
        }
        let left = vec![Span::raw("    "), ink2(format!("{}  ", look::cell(&c[0], wd[0]))), Span::styled(format!("{}  ", look::cell(&c[1], wd[1])), palette::ink()), muted(c[2].clone())];
        b.row(format!("sup:{}", recs.len() - 1 - i), "supervision", left, vec![muted(right)], SAct::Show, None);
    }
    more(b, "supervision", "supervision", recs.len() - n, 0);
}

/// How many rows of a list Bus shows before `show more` (msg 235: "with
/// the ability to, like, show more. And I can keep clicking show more, and
/// it shows me more rows of that type until there's no more").
pub const BUS_PAGE: usize = 8;

/// The rows the bus shows of a list keyed `key`: a page, and one more per
/// `show more`.
fn shown(app: &App, key: &str) -> usize {
    BUS_PAGE * (1 + app.bus_pages.get(key).copied().unwrap_or(0))
}

/// `show more`, under a list with rows left; nothing once none are.
fn more(b: &mut B, key: &str, parent: &str, left: usize, depth: usize) {
    if left == 0 {
        return;
    }
    let s = b.v.lines.len();
    b.line(lr(vec![Span::raw("  ".repeat(depth + 1)), Span::styled("show more", palette::code())], vec![muted(format!("{left} more"))], b.w));
    b.item(s, format!("more:{key}"), Some(parent.to_string()), SAct::More(key.to_string()), None);
}

/// EVENTS (msg 235: "they need to be grouped by type. And then when I expand
/// that, I want to see one row item for every one of the events of that type
/// with a timestamp"): a band per type the bus carried, each row an event —
/// its time, its cluster, the message it carries, the handlers it woke.
fn events(b: &mut B) {
    let app = b.app;
    let all = app.rstore.events();
    let classes: Vec<String> = app.rstore.classes().into_iter().filter(|c| all.iter().any(|e| e.class == *c)).collect();
    let meta = look::meta([look::seg("", classes.len(), if classes.len() == 1 { "type" } else { "types" }, Tone::Plain), look::seg("", all.len(), if all.len() == 1 { "event" } else { "events" }, Tone::Plain)]);
    if !b.band(0, "events", None, vec![bold("Events")], meta, None) {
        return;
    }
    for c in classes {
        let evs = crate::cauldron::bus::of_class(&all, &c);
        let key = format!("ev:{c}");
        let last = evs.first().map_or(String::new(), |e| crate::cauldron::bus::when(&e.day, &e.at));
        let fired = evs.iter().filter(|e| !e.fired.is_empty()).count();
        let mut meta = vec![look::seg("", evs.len(), if evs.len() == 1 { "event" } else { "events" }, Tone::Plain)];
        if fired > 0 {
            meta.push(look::seg("", fired, "fired a trigger", Tone::Plain));
        }
        meta.push(look::word(format!("last {last}"), Tone::Plain));
        if !b.band(1, &key, Some("events"), vec![Span::styled(c.clone(), palette::ink())], look::meta(meta), None) {
            continue;
        }
        let n = shown(app, &c).min(evs.len());
        let cw = evs.iter().take(n).map(|e| display_width(&app.cluster_name(&e.cluster))).max().unwrap_or(0).clamp(4, 16);
        for e in evs.iter().take(n) {
            let handlers: Vec<String> = e.fired.iter().map(|f| f.ripple.clone()).collect();
            let right = if handlers.is_empty() { muted("no trigger") } else { Span::styled(format!("→ {}", handlers.join(" · ")), palette::ink2()) };
            let left = vec![Span::raw("    "), ink2(format!("{}  ", look::cell(&crate::cauldron::bus::when(&e.day, &e.at), 10))), Span::styled(format!("{}  ", look::cell(&app.cluster_name(&e.cluster), cw)), palette::ink()), muted(e.cite.clone())];
            b.row(format!("event:{}", e.id), &key, left, vec![right], SAct::Bus(crate::cauldron::bus::BusPage::Event(e.id.clone())), Some(Tag::thing("event", &e.id)));
        }
        more(b, &c, &key, evs.len() - n, 1);
    }
}

/// TRIGGERS, read-only (msg 235: "I know what triggers are present and I can
/// explore them, but I don't want to be able to modify them"): every
/// trigger of every ripple, a row each.
fn triggers(b: &mut B) {
    let app = b.app;
    let list: Vec<(String, usize, crate::cauldron::ripples::Trigger)> = app.rstore.live.file.ripple.iter().flat_map(|r| r.triggers.iter().enumerate().map(move |(i, t)| (r.id.clone(), i, t.clone()))).collect();
    let on = list.iter().filter(|(_, _, t)| t.enabled).count();
    let meta = look::meta([look::seg("", list.len(), if list.len() == 1 { "trigger" } else { "triggers" }, Tone::Plain), look::seg("", on, "enabled", Tone::Plain)]);
    if !b.band(0, "triggers", None, vec![bold("Triggers")], meta, None) {
        return;
    }
    let cells: Vec<Vec<String>> = list.iter().map(|(r, _, t)| vec![t.on.clone(), r.clone(), crate::cauldron::bus::condition(t)]).collect();
    let wd = look::widths(&cells);
    for ((r, i, t), c) in list.iter().zip(&cells) {
        let fires = app.rstore.stats(r).fires.get(&i.to_string()).map_or(0, Vec::len);
        let left = vec![Span::raw("    "), Span::styled(format!("{}  ", look::cell(&c[0], wd[0])), palette::ink()), bold(format!("{}  ", look::cell(&c[1], wd[1]))), muted(c[2].clone())];
        let right = if t.enabled { muted(format!("fired {fires}")) } else { muted("disabled") };
        b.row(format!("trigger:{r}/{i}"), "triggers", left, vec![right], SAct::Bus(crate::cauldron::bus::BusPage::Trigger { ripple: r.clone(), trigger: *i, event: None }), Some(Tag::thing("ripple", r)));
    }
}

/// RIPPLES — the handlers (msg 235: "the handlers are the ripples, right?
/// So we can just call them ripples"): every ripple, a row each, read-only.
fn ripples_band(b: &mut B) {
    let app = b.app;
    let rs = &app.rstore.live.file.ripple;
    let today: u32 = rs.iter().map(|r| app.rstore.stats(&r.id).today).sum();
    let meta = look::meta([look::seg("", rs.len(), if rs.len() == 1 { "ripple" } else { "ripples" }, Tone::Plain), look::seg("", today, "runs today", Tone::Plain)]);
    if !b.band(0, "ripples", None, vec![bold("Ripples")], meta, None) {
        return;
    }
    let cells: Vec<Vec<String>> = rs.iter().map(|r| vec![r.id.clone(), r.kind.label().to_string(), crate::cauldron::bus::wakes(r)]).collect();
    let wd = look::widths(&cells);
    for (r, c) in rs.iter().zip(&cells) {
        let st = app.rstore.stats(&r.id);
        let left = vec![Span::raw("    "), bold(format!("{}  ", look::cell(&c[0], wd[0]))), muted(format!("{}  ", look::cell(&c[1], wd[1]))), ink2(c[2].clone())];
        b.row(format!("ripple:{}", r.id), "ripples", left, vec![muted(format!("{} today", st.today))], SAct::Bus(crate::cauldron::bus::BusPage::Ripple(r.id.clone())), Some(Tag::thing("ripple", &r.id)));
    }
}

/// EXECUTIONS: what is running, parked and pending now (they were the
/// Ripples sub-tab, msg 186), and every run recorded, newest first.
fn executions(b: &mut B) {
    let app = b.app;
    let (running, parked, pending) = counts(app);
    if !b.band(0, "executions", None, vec![bold("Executions")], three_words(running, parked, pending, threads_live(app)), None) {
        return;
    }
    running_band(b);
    parked_band(b);
    pending_band(b);
    recent_band(b);
}

/// RECORDED: every run, newest first, a page at a time.
fn recent_band(b: &mut B) {
    let app = b.app;
    let runs: Vec<crate::cauldron::ripples::Run> = app.rstore.runs().into_iter().cloned().collect();
    let failed = runs.iter().filter(|r| r.state == "failed").count();
    let mut meta = vec![look::seg("", look::noun(runs.len(), "run"), "recorded", Tone::Plain)];
    if failed > 0 {
        meta.push(look::seg("✕", failed, "failed", Tone::Bad));
    }
    if !b.band(1, "recorded", Some("executions"), vec![bold("Recorded")], look::meta(meta), None) {
        return;
    }
    let n = shown(app, "runs").min(runs.len());
    for run in runs.iter().take(n) {
        let (mut left, right) = crate::cauldron::bus::run_row_spans(app, run);
        left.insert(0, Span::raw("    "));
        b.row(format!("run:{}", run.id), "recorded", left, right, SAct::Bus(crate::cauldron::bus::BusPage::Run(run.id.clone())), Some(Tag::thing("run", &run.id)));
    }
    more(b, "runs", "recorded", runs.len() - n, 1);
}

/// An account's one state word: what explains why work is not running on
/// it, shown once, as its state — never as a parked count (0142).
fn account_state(on: bool, card: Option<&crate::cauldron::dash::Account>) -> Option<(String, Style)> {
    if !on {
        return Some(("DISABLED".into(), palette::muted()));
    }
    let c = card?;
    if c.login == "EXPIRED" {
        return Some(("LOGIN EXPIRED".into(), palette::amber()));
    }
    if let Some(ax) = c.axes.iter().find(|a| a.pct >= 100) {
        return Some((format!("AT LIMIT · back {}", ax.resets), palette::amber()));
    }
    c.axes.first().map(|a| (format!("resets {}", a.resets), palette::muted()))
}

/// Whether an account can take work now: on, logged in, under every limit.
fn account_open(on: bool, card: Option<&crate::cauldron::dash::Account>) -> bool {
    on && card.is_none_or(|c| c.login != "EXPIRED" && c.axes.iter().all(|a| a.pct < 100))
}

fn tokens(b: &mut B) {
    let app = b.app;
    let pools = app.pool_ids();
    let live = threads_live(app);
    let n = |x: usize| if live { x.to_string() } else { "—".into() };
    // msg 235 (S3): one band per token source, at the top — no TOKENS band
    // around them (the sub-tab already says Tokens).
    let board = &app.dash.threads;
    for p in pools {
        let key = format!("tok:{p}");
        let accts = app.accounts(&p);
        let built = source_built(&p);
        let r = running_on(app, &p, None);
        let mut meta = Vec::new();
        let name_style = if built { palette::ink().add_modifier(Modifier::BOLD) } else { palette::muted() };
        if built {
            meta.push(look::seg("▶", look::noun(n(r), "run"), "", if live && r > 0 { Tone::Good } else { Tone::Plain }));
            {
                let ons: Vec<bool> = accts.iter().map(|a| app.account_on(&p, &a.id)).collect();
                let open = accts.iter().zip(&ons).filter(|(a, on)| account_open(**on, a.seed.filter(|_| p == CLAUDE_CODE).map(|i| &board.accounts[i]))).count();
                meta.push(look::word(format!("{open} of {} accounts open", accts.len()), Tone::Plain));
            }
        } else {
            // A source the daemon has no pool for runs nothing; what it
            // holds is its accounts (msg 186 V3: every band carries metadata).
            meta.push(look::seg("▶", look::noun(0, "run"), "", Tone::Plain));
            meta.push(look::seg("", accts.len(), if accts.len() == 1 { "account" } else { "accounts" }, Tone::Plain));
        }
        let tag = Some(Tag::thing("setting", &format!("pools.{p}")));
        if !b.band(0, &key, None, vec![Span::styled(pool_label(&p), name_style)], look::meta(meta), tag) {
            continue;
        }
        if !built {
            continue;
        }
        let cells = b.w.saturating_sub(56).clamp(8, 24);
        for a in accts {
            let akey = format!("acct:{p}/{}", a.id);
            let card = a.seed.filter(|_| p == CLAUDE_CODE).map(|i| &board.accounts[i]);
            let on = app.account_on(&p, &a.id);
            let r = running_on(app, &p, Some(&a.id));
            let mut meta: Vec<Span<'static>> = vec![look::seg("▶", look::noun(n(r), "run"), "", if live && r > 0 { Tone::Good } else { Tone::Plain })];
            if let Some(c) = card {
                for ax in &c.axes {
                    meta.push(look::word(format!("{} {}%", ax.axis, ax.pct), if ax.pct >= 90 { Tone::Warn } else { Tone::Plain }));
                }
            }
            // Set up globally, not taken by this cauldron (msg 186).
            let state = if on && !app.account_allowed(&p, &a.id) { Some(("NOT USED HERE".to_string(), palette::muted())) } else { account_state(on, card) };
            let warn = state.as_ref().is_some_and(|(_, st)| *st == palette::amber());
            if let Some((w, st)) = state {
                meta.push(Span::styled(w, st));
            }
            let tag = Some(Tag::thing("setting", &format!("pools.{p}.accounts.{}", a.id)));
            let name_style = if on { palette::ink().add_modifier(Modifier::BOLD) } else { palette::muted() };
            // An account is a CARD (msg 186: "the cloud code and the accounts,
            // each one of those can be a ... card ... let's just start with all
            // of them being the same"): its border its state.
            let border = if !on {
                palette::muted()
            } else if warn {
                palette::amber()
            } else if live && r > 0 {
                palette::code()
            } else {
                palette::muted()
            };
            // Two columns in from the pool's title (0211 R4).
            let body = b.card_fixed(2, &akey, Some(&key), vec![Span::styled(a.label.clone(), name_style)], look::meta(meta), border, tag);
            {
                let iw = look::inside(b.w.saturating_sub(4));
                let mut rows: Vec<(String, Vec<Span<'static>>)> = Vec::new();
                if let Some(c) = card {
                    for ax in &c.axes {
                        let tail = if ax.pct >= 100 { Span::styled(format!("   at the limit — back in {}", ax.resets), palette::amber()) } else { muted(format!("   resets in {}", ax.resets)) };
                        rows.push((ax.axis.clone(), vec![Span::styled(meter(ax.pct, cells), if ax.pct >= 90 { palette::amber() } else { palette::ink() }), Span::styled(format!(" {:>3}%", ax.pct), palette::ink()), tail]));
                    }
                    rows.push(("login".into(), vec![Span::styled(c.login.clone(), if c.login == "EXPIRED" { palette::amber() } else { palette::ink() })]));
                    if let Some(sub) = &c.subscription {
                        rows.push(("subscription".into(), vec![Span::styled(sub.clone(), palette::ink())]));
                    }
                    // msg 235 (S4): tokens left over time left, this
                    // account's alone — above 1 it is not on pace to spend
                    // its week.
                    let pressure = c.pressure();
                    rows.push(("pressure".into(), vec![Span::styled(pressure.map_or("—".to_string(), |p| format!("{p:.2}x")), if pressure.is_some_and(|p| p > 1.0) { palette::code() } else { palette::ink() })]));
                    let opp = match app.opportunistic(&p, &a) {
                        Some(true) => "flagged",
                        Some(false) => "not flagged",
                        None => "—",
                    };
                    rows.push(("opportunistic".into(), vec![Span::styled(opp.to_string(), palette::ink())]));
                }
                rows.push(("running".into(), vec![Span::styled(n(r), if live && r > 0 { palette::code() } else { palette::ink() })]));
                for l in look::table(rows, 0, iw) {
                    b.line(l);
                }
                let s = b.v.lines.len();
                b.line(Line::from(Span::styled("config ▸", palette::code())));
                b.item(s, format!("cfg:{p}/{}", a.id), Some(akey.clone()), SAct::Config { pool: p.clone(), account: a.id.clone() }, None);
            }
            b.card_end(2, body - 1, body, border);
        }
    }
    endpoints(b);
}

/// INFERENCE ENDPOINTS (design 0199 D6: what a key has spent is Status's,
/// never Config's): one always-open card per endpoint this cauldron can see
/// — what its key spent this period of its budget, what is left, when it
/// resets, and what THIS cauldron drew of it against its metered cap. FAKE
/// readings: nothing is called from this face.
fn endpoints(b: &mut B) {
    let app = b.app;
    let seen = app.seen_endpoints();
    let used = seen.iter().filter(|(h, id)| app.ep_used_here(*h, id)).count();
    let live = threads_live(app);
    let n = |x: usize| if live { x.to_string() } else { "—".into() };
    let running: usize = seen.iter().map(|(h, id)| running_on_endpoint(app, id, &app.ep_name(*h, id))).sum();
    let key = "tok:endpoints";
    let meta = vec![
        look::seg("▶", look::noun(n(running), "run"), "", if live && running > 0 { Tone::Good } else { Tone::Plain }),
        look::seg("", seen.len(), if seen.len() == 1 { "endpoint" } else { "endpoints" }, Tone::Plain),
        look::word(format!("{used} used here"), Tone::Plain),
    ];
    let name = vec![Span::styled("Inference endpoints", palette::ink().add_modifier(Modifier::BOLD))];
    if !b.band(0, key, None, name, look::meta(meta), Some(Tag::thing("setting", "endpoints"))) {
        return;
    }
    let cells = b.w.saturating_sub(56).clamp(8, 24);
    for (here, id) in seen {
        let reading = app.dash.endpoints.get(&id);
        let spent = reading.map_or(0.0, |r| r.spent);
        let budget = app.ep_val(here, &id, "budget_usd").as_int();
        let per = app.ep_per(here, &id);
        let on = app.ep_used_here(here, &id);
        let over = budget.is_some_and(|bu| spent >= bu as f64);
        let r = running_on_endpoint(app, &id, &app.ep_name(here, &id));
        let mut meta: Vec<Span<'static>> = vec![look::seg("▶", look::noun(n(r), "run"), "", if live && r > 0 { Tone::Good } else { Tone::Plain })];
        meta.push(look::word(
            match budget {
                Some(bu) => format!("${spent:.2} of ${bu}{per}"),
                None => format!("${spent:.2} spent"),
            },
            if over { Tone::Warn } else { Tone::Plain },
        ));
        if !on {
            meta.push(Span::styled(if app.ep_val(here, &id, "enabled").as_bool() { "NOT USED HERE" } else { "DISABLED" }, palette::muted()));
        }
        let border = if !on {
            palette::muted()
        } else if over {
            palette::amber()
        } else if live && r > 0 {
            palette::code()
        } else {
            palette::muted()
        };
        let name_style = if on { palette::ink().add_modifier(Modifier::BOLD) } else { palette::muted() };
        let ekey = format!("ep:{id}");
        let body = b.card_fixed(2, &ekey, Some(key), vec![Span::styled(app.ep_name(here, &id), name_style)], look::meta(meta), border, Some(Tag::thing("endpoint", &id)));
        let iw = look::inside(b.w.saturating_sub(4));
        let mut rows: Vec<(String, Vec<Span<'static>>)> = Vec::new();
        let resets = reading.map(|r| r.resets.clone()).filter(|r| !r.is_empty());
        match budget {
            Some(bu) => {
                let pct = if bu > 0 { ((spent / bu as f64) * 100.0).round() as u32 } else { 100 };
                let mut row = vec![Span::styled(meter(pct.min(100), cells), if over { palette::amber() } else { palette::ink() }), Span::styled(format!(" ${spent:.2} of ${bu}{per}"), palette::ink())];
                if let Some(r) = &resets {
                    row.push(muted(format!("   resets in {r}")));
                }
                rows.push(("spent".into(), row));
                rows.push(("left".into(), vec![Span::styled(format!("${:.2}", (bu as f64 - spent).max(0.0)), palette::ink())]));
            }
            None => rows.push(("spent".into(), vec![Span::styled(format!("${spent:.2}"), palette::ink()), muted("   budget off")])),
        }
        // What this cauldron drew of the key, against its allowance.
        let drew = reading.and_then(|r| r.by_cauldron.get(&app.env.name)).copied().unwrap_or(if here { spent } else { 0.0 });
        let cap = if here { None } else { app.sget(&crate::cauldron::menu::Bound::new("cauldron.use.{ep}.metered_cap_usd", crate::cauldron::settings::Ctx::ep(&id))).val.as_int() };
        rows.push((
            "this cauldron".into(),
            vec![Span::styled(
                match cap {
                    Some(c) => format!("${drew:.2} of ${c}{per} metered cap"),
                    None => format!("${drew:.2}"),
                },
                if cap.is_some_and(|c| drew >= c as f64) { palette::amber() } else { palette::ink() },
            )],
        ));
        rows.push(("running".into(), vec![Span::styled(n(r), if live && r > 0 { palette::code() } else { palette::ink() })]));
        for l in look::table(rows, 0, iw) {
            b.line(l);
        }
        let s = b.v.lines.len();
        b.line(Line::from(Span::styled("config ▸", palette::code())));
        b.item(s, format!("cfg:ep/{id}"), Some(ekey.clone()), SAct::Endpoint { here, id: id.clone() }, None);
        b.card_end(2, body - 1, body, border);
    }
}

/// The cluster column of a PARKED or PENDING row: its name, or the ripple
/// of a background run.
fn who(app: &App, e: &Entry) -> String {
    match (&e.what, e.cluster) {
        (What::Task(n), Some(g)) => format!("{} T-{n}", app.forest.get(g).name),
        (What::Run(id), Some(g)) => format!("{} {id}", app.forest.get(g).name),
        (What::Run(id), None) => id.clone(),
        (What::Background(i), _) => app.dash.status.background[*i].ripple.clone(),
        (What::Task(n), None) => format!("T-{n}"),
    }
}

fn entry_act(e: &Entry) -> SAct {
    match (&e.what, e.cluster) {
        (What::Task(n), Some(g)) => SAct::Task { cluster: g, serial: *n },
        (What::Run(id), _) => SAct::Bus(crate::cauldron::bus::BusPage::Run(id.clone())),
        _ => SAct::Show,
    }
}

fn entry_tag(app: &App, e: &Entry) -> Option<Tag> {
    match (&e.what, e.cluster) {
        (What::Task(n), Some(_)) => Some(Tag::item(format!("T-{n}"))),
        (What::Run(id), _) => Some(Tag::thing("run", id)),
        (What::Background(i), _) => Some(Tag::thing("ripple", &app.dash.status.background[*i].ripple)),
        _ => None,
    }
}

/// The short name of a daemon cause, for the PARKED header's breakdown.
fn cause_word(cause: &str) -> &str {
    match cause {
        "account_disabled" => "accounts off",
        "disabled" => "key off",
        "unknown_key" | "scope" => "needs config",
        "internal" => "budgets failed",
        "budget" => "cap",
        c => c,
    }
}

fn parked_band(b: &mut B) {
    let app = b.app;
    let live = threads_live(app);
    let parked: Vec<Entry> = census(app).into_iter().filter(|e| e.state == RippleState::Parked).collect();
    // ONE cauldron number, broken down by cause.
    let mut causes: Vec<(String, usize)> = Vec::new();
    for e in &parked {
        let c = e.parked.as_ref().map_or("unknown", |p| cause_word(&p.cause)).to_string();
        match causes.iter_mut().find(|(x, _)| *x == c) {
            Some((_, n)) => *n += 1,
            None => causes.push((c, 1)),
        }
    }
    let mut meta = vec![look::seg("◆", look::noun(if live { parked.len().to_string() } else { "—".into() }, "run"), "", if live && !parked.is_empty() { Tone::Warn } else { Tone::Plain })];
    if live {
        for (c, n) in &causes {
            meta.push(look::seg("", n, c, Tone::Plain));
        }
    }
    if !b.band(1, "parked", Some("executions"), vec![bold("Parked")], look::meta(meta), None) {
        return;
    }
    if !live {
        return;
    }
    let who_w = parked.iter().map(|e| display_width(&who(app, e))).max().unwrap_or(0).clamp(6, 22);
    for (i, e) in parked.iter().enumerate() {
        let p = e.parked.as_ref();
        let phrase_style = if p.is_some_and(Parked::needs_config) { palette::red() } else { palette::ink2() };
        let left = vec![Span::raw("    "), Span::styled("◆ ", palette::amber()), bold(col(&who(app, e), who_w)), Span::raw("  "), Span::styled(e.title.clone(), palette::ink())];
        let mut right = vec![Span::styled(p.map_or(String::new(), |p| p.phrase.clone()), phrase_style)];
        if designs::PARKED_ROW_CARRIES_ELIGIBLE {
            if let Some(p) = p.filter(|p| !p.eligible.is_empty()) {
                right.push(muted(format!("   {}", p.eligible.join(" · "))));
            }
        }
        right.push(muted(format!("  {:>4}", p.map_or(String::new(), |p| p.since.clone()))));
        b.row(format!("parked:{i}"), "parked", left, right, entry_act(e), entry_tag(app, e));
    }
}

fn pending_band(b: &mut B) {
    let app = b.app;
    let pending: Vec<Entry> = census(app).into_iter().filter(|e| e.state == RippleState::Pending).collect();
    if !b.band(1, "pending", Some("executions"), vec![bold("Pending")], vec![look::seg("◇", look::noun(pending.len(), "run"), "", Tone::Plain)], None) {
        return;
    }
    let who_w = pending.iter().map(|e| display_width(&who(app, e))).max().unwrap_or(0).clamp(6, 22);
    for (i, e) in pending.iter().enumerate() {
        let left = vec![Span::raw("    "), muted("◇ "), bold(col(&who(app, e), who_w)), Span::raw("  "), Span::styled(e.title.clone(), palette::ink())];
        let mut right = Vec::new();
        if let Some(r) = &e.retry {
            right.push(muted(format!("↻ retry {r}")));
        }
        // The head of the queue takes the next slot.
        if i == 0 {
            if !right.is_empty() {
                right.push(muted(" · "));
            }
            right.push(muted("next slot"));
        }
        b.row(format!("pending:{i}"), "pending", left, right, entry_act(e), entry_tag(app, e));
    }
}

/// RUNNING, inside Ripples: every run alive now, the account it is on.
fn running_band(b: &mut B) {
    let app = b.app;
    let live = threads_live(app);
    let running: Vec<Entry> = census(app).into_iter().filter(|e| e.state == RippleState::Running).collect();
    let n = if live { running.len().to_string() } else { "—".into() };
    if !b.band(1, "running", Some("executions"), vec![bold("Running")], vec![look::seg("▶", look::noun(&n, "run"), "", if live && !running.is_empty() { Tone::Good } else { Tone::Plain })], None) {
        return;
    }
    if !live {
        return;
    }
    let who_w = running.iter().map(|e| display_width(&who(app, e))).max().unwrap_or(0).clamp(6, 22);
    for (i, e) in running.iter().enumerate() {
        let left = vec![Span::raw("    "), Span::styled("▶ ", palette::code()), bold(col(&who(app, e), who_w)), Span::raw("  "), Span::styled(e.title.clone(), palette::ink())];
        let right = vec![muted(e.account.clone().unwrap_or_default())];
        b.row(format!("running:{i}"), "running", left, right, entry_act(e), entry_tag(app, e));
    }
}

/// UNCLAIMED REGIONS, on Status › Soup (msg 186, moved from Config): each a
/// band opening to its boundary and its nodes, each node a band opening to
/// its body.
fn unclaimed_regions(b: &mut B) {
    let app = b.app;
    let c = &app.corpus;
    let orphans = c.orphans();
    let nodes: usize = orphans.iter().map(|&r| c.members[r].len()).sum();
    if !b.band(0, "unregions", None, vec![bold("Unclaimed regions")], look::meta([look::seg("", orphans.len(), &format!("region{}", plural(orphans.len())), Tone::Plain), look::seg("", nodes, &format!("node{}", plural(nodes)), Tone::Plain)]), None) {
        return;
    }
    for r in orphans {
        let reg = &c.regions[r];
        let key = format!("reg:{}", reg.id);
        let n = c.members[r].len();
        let name = vec![bold(col(&reg.id, 8)), Span::styled(reg.shown_handle(), palette::ink())];
        if !b.band(1, &key, Some("unregions"), name, vec![look::seg("", n, &format!("node{}", plural(n)), Tone::Plain)], Some(Tag::thing("region", &reg.id))) {
            continue;
        }
        for row in crate::cauldron::view::wrap_plain(&reg.boundary, b.w.saturating_sub(6).max(10)) {
            b.line(fit(vec![Span::raw("      "), ink2(row)], b.w));
        }
        // Its nodes are CARDS (msg 186: "Everything in the soup will be a
        // node"): the band header is the card's top, its body inside.
        let kinds: Vec<Vec<String>> = c.members[r].iter().map(|&m| vec![c.nodes[m].id.clone(), c.nodes[m].kind.clone()]).collect();
        let wd = look::widths(&kinds);
        for &m in &c.members[r] {
            let node = &c.nodes[m];
            let nkey = format!("node:{}", node.id);
            let border = look::status_border(&node.state);
            let name = vec![Span::styled(look::cell(&node.kind, wd[1]), palette::ink2()), muted(" · "), Span::styled(node.state.clone(), border)];
            // Two columns in from the region's title, as every band's cards
            // are (0211 R4); its top as wide as its body (R5).
            let (open, body) = b.card_band(3, &nkey, Some(&key), name, vec![Span::styled(node.id.clone(), palette::ink2())], border, Some(Tag::thing("node", &node.id)));
            let iw = look::inside(b.w.saturating_sub(6));
            for row in crate::cauldron::view::wrap_plain(&node.handle, iw) {
                b.line(Line::from(Span::styled(row, palette::ink())));
            }
            if open {
                for row in crate::cauldron::view::wrap_plain(&node.body, iw) {
                    b.line(Line::from(ink2(row)));
                }
            }
            b.card_end(3, body - 1, body, border);
        }
    }
}

/// UNCLAIMED REPOS, on Status › Soup: each a band opening to its registry
/// facts and its blueprint's modules.
fn unclaimed_repos(b: &mut B) {
    let app = b.app;
    let unclaimed = app.repos.unclaimed();
    if !b.band(0, "unrepos", None, vec![bold("Unclaimed repos")], vec![look::seg("", unclaimed.len(), &format!("repo{}", plural(unclaimed.len())), Tone::Plain)], None) {
        return;
    }
    for r in unclaimed {
        let repo = &app.repos.repos[r];
        let key = format!("repo:{}", repo.name);
        let mut meta = vec![look::word(repo.at_word(), Tone::Plain), look::word(repo.bp_word(), Tone::Plain), look::seg("", repo.count("module"), "modules", Tone::Plain)];
        if let Some(pct) = repo.linked_pct() {
            meta.push(look::word(format!("{pct}% linked"), Tone::Plain));
        }
        let meta = look::meta(meta);
        let name = vec![bold(col(&repo.name, 16)), Span::styled(crate::cauldron::artpage::tilde(&repo.path), palette::code())];
        if !b.band(1, &key, Some("unrepos"), name, meta, Some(Tag::thing("repo", &repo.name))) {
            continue;
        }
        // Opened, a TABLE of its fields.
        let mut rows = vec![("branch".to_string(), vec![ink2(repo.at_word())]), ("blueprint".to_string(), vec![ink2(repo.bp_word())])];
        for w in &repo.worktrees {
            rows.push(("worktree".into(), vec![ink2(format!("{} · {} @ {}", w.name, w.branch, w.head))]));
        }
        rows.push(("adopted".into(), vec![ink2(if repo.adopted.is_empty() { "not yet".into() } else { repo.adopted.clone() })]));
        for m in repo.top_modules() {
            let n = &repo.nodes[m];
            rows.push(("module".into(), vec![bold(format!("{}  ", n.short())), ink2(n.handle.clone())]));
        }
        for l in look::table(rows, 6, b.w) {
            b.line(l);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use env::paths::Source;
    use env::Environment;

    use super::*;
    use crate::cauldron::app::{CTab, Focus};
    use crate::cauldron::ledger;

    fn app(env: &str) -> App {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!("rv10a-st-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("CAULDRON_TYPE_ONE", "off");
        let e = Environment { root: root.clone(), root_source: Source::CauldronHome, name: env.into(), name_source: Source::Default, data_root: root.join("data").join(env) };
        let mut a = App::open(e, crate::cauldron::store::seed_dir(), true).expect("opens");
        a.set_ctab(CTab::Status);
        a.set_sttab(StatusTab::Ledger);
        a.focus = Focus::Content;
        a
    }

    fn text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// What a dump's band header must carry, whatever the width: its
    /// state, its ripple count, its time (long or short) and its duration.
    fn parts(d: &ledger::Dump) -> [String; 3] {
        let st = d.state();
        let (g, _) = crate::cauldron::bus::state_glyph(st);
        [format!("{g} {st}"), look::noun(d.all().len(), "ripple"), ledger::took(d.span_ms())]
    }

    fn has_all(head: &str, d: &ledger::Dump) -> bool {
        parts(d).iter().all(|p| head.contains(p.as_str())) && (head.contains(&ledger::when(d.at)) || head.contains(&ledger::when_short(d.at)))
    }

    /// Re-audit 10 D3, on the real frame at 80 and at 50 columns: every
    /// dump's band keeps its state, count, time and duration, and it is the
    /// TITLE that crops, as Tasks crops.
    #[test]
    fn d3_band_metadata_stands_at_80_and_50_columns() {
        let mut a = app("prod");
        let ds = ledger::dumps(&a.ledger, &a.sends.sends);
        assert!(ds.len() >= 4, "the prod seed's dumps: {}", ds.len());
        for w in [80u16, 50] {
            let rows = crate::cauldron::render_to_text(&mut a, w, 40).unwrap();
            for d in &ds {
                let head = rows.iter().find(|l| l.contains(&format!("Dump {} ", d.seq)) || l.contains(&format!("Dump {}…", d.seq))).unwrap_or_else(|| panic!("{w}: no Dump {}:\n{}", d.seq, rows.join("\n")));
                assert!(has_all(head, d), "{w} columns, Dump {}: {head:?} lacks one of {:?}", d.seq, parts(d));
            }
            if w == 50 {
                let head = rows.iter().find(|l| l.contains("Dump 11")).unwrap();
                assert!(!head.contains("Let's call them dumps, not batches."), "at 50 the title crops: {head:?}");
            }
        }
    }

    /// The adversarial sweep, 30..=200 columns: never wider than the line,
    /// `Dump N` always whole, and wherever `Dump N` and the metadata CAN
    /// share the line, all of it is there.
    #[test]
    fn d3_band_headers_across_widths_30_to_200() {
        let mut a = app("prod");
        // Every dump open, the longest metadata included.
        let ds = ledger::dumps(&a.ledger, &a.sends.sends);
        for w in 30..=200usize {
            let v = view(&a, w);
            for d in &ds {
                let head = v.lines.iter().map(text).find(|l| l.contains(&format!("Dump {}", d.seq))).unwrap_or_else(|| panic!("{w}: Dump {}", d.seq));
                assert!(display_width(&head) <= w, "{w}: {head:?}");
                if w == 200 {
                    assert!(head.contains(&d.headline()), "wide, the title is whole: {head:?}");
                }
                let tight: usize = parts(d).iter().map(|p| display_width(p)).sum::<usize>() + display_width(&ledger::when_short(d.at)) + 3;
                if w >= 2 + display_width(&format!("Dump {}", d.seq)) + 2 + tight {
                    assert!(has_all(&head, d), "{w} columns: {head:?}");
                }
            }
        }
        // And with a dump made here (nothing run): no duration owed.
        a.dump("a thought with a long first line that will have to crop somewhere");
        for w in [30usize, 50, 80] {
            let head = view(&a, w).lines.iter().map(text).find(|l| l.contains("Dump 12")).unwrap();
            assert!(head.contains("◇ pending") || head.contains("✓ pending"), "{w}: {head:?}");
            assert!(head.contains("0 ripples"), "{w}: {head:?}");
        }
    }

    /// Re-audit 10 D4: the empty Ledger says the fact and nothing more.
    #[test]
    fn d4_the_empty_ledger_reads_no_dump_yet() {
        let mut a = app("prod");
        a.ledger = ledger::Seed::default();
        a.sends.sends.clear();
        let v = view(&a, 80);
        let body: Vec<String> = v.lines.iter().skip(1).map(text).collect();
        assert_eq!(body, ["  no dump yet"]);
        // Opposed: with a dump on it, the caption is gone.
        let full = app("prod");
        assert!(!view(&full, 80).lines.iter().map(text).any(|l| l.contains("no dump yet")));
    }
}
