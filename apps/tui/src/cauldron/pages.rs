//! **A cluster tab's page** (8e4fca10 msg 165; designs 0069 and 0071 before
//! it): what each cluster tab draws, row by row, and which rows are
//! selectable. The page is the ONE source of a tab's items — `App::items` is
//! this page's `ids` — so what is drawn and what the keys walk cannot drift
//! apart.
//!
//! Details (it was Overview) is always-expanded bands (scope, last dump, his
//! directives and constraints, the agents' objectives and milestones); Tasks is a box per question and per
//! human action, under collapsible status bands; Soup is the regions;
//! Artifacts the repos; Status the ripples and the engine as nested bands.
//! → on a node opens its trace page. Nothing on any tab explains the tab: no
//! legend, no label words, no empty-state sentence.
//!
//! **Scrolling never changes a row's shape** (msg 165, an invariant): no
//! line appears on a row because it is selected or visible. What a row
//! shows, it shows always; more is behind →.

use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tui_render::md;
use tui_render::palette;

use crate::cauldron::app::{is_trace_root, App, Band, Footer, HaBand, Hit, Item, DBand, QBand, RBand, RNav, KTab};
use crate::cauldron::blueprint::{short_id, working_on, RepoId};
use crate::cauldron::corpus::{NodeId, RegionId};
use crate::cauldron::engine::{self, Kind, Node};
use crate::cauldron::model::{Local, ClusterId, HaStatus, MsState, QStatus, Question, RippleState, Task, TaskState};
use crate::cauldron::look::{self, Card, Tone};
use crate::cauldron::view::{crop, display_width, plural, tagged_spans, wrap, wrap_plain};

/// A page: its rows, the row range each selectable item takes, the items
/// themselves, the band or box each item sits in (← climbs to it), which
/// items begin a section (where ⌥↑ / ⌥↓ land), and the click hotspots inside
/// rows (a path to copy, its open icon, a sub-tab).
pub struct Page {
    pub lines: Vec<Line<'static>>,
    pub items: Vec<Range<usize>>,
    pub ids: Vec<Item>,
    pub parents: Vec<Option<Item>>,
    pub heads: Vec<usize>,
    /// (line, column, width, what a click there does).
    pub hotspots: Vec<(usize, usize, usize, Hit)>,
    /// The paths a `Hit::CopyAt` / `Hit::OpenAt` hotspot names, by index.
    pub paths: Vec<String>,
    /// The Artifacts tab's rows, for the keys (`artpage::ArtRow`).
    pub arts: Vec<crate::cauldron::artpage::ArtRow>,
    /// The item the next ended item sits in.
    within: Vec<Item>,
}

impl Page {
    fn new() -> Self {
        Page { lines: Vec::new(), items: Vec::new(), ids: Vec::new(), parents: Vec::new(), heads: Vec::new(), hotspots: Vec::new(), paths: Vec::new(), arts: Vec::new(), within: Vec::new() }
    }
    pub(crate) fn begin(&self) -> usize {
        self.lines.len()
    }
    pub(crate) fn end_item(&mut self, start: usize, id: Item) {
        self.end(start, id);
    }
    fn end(&mut self, start: usize, id: Item) {
        self.items.push(start..self.lines.len());
        self.ids.push(id);
        self.parents.push(self.within.last().copied());
    }
    /// Where the next line and the next click hotspot go: [`Page::shift`]'s mark.
    fn mark(&self) -> (usize, usize) {
        (self.lines.len(), self.hotspots.len())
    }
    /// Everything drawn since `from` moves `n` columns right, its click hotspots
    /// with it: a band's cards sit two columns in from its title (V1; 0211 R4).
    fn shift(&mut self, from: (usize, usize), n: usize) {
        look::indent(&mut self.lines, from.0, n);
        for z in &mut self.hotspots[from.1..] {
            z.1 += n;
        }
    }
    /// The items ended until [`Page::leave`] sit inside `it`.
    pub(crate) fn enter(&mut self, it: Item) {
        self.within.push(it);
    }
    pub(crate) fn leave(&mut self) {
        self.within.pop();
    }
    /// The next item ended begins a section.
    pub(crate) fn head(&mut self) {
        if self.heads.last() != Some(&self.ids.len()) {
            self.heads.push(self.ids.len());
        }
    }
    fn blank(&mut self) {
        self.lines.push(Line::default());
    }
    pub(crate) fn push(&mut self, l: Line<'static>) {
        self.lines.push(l);
    }
}

// ── the shared grammar ─────────────────────────────────────────────────────

fn sw(s: &Span) -> usize {
    display_width(&s.content)
}

/// `left` and a right-aligned `right` on one row of `width`; the left crops
/// with … so the right-hand token always stands.
fn lr(left: Vec<Span<'static>>, right: Option<Span<'static>>, width: usize) -> Line<'static> {
    let rw = right.as_ref().map_or(0, |r| sw(r) + 2);
    let avail = width.saturating_sub(rw);
    let mut used = 0;
    let mut out = Vec::new();
    for s in left {
        let w = sw(&s);
        if used + w > avail {
            out.push(Span::styled(crop(&s.content, avail.saturating_sub(used)), s.style));
            used = avail;
            break;
        }
        used += w;
        out.push(s);
    }
    if let Some(r) = right {
        let pad = width.saturating_sub(used + sw(&r));
        out.push(Span::raw(" ".repeat(pad)));
        out.push(r);
    }
    Line::from(out)
}

/// A strong item: `lead` (glyph and id) then `text` wrapped at the lead's
/// width (or cropped to one row when `wrap` is false), `right` on the first
/// row.
#[allow(clippy::too_many_arguments)]
fn strong(p: &mut Page, lead: Vec<Span<'static>>, text: &str, style: Style, right: Option<Span<'static>>, width: usize, wrap_text: bool) {
    let lw: usize = lead.iter().map(sw).sum();
    let rw = right.as_ref().map_or(0, |r| sw(r) + 2);
    let tw = width.saturating_sub(lw + rw).max(1);
    let rows: Vec<Line<'static>> = if wrap_text {
        md::lay(text, tw, style, palette::code()).into_iter().map(|r| Line::from(r.spans)).collect()
    } else {
        vec![Line::from(Span::styled(crop(text, tw), style))]
    };
    for (j, row) in rows.into_iter().enumerate() {
        let mut spans = if j == 0 { lead.clone() } else { vec![Span::raw(" ".repeat(lw))] };
        spans.extend(row.spans);
        p.push(lr(spans, if j == 0 { right.clone() } else { None }, width));
    }
}

fn bold(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), palette::ink().add_modifier(Modifier::BOLD))
}

fn muted(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), palette::muted())
}

/// A muted line at `indent`.
fn sub(p: &mut Page, indent: usize, text: &str, style: Style, width: usize) {
    wrap(&mut p.lines, text, width, indent, style);
}

/// Relative age of an RFC 3339 instant or a `YYYY-MM-DD` date: `2m`, `1h`,
/// `3d`; the text itself when it is neither.
pub fn age(at: &str) -> String {
    let now = chrono::Local::now();
    let then = chrono::DateTime::parse_from_rfc3339(at).map(|t| t.with_timezone(&chrono::Local)).ok().or_else(|| {
        chrono::NaiveDate::parse_from_str(at, "%Y-%m-%d")
            .ok()
            .and_then(|d| d.and_hms_opt(12, 0, 0))
            .and_then(|d| d.and_local_timezone(chrono::Local).single())
    });
    let Some(then) = then else { return at.to_string() };
    let secs = (now - then).num_seconds().max(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

// ── traces ─────────────────────────────────────────────────────────────────

/// Where an item came from: its parents (nearest first), the operator
/// message it cites, and its regions. Every field is read off the node and
/// its edges; nothing is stored for the trace.
pub struct Trace {
    pub chain: Vec<Item>,
    pub msg: Option<String>,
    pub regions: Vec<String>,
}

fn directive_item(app: &App, id: &str) -> Option<Item> {
    let (g, _) = app.forest.directive(id)?;
    let ix = app.forest.get(g).directives.iter().position(|d| d.id == id)?;
    Some(Item::Directive { cluster: g, ix })
}

fn first_claim(app: &App, g: ClusterId) -> Vec<String> {
    app.corpus.claims.get(g).and_then(|c| c.first()).map(|&r| vec![app.corpus.regions[r].id.clone()]).unwrap_or_default()
}

pub fn trace_of(app: &App, it: Item) -> Trace {
    let f = &app.forest;
    let mut chain = Vec::new();
    let mut msg: Option<String> = None;
    let mut region: Option<String> = None;
    let mut cluster: Option<ClusterId> = None;
    let take = |m: Option<&String>, r: Option<&String>, msg: &mut Option<String>, region: &mut Option<String>| {
        if msg.is_none() {
            *msg = m.cloned();
        }
        if region.is_none() {
            *region = r.cloned();
        }
    };
    // Walk up: task → milestone → objective → directive.
    let mut objective: Option<(ClusterId, u32)> = None;
    let mut milestone: Option<(ClusterId, u32)> = None;
    match it {
        Item::Task { cluster: g, serial } | Item::Ripple { cluster: g, serial } => {
            cluster = Some(g);
            if let Some(t) = f.task(g, serial) {
                take(t.from.as_ref(), t.region.as_ref(), &mut msg, &mut region);
            }
            milestone = f.get(g).milestone_of(serial).map(|m| (g, m.serial));
        }
        Item::Milestone { cluster: g, serial } => {
            cluster = Some(g);
            milestone = Some((g, serial));
        }
        Item::Objective { cluster: g, serial } => {
            cluster = Some(g);
            objective = Some((g, serial));
        }
        Item::Directive { cluster: g, ix } => {
            let d = &f.get(g).directives[ix];
            take(Some(&d.cite), d.region.as_ref(), &mut msg, &mut region);
            cluster = Some(g);
        }
        Item::Question { cluster: g, serial } | Item::Option { cluster: g, serial, .. } => {
            cluster = Some(g);
            if let Some(q) = f.get(g).question(serial) {
                chain.extend(q.gates.as_deref().and_then(|x| local_item(g, x)));
                take(q.cite.as_ref(), q.region.as_ref(), &mut msg, &mut region);
            }
        }
        Item::Ha { cluster: g, serial } => {
            cluster = Some(g);
            if let Some(h) = f.get(g).human_action(serial) {
                chain.extend(h.raised_by.as_deref().and_then(|x| local_item(g, x)));
                take(h.cite.as_ref(), h.region.as_ref(), &mut msg, &mut region);
            }
        }
        _ => {}
    }
    if let Some((g, s)) = milestone {
        if !matches!(it, Item::Milestone { .. }) {
            chain.push(Item::Milestone { cluster: g, serial: s });
        }
        if let Some(m) = f.get(g).milestone(s) {
            take(m.from.as_ref(), m.region.as_ref(), &mut msg, &mut region);
            objective = Some((g, m.objective));
        }
    }
    if let Some((g, s)) = objective {
        if !matches!(it, Item::Objective { .. }) {
            chain.push(Item::Objective { cluster: g, serial: s });
        }
        if let Some(d) = f.get(g).objective(s).and_then(|o| directive_item(app, &o.serves)) {
            chain.push(d);
            if let Item::Directive { cluster: dg, ix } = d {
                let d = &f.get(dg).directives[ix];
                take(Some(&d.cite), d.region.as_ref(), &mut msg, &mut region);
            }
        }
    }
    let regions = match (region, cluster) {
        (Some(r), _) => vec![r],
        (None, Some(g)) => first_claim(app, g),
        _ => Vec::new(),
    };
    Trace { chain, msg, regions }
}

/// `T-4` / `Q-1` / `M-2` / `HA-1` in cluster `g` as an item.
fn local_item(g: ClusterId, s: &str) -> Option<Item> {
    use crate::cauldron::model::Local;
    Some(match Local::parse(s)? {
        Local::T(n) => Item::Task { cluster: g, serial: n },
        Local::M(n) => Item::Milestone { cluster: g, serial: n },
        Local::Q(n) => Item::Question { cluster: g, serial: n },
        Local::Ha(n) => Item::Ha { cluster: g, serial: n },
        Local::Obj(n) => Item::Objective { cluster: g, serial: n },
    })
}

/// The id an item is written as on a page.
pub fn item_id(app: &App, it: Item) -> String {
    match it {
        Item::Task { serial, .. } | Item::Ripple { serial, .. } => format!("T-{serial}"),
        Item::Milestone { serial, .. } => format!("M-{serial}"),
        Item::Objective { serial, .. } => format!("OBJ-{serial}"),
        Item::Directive { cluster, ix } => app.forest.get(cluster).directives[ix].id.clone(),
        Item::Question { serial, .. } | Item::Closed { serial, .. } => format!("Q-{serial}"),
        Item::QArt { cluster, serial, ix } => app.forest.get(cluster).question(serial).and_then(|q| q.artifacts.get(ix)).map_or(String::new(), |a| q_artifact(app, a).3.unwrap_or_default()),
        Item::Option { serial, opt, .. } => format!("Q-{serial}.opt{opt}"),
        Item::Ha { serial, .. } => format!("HA-{serial}"),
        Item::Region(r) => app.corpus.regions[r].id.clone(),
        Item::Node(n) => app.corpus.nodes[n].id.clone(),
        Item::Repo(r) => app.repos.repos[r].name.clone(),
        Item::Bp { repo, node } => short_id(&app.repos.repos[repo].nodes[node].id).to_string(),
        Item::Message(i) => app.messages[i].0.clone(),
        Item::Description(g) => app.forest.get(g).name.clone(),
        Item::Constraint { cluster, ix } => app.forest.get(cluster).constraints[ix].id.clone(),
        Item::Log => "log".into(),
        Item::Sent(seq) => format!("#{seq}"),
        Item::Run(ix) => app.rstore.run_at(ix).map_or(String::new(), |r| r.id.clone()),
        Item::HaPart { serial, .. } => format!("HA-{serial}"),
        Item::Background { .. } | Item::Footer(_) | Item::Manage(_) | Item::LastDump(_) | Item::Band(_) | Item::Art(_) => String::new(),
    }
}

fn message_index(app: &App, cite: &str) -> Option<usize> {
    app.messages.iter().position(|(k, _)| k == cite)
}

// ── cards and bands (the visual system: `look`) ───────────────────────────

/// What a card's content is laid out to.
fn inside(width: usize) -> usize {
    look::inside(width)
}

/// A NODE card's top border (`look::card_top`).
fn box_top(left: Vec<Span<'static>>, right: Option<Span<'static>>, style: Style, width: usize) -> Line<'static> {
    look::card_top(Card::Node, left, right.into_iter().collect(), style, width)
}

/// Frame the lines from `from` on as a node card's body, and close it.
fn box_close(p: &mut Page, from: usize, style: Style, width: usize) {
    look::card_close(Card::Node, &mut p.lines, from, style, width);
}

/// A band's header on a cluster's page — THE band line (`look::band_line`):
/// `▾ Active` and its metadata flush right.
fn band(app: &App, p: &mut Page, b: Band, label: &str, meta: Vec<Span<'static>>, width: usize, depth: usize) -> bool {
    let open = app.band_open(b);
    let start = p.begin();
    p.head();
    p.push(look::band_line(depth, open, vec![bold(label.to_string())], meta, width));
    p.end(start, Item::Band(b));
    open
}

/// An ALWAYS-EXPANDED band's header ([`Band::fixed`]): THE band line with a
/// dot where the chevron would be. It is still an item, so the keys stop on
/// it and its rows have a parent; nothing opens or closes it.
fn fixed_band(p: &mut Page, b: Band, label: &str, meta: Vec<Span<'static>>, width: usize) {
    let start = p.begin();
    p.head();
    p.push(look::fixed_band_line(0, vec![bold(label.to_string())], meta, width));
    p.end(start, Item::Band(b));
}

/// `n word` as a band's metadata (the word made plural when it should be).
fn count(n: usize, word: &str) -> Vec<Span<'static>> {
    vec![look::seg("", n, &format!("{word}{}", plural(n)), Tone::Plain)]
}

/// How far a band's cards sit in from its chevron: two columns in from the
/// band's title, on every band that holds cards (V1; task 0209 D7, 0211 R4).
pub const CARD_IND: usize = 4;

/// One node of Details as a CARD (msg 186: "Everything in overview is a
/// node"): its id the legend, `left` at the top left, its text inside; the
/// border its state's colour. Inside a band it is drawn at depth 2: two
/// columns in from the band's title, as every band's contents are (V1;
/// task 0209 D7).
#[allow(clippy::too_many_arguments)]
fn node_card(p: &mut Page, item: Item, id: &str, left: Vec<Span<'static>>, text: &str, style: Style, border: Style, depth: usize, width: usize) {
    let ind = 2 * depth;
    let cw = width.saturating_sub(ind);
    let start = p.begin();
    p.head();
    p.push(box_top(left, Some(Span::styled(id.to_string(), palette::ink2())), border, cw));
    let body = p.begin();
    wrap(&mut p.lines, text, inside(cw), 0, style);
    box_close(p, body, border, cw);
    look::indent(&mut p.lines, start, ind);
    p.end(start, item);
}

/// A sub-header inside a box: `Context`, `Options`, `Recommendation`.
fn part_head(p: &mut Page, title: &str) {
    p.push(Line::from(Span::styled(title.to_string(), palette::ink2().add_modifier(Modifier::BOLD))));
}

/// A paragraph inside a box, two spaces in.
fn para(p: &mut Page, text: &str, style: Style, width: usize) {
    wrap(&mut p.lines, text, inside(width), 2, style);
}

// ── the dispatcher ─────────────────────────────────────────────────────────

/// The focused cluster's current tab at `width` columns.
pub fn page(app: &App, width: usize) -> Page {
    let mut p = Page::new();
    if !app.trace.is_empty() {
        trace_page(app, &mut p, width);
        return p;
    }
    match app.ktab {
        KTab::Details => details_page(app, &mut p, width),
        KTab::Questions => questions_page(app, &mut p, width),
        KTab::Actions => ha_page(app, &mut p, width),
        KTab::Tasks => tasks_page(app, &mut p, width),
        KTab::Nodes => match app.rnav.last() {
            Some((RNav::Entry(r, n), ..)) => entry_page(app, &mut p, *r, *n, width),
            _ => crate::cauldron::artpage::page(app, &mut p, width),
        },
        KTab::Status => status_page(app, &mut p, width),
    }
    p
}

// ── Details ────────────────────────────────────────────────────────────────

/// Details (it was Overview; 8e4fca10 msg 385: "move overview to the end and
/// call it Details. And make the sections into bands without expand ability
/// and with a blank line after each section") as ALWAYS-EXPANDED BANDS, a
/// dot for the chevron and one blank row after each (msg 186: "I don't like
/// the boxes ... use the collapsible band structure"): Scope,
/// Last dump, Directives, Constraints (his), Objectives, Milestones (the
/// agents'), and last the CLUSTER itself — its minted id, and for a cluster
/// made here `rename ▸`, `move under ▸` and `delete ▸` (they were on its
/// Config tab, which msg 235 took away) — every one always open, each
/// header counting what it holds. The nodes inside are rows; their
/// card look is the visual system's.
fn details_page(app: &App, p: &mut Page, width: usize) {
    let f = &app.forest;
    let id = app.selected;
    let g = f.get(id);
    let lead = || Span::raw("    ");
    // SCOPE: what the cluster is, its boundaries.
    fixed_band(p, Band::Details(DBand::Scope), "Scope", vec![look::word("you", Tone::Plain)], width);
    {
        p.enter(Item::Band(Band::Details(DBand::Scope)));
        let start = p.begin();
        if g.whole {
            // The whole cauldron is no cluster of his: its scope is all of
            // them (0255 H3: it read `—`).
            let n = f.subtree(id).len() - 1;
            wrap(&mut p.lines, &format!("Everything in the {}: the soup, and the {n} clusters in it.", g.name), width, 4, palette::ink2());
        } else if g.description.trim().is_empty() {
            p.push(Line::from(vec![lead(), muted("—")]));
        } else {
            wrap(&mut p.lines, &g.description, width, 4, palette::ink2());
        }
        p.end(start, Item::Description(id));
        p.leave();
    }
    p.blank();
    // LAST DUMP: when he last dumped here, and how long ago.
    // The whole cauldron holds every dump, one that landed on no cluster
    // (into the soup) too (0255 H3).
    let here = |s: &&crate::cauldron::store::Send| g.whole || s.targets.contains(&g.key);
    let last = app.sends.sends.iter().rev().find(here);
    let dumps = app.sends.sends.iter().filter(here).count();
    let mut meta = vec![look::seg("", dumps, if dumps == 1 { "dump" } else { "dumps" }, Tone::Plain)];
    if let Some(s) = last {
        meta.push(look::word(format!("#{}", s.seq), Tone::Plain));
    }
    fixed_band(p, Band::Details(DBand::LastDump), "Last dump", look::meta(meta), width);
    {
        p.enter(Item::Band(Band::Details(DBand::LastDump)));
        let start = p.begin();
        match last {
            Some(s) => {
                let at = chrono::DateTime::parse_from_rfc3339(&s.at).ok().map(|t| t.with_timezone(&chrono::Local));
                let when = at.map_or_else(|| s.at.clone(), |t| t.format("%Y-%m-%d %H:%M").to_string());
                p.push(lr(vec![lead(), Span::styled(when, palette::ink()), muted(format!("  {} ago", age(&s.at)))], None, width));
            }
            None => p.push(Line::from(vec![lead(), muted("—")])),
        }
        p.end(start, Item::LastDump(id));
        p.leave();
    }
    p.blank();
    // DIRECTIVES and CONSTRAINTS: his.
    // The whole cauldron's are every cluster's (0255 H3: it read 0).
    let dirs: Vec<(ClusterId, usize)> = if g.whole { f.subtree(id).into_iter().flat_map(|c| (0..f.get(c).directives.len()).map(move |i| (c, i))).collect() } else { f.binding_directives(id) };
    let cons: Vec<(ClusterId, usize)> = if g.whole { f.subtree(id).into_iter().flat_map(|c| (0..f.get(c).constraints.len()).map(move |i| (c, i))).collect() } else { (0..g.constraints.len()).map(|i| (id, i)).collect() };
    // Every node here is a CARD (msg 186): the id its legend, its cite at
    // the top left.
    fixed_band(p, Band::Details(DBand::Directives), "Directives", look::meta([look::word(look::noun(dirs.len(), "directive"), Tone::Plain), look::word("you", Tone::Plain)]), width);
    {
        p.enter(Item::Band(Band::Details(DBand::Directives)));
        for (dg, ix) in dirs {
            let d = &f.get(dg).directives[ix];
            node_card(p, Item::Directive { cluster: dg, ix }, &d.id, vec![muted(d.cite.clone())], &d.text, palette::ink(), palette::amber(), 2, width);
        }
        p.leave();
    }
    p.blank();
    fixed_band(p, Band::Details(DBand::Constraints), "Constraints", look::meta([look::word(look::noun(cons.len(), "constraint"), Tone::Plain), look::word("you", Tone::Plain)]), width);
    {
        p.enter(Item::Band(Band::Details(DBand::Constraints)));
        for (cg, ix) in cons {
            let c = &f.get(cg).constraints[ix];
            node_card(p, Item::Constraint { cluster: cg, ix }, &c.id, c.cite.clone().map(muted).into_iter().collect(), &c.text, palette::ink(), palette::amber(), 2, width);
        }
        p.leave();
    }
    p.blank();
    // OBJECTIVES and MILESTONES: the agents'.
    let scope = app.scope();
    // An item of a cluster below (^G) says whose it is at its top left; its
    // id is the soup's, and needs no cluster to name it (msg 235).
    let owner = |o: ClusterId| if o == id { Vec::new() } else { vec![Span::styled(format!("{} · ", f.get(o).name), palette::ink2())] };
    let objs: Vec<(ClusterId, u32)> = scope.iter().flat_map(|&o| f.get(o).objectives.iter().map(move |x| (o, x.serial))).collect();
    fixed_band(p, Band::Details(DBand::Objectives), "Objectives", look::meta([look::word(look::noun(objs.len(), "objective"), Tone::Plain), look::word("agents", Tone::Plain)]), width);
    {
        p.enter(Item::Band(Band::Details(DBand::Objectives)));
        for (o, serial) in objs {
            let x = f.get(o);
            let Some(ob) = x.objective(serial) else { continue };
            let (done, of) = x.objective_progress(serial);
            let border = if of > 0 && done == of { palette::muted() } else { palette::code() };
            let mut left = owner(o);
            left.push(muted(format!("{done}/{of}")));
            node_card(p, Item::Objective { cluster: o, serial }, &ob.written_id(), left, &ob.title, palette::ink(), border, 2, width);
        }
        p.leave();
    }
    p.blank();
    let mss: Vec<(ClusterId, u32)> = scope.iter().flat_map(|&o| f.get(o).milestones.iter().map(move |m| (o, m.serial))).collect();
    fixed_band(p, Band::Details(DBand::Milestones), "Milestones", look::meta([look::word(look::noun(mss.len(), "milestone"), Tone::Plain), look::word("agents", Tone::Plain)]), width);
    {
        p.enter(Item::Band(Band::Details(DBand::Milestones)));
        for (o, serial) in mss {
            let x = f.get(o);
            let Some(m) = x.milestone(serial) else { continue };
            let st = x.milestone_state(m);
            let (done, of) = x.milestone_progress(m);
            let text_style = if st == MsState::Done { palette::muted() } else { palette::ink() };
            let mut left = owner(o);
            left.extend([Span::styled(format!("{} {}", st.glyph(), ms_word(st)), ms_style(st)), muted(format!(" · {done}/{of}"))]);
            node_card(p, Item::Milestone { cluster: o, serial }, &m.written_id(), left, &m.handle, text_style, ms_style(st), 2, width);
        }
        p.leave();
    }
    p.blank();
    // THE CLUSTER: its id, and what may be done to one made here.
    let made = if g.made_here { "made here" } else { "seeded" };
    fixed_band(p, Band::Details(DBand::Cluster), "Cluster", look::meta([look::word(g.key.clone(), Tone::Plain), look::word(made, Tone::Plain)]), width);
    {
        p.enter(Item::Band(Band::Details(DBand::Cluster)));
        if g.made_here {
            let parent = g.parent.map_or_else(|| "the root".to_string(), |x| f.path(x));
            let below = f.subtree(id).len() - 1;
            let tail = if below > 0 { format!(" and the {below} under it") } else { String::new() };
            // Buttons, each pressed at one click (task 0209 D18), what
            // each acts on beside it.
            let mut acts: Vec<(u8, &str, String)> = vec![(0, "rename", g.name.clone()), (1, "move under", parent), (2, "delete", format!("{}{tail}", g.name))];
            // A seeding whose face died (IS-32): run it again, or clear it.
            if g.background.iter().any(|b| b.kind == "seed" && b.state == "interrupted") {
                acts.push((3, "reseed", "run its interrupted seeding again".into()));
                acts.push((4, "clear seeding", "drop the interrupted mark, no claims".into()));
            }
            for (k, label, summary) in acts {
                let start = p.begin();
                let b = look::button(label, false);
                let pad = 14usize.saturating_sub(label.chars().count() + 2).max(1);
                p.push(Line::from(vec![lead(), b, Span::raw(" ".repeat(pad)), Span::styled(summary, palette::ink2())]));
                p.end(start, Item::Manage(k));
            }
        } else {
            for l in look::table(vec![("id".into(), vec![Span::styled(g.key.clone(), palette::ink2())])], 2, width) {
                p.push(l);
            }
        }
        p.leave();
    }
    p.blank();
}

// ── Questions ──────────────────────────────────────────────────────────────

/// One question as a BOX (msg 165): its id a legend at the top right; the
/// question; then Context, Options (numbered, each a **bold one-line
/// summary.** and the rest of it flowing on) and Recommendation. Never
/// collapsed; no score, no ordering number.
fn question_box(app: &App, p: &mut Page, cluster: ClusterId, q: &Question, owner: Option<&str>, width: usize) {
    let item = Item::Question { cluster, serial: q.serial };
    let iw = inside(width);
    let style = if q.blocking { palette::amber() } else { palette::muted() };
    let legend = match owner {
        Some(o) => format!("{} · {o}", q.written_id()),
        None => q.written_id(),
    };
    let legend = Span::styled(if q.blocking { format!("◆ {legend}") } else { legend }, if q.blocking { palette::amber().add_modifier(Modifier::BOLD) } else { palette::ink2() });
    let start = p.begin();
    p.head();
    p.push(box_top(Vec::new(), Some(legend), style, width));
    let body = p.begin();
    wrap(&mut p.lines, &q.question, iw, 0, palette::ink().add_modifier(Modifier::BOLD));
    p.blank();
    part_head(p, "Context");
    para(p, &q.why, palette::ink2(), width);
    // Its inserted artifacts: each a card inside the card (msg 186), an item
    // inside the question.
    let mut options: Vec<(Range<usize>, Item)> = Vec::new();
    for (ix, a) in q.artifacts.iter().enumerate() {
        p.blank();
        let s0 = p.begin();
        artifact_card(app, p, a, 2, iw);
        options.push((s0..p.lines.len(), Item::QArt { cluster, serial: q.serial, ix }));
    }
    p.blank();
    part_head(p, "Options");
    // The options are items inside the question, recorded after it so a
    // click on an option's row picks the option.
    for (i, o) in q.options.iter().enumerate() {
        let (name, rest) = Question::option_parts(o);
        let n = i + 1;
        let os = p.begin();
        let num = format!("  {n}. ");
        let nw = display_width(&num);
        let text = if rest.is_empty() { format!("**{name}.**") } else { format!("**{name}.** {rest}") };
        for (j, row) in md::lay(&text, iw.saturating_sub(nw).max(1), palette::ink2(), palette::code()).into_iter().enumerate() {
            let mut spans = vec![if j == 0 { muted(num.clone()) } else { Span::raw(" ".repeat(nw)) }];
            spans.extend(row.spans.into_iter().map(|s| if s.style.add_modifier.contains(Modifier::BOLD) { Span::styled(s.content, palette::ink().add_modifier(Modifier::BOLD)) } else { s }));
            p.push(Line::from(spans));
        }
        options.push((os..p.lines.len(), Item::Option { cluster, serial: q.serial, opt: n }));
    }
    p.blank();
    part_head(p, "Recommendation");
    // His recommendation as written, the option it names in bold.
    let rec = match q.lean().0 {
        Some(i) => {
            let (name, _) = Question::option_parts(&q.options[i - 1]);
            format!("**{name}**{}", &q.recommendation[name.len()..])
        }
        None => q.recommendation.clone(),
    };
    para(p, &rec, palette::ink2(), width);
    box_close(p, body, style, width);
    p.end(start, item);
    for (r, it) in options {
        p.items.push(r);
        p.ids.push(it);
        p.parents.push(Some(item));
    }
}

/// What an inserted artifact `a` (an artifact's id, or a path) is: its
/// name, its path, its handle, and its id when it is one of the soup's.
pub fn q_artifact(app: &App, a: &str) -> (String, String, String, Option<String>) {
    if let Some(x) = app.arts.artifacts.iter().find(|x| x.id == a) {
        return (x.name().to_string(), x.location.clone(), x.handle.clone(), Some(x.id.clone()));
    }
    let name = a.rsplit('/').next().unwrap_or(a).to_string();
    (name, a.to_string(), String::new(), None)
}

/// An ARTIFACT card, `width` wide, laid out as page lines (inside a question
/// card, its content's width): `┌─ ▤ name ──── open ↗ ─┐`, its handle and
/// its path inside. `col` is where the card's first column falls on the
/// page, for the `↗`'s click hotspot (it runs `open`).
fn artifact_card(app: &App, p: &mut Page, a: &str, col: usize, width: usize) {
    let (name, path, handle, id) = q_artifact(app, a);
    let border = palette::code();
    let legend = format!("open {OPEN_ICON}");
    let mut lines = vec![look::card_top(Card::Artifact, vec![Span::styled(format!("{} ", look::ARTIFACT_GLYPH), palette::code()), bold(name)], vec![Span::styled(legend.clone(), palette::code())], border, width)];
    let ix = p.paths.len() as u16;
    p.paths.push(path.clone());
    let lcol = col + width.saturating_sub(3 + display_width(&legend));
    p.hotspots.push((p.lines.len(), lcol, display_width(&legend), Hit::OpenAt(ix)));
    let iw = look::inside(width);
    let mut body = Vec::new();
    if !handle.is_empty() {
        wrap(&mut body, &handle, iw, 0, palette::ink2());
    }
    // The path cropped from its LEFT, as on the artifact's own row: its
    // file name is what tells one from another.
    let lead = id.map(|id| format!("{id} · ")).unwrap_or_default();
    let full = crate::cauldron::artpage::tilde(&path);
    let room = iw.saturating_sub(display_width(&lead));
    let shown = if display_width(&full) > room { format!("…{}", full.chars().rev().take(room.saturating_sub(1)).collect::<Vec<_>>().into_iter().rev().collect::<String>()) } else { full };
    body.push(Line::from(look::fit_spans(vec![muted(lead), muted(shown)], iw)));
    let from = lines.len();
    lines.extend(body);
    look::card_close(Card::Artifact, &mut lines, from, border, width);
    p.lines.extend(lines);
}

/// A machine nag (a violation or contradiction on a claimed repo's focus) as
/// a box among his questions (0071 Q2: focus nags ranked).
fn nag_box(app: &App, p: &mut Page, r: RepoId, n: usize, width: usize) {
    let repo = &app.repos.repos[r];
    let node = &repo.nodes[n];
    let iw = inside(width);
    let it = Item::Bp { repo: r, node: n };
    let start = p.begin();
    p.head();
    p.push(box_top(Vec::new(), Some(Span::styled(format!("{} {}", node.glyph(), node.short()), palette::amber())), palette::muted(), width));
    let body = p.begin();
    wrap(&mut p.lines, node.meta_str("line").unwrap_or(&node.handle), iw, 0, palette::ink().add_modifier(Modifier::BOLD));
    p.blank();
    part_head(p, "Context");
    para(p, &format!("{} · {} — {}", repo.name, node.meta_str("kind").unwrap_or(&node.kind), node.handle), palette::ink2(), width);
    let inv: Vec<String> = repo.resolve(node.out("involves")).into_iter().map(|x| repo.nodes[x].short().to_string()).collect();
    if !inv.is_empty() {
        para(p, &format!("involves {}", inv.join(" · ")), palette::muted(), width);
    }
    box_close(p, body, palette::muted(), width);
    p.end(start, it);
}

/// The value a machine nag ranks at among his questions: the FLOOR (0071 Q2,
/// its recommendation "focus nags ranked", built while the question is open).
/// One constant, so the floor is one edit.
pub const NAG_FLOOR: u8 = 50;

/// The machine nags that enter cluster `g`'s ranking: open violations and
/// contradictions touching the focus of a repo it claims (0071 Q2: FOCUS
/// NAGS RANKED). Drag is tolerated by definition and stays on Artifacts.
pub fn focus_nags(app: &App, g: ClusterId) -> Vec<(RepoId, usize)> {
    let mut out: Vec<(RepoId, usize)> = Vec::new();
    for c in app.repos.claims.get(g).map(Vec::as_slice).unwrap_or(&[]) {
        let repo = &app.repos.repos[c.repo];
        for &f in &c.focus {
            for n in repo.nags_touching(f) {
                let ok = matches!(repo.nodes[n].kind.as_str(), "violation" | "contradiction");
                if ok && !out.contains(&(c.repo, n)) {
                    out.push((c.repo, n));
                }
            }
        }
    }
    out
}

fn questions_page(app: &App, p: &mut Page, width: usize) {
    let f = &app.forest;
    let id = app.selected;
    let scope = app.scope();
    enum Row<'a> {
        Q(&'a Question),
        Nag(RepoId, usize),
    }
    // ACTIVE: ranked (blocking first, then the priority the soup computed),
    // the rank never shown.
    let n_active = app.active_questions();
    let blocking: usize = scope.iter().map(|&g| f.get(g).questions.iter().filter(|q| q.blocking).count()).sum();
    let mut meta = Vec::new();
    if blocking > 0 {
        meta.push(look::seg("◆", blocking, "blocking", Tone::Warn));
    }
    meta.extend(count(n_active, "question"));
    if band(app, p, Band::Questions(QBand::Active), "Active", look::meta(meta), width, 0) {
        p.enter(Item::Band(Band::Questions(QBand::Active)));
        let (m, width) = (p.mark(), width.saturating_sub(CARD_IND));
        for &g in &scope {
            let mut rows: Vec<(bool, u8, u32, Row)> = f.get(g).questions.iter().map(|q| (q.blocking, q.value, q.serial, Row::Q(q))).collect();
            rows.extend(focus_nags(app, g).into_iter().map(|(r, n)| (false, NAG_FLOOR, u32::MAX, Row::Nag(r, n))));
            rows.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
            for (_, _, _, row) in rows {
                match row {
                    Row::Q(q) => question_box(app, p, g, q, (g != id).then(|| f.get(g).name.as_str()), width),
                    Row::Nag(r, n) => nag_box(app, p, r, n, width),
                }
            }
        }
        // The questions the seeding linked here from other clusters.
        if !app.aggregate {
            for (o, q) in f.linked_questions(id) {
                question_box(app, p, o, q, Some(&f.get(o).name), width);
            }
        }
        p.shift(m, CARD_IND);
        p.leave();
    }
    // ASSUMED is second and always shown (msg 186: "There should be active,
    // assumed, and then some of the other ones").
    for (qb, st, word) in [(QBand::Assumed, QStatus::Assumed, "Assumed"), (QBand::Complete, QStatus::Complete, "Complete"), (QBand::Superseded, QStatus::Superseded, "Superseded"), (QBand::Archived, QStatus::Archived, "Archived")] {
        let closed: Vec<(ClusterId, &crate::cauldron::model::ClosedQuestion)> = scope.iter().flat_map(|&g| f.get(g).closed_questions.iter().filter(move |c| c.status == st).map(move |c| (g, c))).collect();
        if closed.is_empty() && qb != QBand::Assumed {
            continue;
        }
        let b = Band::Questions(qb);
        if !band(app, p, b, word, count(closed.len(), "question"), width, 0) {
            continue;
        }
        p.enter(Item::Band(b));
        let (m, width) = (p.mark(), width.saturating_sub(CARD_IND));
        for (g, c) in closed {
            let iw = inside(width);
            let start = p.begin();
            let legend = if g == id { format!("Q-{}", c.serial) } else { format!("Q-{} · {}", c.serial, f.get(g).name) };
            p.push(box_top(Vec::new(), Some(muted(legend)), palette::muted(), width));
            let body = p.begin();
            wrap(&mut p.lines, &c.question, iw, 0, palette::ink2());
            p.blank();
            part_head(p, match c.status {
                QStatus::Assumed => "Assumed",
                QStatus::Complete => "Answer",
                QStatus::Superseded => "Superseded",
                QStatus::Archived => "Archived",
            });
            para(p, &c.answer, palette::ink2(), width);
            para(p, &format!("→ {} · {}", c.resolved_by, c.date), palette::muted(), width);
            box_close(p, body, palette::muted(), width);
            p.end(start, Item::Closed { cluster: g, serial: c.serial });
        }
        p.shift(m, CARD_IND);
        p.leave();
    }
    below(app, p, width);
}

/// `n below`: what the clusters below hold on this tab, as one bare count
/// (msg 33, F10); → turns aggregate on.
fn below(app: &App, p: &mut Page, width: usize) {
    if app.aggregate {
        return;
    }
    let f = &app.forest;
    let id = app.selected;
    let count = |g: ClusterId| -> usize {
        let x = f.get(g);
        match app.ktab {
            // A sub-cluster's focus nags rank among its questions (0071 Q2),
            // so they count where its questions count.
            KTab::Questions => x.questions.len() + focus_nags(app, g).len(),
            KTab::Actions => x.open_actions(),
            // The graph below: its tasks and milestones.
            KTab::Tasks => x.tasks.len() + x.milestones.len(),
            // Own-only by default (F5): what runs below is a count here.
            KTab::Status => {
                let (r, pk, pe) = crate::cauldron::status::cluster_counts(app, g, false);
                r + pk + pe
            }
            _ => 0,
        }
    };
    let total: usize = f.subtree(id)[1..].iter().map(|&g| count(g)).sum();
    let _ = width;
    if total > 0 {
        let start = p.begin();
        p.head();
        p.push(Line::from(muted(format!("{total} below"))));
        p.end(start, Item::Footer(Footer::Below));
    }
}

// ── Human Actions ──────────────────────────────────────────────────────────

/// Each human action as a BOX (msg 165): its header the status, its age
/// since it was filed and its step progress `n/m`; its id the legend; then
/// the ask, and its STEPS, each holding his hand — its text, the links to
/// open, the texts to copy, the fields to paste into (masked when secret),
/// and its `done`. Nothing here sets the action's status: the soup does.
fn ha_page(app: &App, p: &mut Page, width: usize) {
    let f = &app.forest;
    let id = app.selected;
    for (hb, word) in [(HaBand::Active, "Active"), (HaBand::Done, "Done"), (HaBand::Archived, "Archived")] {
        let hs: Vec<(ClusterId, &crate::cauldron::model::HumanAction)> = app
            .scope()
            .into_iter()
            .flat_map(|g| f.get(g).human_actions.iter().map(move |h| (g, h)))
            .filter(|(_, h)| match hb {
                HaBand::Active => matches!(h.status, HaStatus::Open | HaStatus::Draft),
                HaBand::Done => h.status == HaStatus::Resolved,
                HaBand::Archived => h.status == HaStatus::Retired,
            })
            .collect();
        if hs.is_empty() && hb != HaBand::Active {
            continue;
        }
        let b = Band::Actions(hb);
        if !band(app, p, b, word, count(hs.len(), "action"), width, 0) {
            continue;
        }
        p.enter(Item::Band(b));
        let m = p.mark();
        for (g, h) in hs {
            ha_box(app, p, g, h, (g != id).then(|| f.get(g).name.clone()), width.saturating_sub(CARD_IND));
        }
        p.shift(m, CARD_IND);
        p.leave();
    }
    below(app, p, width);
}

fn ha_box(app: &App, p: &mut Page, g: ClusterId, h: &crate::cauldron::model::HumanAction, owner: Option<String>, width: usize) {
    use crate::cauldron::tickets::{parse, Block};
    let iw = inside(width);
    let key = app.forest.get(g).key.clone();
    let t = parse(&h.body);
    let item = Item::Ha { cluster: g, serial: h.serial };
    let done_n = (0..t.steps.len()).filter(|&i| app.step_done(g, h.serial, &t, i)).count();
    let (glyph, word, style) = match h.status {
        HaStatus::Open => ("●", "open", palette::amber()),
        HaStatus::Draft => ("◐", "draft · awaiting a value", palette::ink2()),
        HaStatus::Resolved => ("✓", "resolved", palette::muted()),
        HaStatus::Retired => ("✕", "retired", palette::muted()),
    };
    let border = if h.status == HaStatus::Open { palette::amber() } else { palette::muted() };
    let header = vec![
        Span::styled(format!("{glyph} {word}"), style),
        muted(format!(" · {}", age(&h.filed))),
        muted(format!(" · {done_n}/{} step{}", t.steps.len(), plural(t.steps.len()))),
    ];
    let legend = match &owner {
        Some(o) => format!("{} · {o}", h.written_id()),
        None => h.written_id(),
    };
    let start = p.begin();
    p.head();
    p.push(box_top(header, Some(Span::styled(legend, palette::ink2())), border, width));
    let body = p.begin();
    let text_style = if h.status == HaStatus::Retired { palette::muted().add_modifier(Modifier::CROSSED_OUT) } else { palette::ink().add_modifier(Modifier::BOLD) };
    wrap(&mut p.lines, &h.ask, iw, 0, text_style);
    if !t.intro.is_empty() {
        p.blank();
        for l in &t.intro {
            wrap(&mut p.lines, l, iw, 0, palette::ink2());
        }
    }
    // Its parts are items inside it, recorded after it.
    let mut parts: Vec<(Range<usize>, Item)> = Vec::new();
    let mut pi = 0;
    for (si, st) in t.steps.iter().enumerate() {
        p.blank();
        let done = app.step_done(g, h.serial, &t, si);
        let title = if st.title.is_empty() { format!("Step {}", si + 1) } else { st.title.clone() };
        let state = if done { Span::styled("✓ done", palette::green()) } else { muted("") };
        p.push(lr(vec![muted(format!("{}  ", si + 1)), bold(title)], Some(state), iw));
        let ind = 3;
        for b in &st.blocks {
            match b {
                Block::Text(x) => wrap(&mut p.lines, x, iw, ind, palette::ink2()),
                Block::Open { label, url } => {
                    let s = p.begin();
                    p.push(lr(vec![Span::raw(" ".repeat(ind)), Span::styled(format!("↗ {label}"), palette::code())], None, iw));
                    p.push(Line::from(vec![Span::raw(" ".repeat(ind + 2)), muted(crop(&app.ha_url(url), iw.saturating_sub(ind + 2)))]));
                    parts.push((s..p.lines.len(), Item::HaPart { cluster: g, serial: h.serial, part: pi }));
                    pi += 1;
                }
                Block::Copy { label, text } => {
                    let s = p.begin();
                    p.push(Line::from(vec![Span::raw(" ".repeat(ind)), Span::styled(format!("⧉ copy  {label}"), palette::code())]));
                    for row in wrap_plain(text, iw.saturating_sub(ind + 2).max(1)) {
                        p.push(Line::from(vec![Span::raw(" ".repeat(ind + 2)), Span::styled(row, palette::ink())]));
                    }
                    parts.push((s..p.lines.len(), Item::HaPart { cluster: g, serial: h.serial, part: pi }));
                    pi += 1;
                }
                Block::Field(fl) => {
                    let s = p.begin();
                    let submitted = app.tickets.is_submitted(&key, h.serial, &fl.name);
                    p.push(lr(vec![Span::raw(" ".repeat(ind)), Span::styled(fl.label.clone(), palette::ink())], Some(muted(fl.name.clone())), iw));
                    if let Some(n) = &fl.note {
                        p.push(Line::from(vec![Span::raw(" ".repeat(ind)), muted(n.clone())]));
                    }
                    // V1.8: the input. A secret's every character draws as
                    // `•`; a value that already reached its destination
                    // shows a dim placeholder (never the value, never its
                    // length); a field the validator refuses has no input.
                    let entry = app.ha_entry.as_ref().filter(|e| e.cluster == g && e.serial == h.serial && e.step == si);
                    let typing = entry.is_some_and(|e| e.editing.as_deref() == Some(fl.name.as_str()));
                    let typed = entry.and_then(|e| e.value(&fl.name)).filter(|v| !v.is_empty() || typing);
                    let dest = muted(format!("  → {}", fl.route.word()));
                    let input: Vec<Span<'static>> = if let Some(r) = crate::cauldron::tickets::field_reject(fl) {
                        vec![Span::styled(format!("✗ refused: {r}"), palette::amber())]
                    } else if fl.route == crate::cauldron::route::Route::Unset {
                        vec![muted("no destination: its ticket names no route")]
                    } else if let Some(v) = typed {
                        let shown = if fl.secret { "•".repeat(v.chars()) } else { v.expose().to_string() };
                        let mut out = vec![Span::styled(format!("› {shown}"), palette::ink())];
                        if typing {
                            out.push(Span::styled("▏", palette::code()));
                        }
                        out.push(dest);
                        out
                    } else if submitted {
                        vec![muted("•••• submitted"), dest]
                    } else {
                        vec![muted("Enter to type"), dest]
                    };
                    let mut spans = vec![Span::raw(" ".repeat(ind))];
                    spans.extend(input);
                    p.push(Line::from(spans));
                    parts.push((s..p.lines.len(), Item::HaPart { cluster: g, serial: h.serial, part: pi }));
                    pi += 1;
                }
            }
        }
        if let Some(d) = &st.deliverable {
            wrap(&mut p.lines, &format!("Deliverable: {d}"), iw, ind, palette::muted());
        }
        let s = p.begin();
        let button = match (h.status, done) {
            (HaStatus::Open, true) => Span::styled("[✓ done]", palette::green()),
            (HaStatus::Open, false) => Span::styled("[ done ]", palette::code()),
            (_, true) => muted("[✓ done]"),
            _ => muted("[ done ]"),
        };
        p.push(Line::from(vec![Span::raw(" ".repeat(ind)), button]));
        parts.push((s..p.lines.len(), Item::HaPart { cluster: g, serial: h.serial, part: pi }));
        pi += 1;
    }
    box_close(p, body, border, width);
    p.end(start, item);
    for (r, it) in parts {
        p.items.push(r);
        p.ids.push(it);
        p.parents.push(Some(item));
    }
}

// ── Status ─────────────────────────────────────────────────────────────────

/// The cluster's ripples and its execution graph as nested bands (msg 165),
/// each header counting what it holds.
fn status_page(app: &App, p: &mut Page, width: usize) {
    use crate::cauldron::status::{census, three_words, threads_live, What};
    let f = &app.forest;
    // The cluster's three words (design 0142): the census's entries on this
    // cluster (or, under ^G, its subtree) — the same list the cauldron's
    // Status strip counts, so the two cannot disagree.
    let scope = app.scope();
    let mine: Vec<_> = census(app).into_iter().filter(|e| e.cluster.is_some_and(|s| scope.contains(&s))).collect();
    let n = |st: RippleState| mine.iter().filter(|e| e.state == st).count();
    let (running, parked, pending) = (n(RippleState::Running), n(RippleState::Parked), n(RippleState::Pending));
    let open = app.band_open(Band::Ripples);
    let start = p.begin();
    p.head();
    p.push(look::band_line(0, open, vec![bold("Ripples".to_string())], three_words(running, parked, pending, threads_live(app)), width));
    p.end(start, Item::Band(Band::Ripples));
    if open {
        p.enter(Item::Band(Band::Ripples));
        for (rb, st, word, n, glyph, tone) in [(RBand::Active, RippleState::Running, "Running", running, "▶", Tone::Good), (RBand::Parked, RippleState::Parked, "Parked", parked, "◆", Tone::Warn), (RBand::Pending, RippleState::Pending, "Pending", pending, "◇", Tone::Plain)] {
            let b = Band::RippleState(rb);
            let tone = if n > 0 { tone } else { Tone::Plain };
            if !band(app, p, b, word, vec![look::seg(glyph, look::noun(n, "run"), "", tone)], width, 1) {
                continue;
            }
            p.enter(Item::Band(b));
            for e in mine.iter().filter(|e| e.state == st) {
                match (&e.what, e.cluster) {
                    (What::Task(serial), Some(g)) => ripple_row(app, p, g, *serial, width),
                    (What::Run(id), _) => run_row(app, p, id, width),
                    _ => {}
                }
            }
            p.leave();
        }
        let bg: Vec<(ClusterId, usize)> = app.scope().into_iter().flat_map(|g| (0..f.get(g).background.len()).map(move |i| (g, i))).collect();
        let b = Band::RippleState(RBand::Background);
        if !bg.is_empty() && band(app, p, b, "Background", vec![look::seg("⟳", look::noun(bg.len(), "run"), "", Tone::Plain)], width, 1) {
            p.enter(Item::Band(b));
            for (g, i) in bg {
                let bk = &f.get(g).background[i];
                let (glyph, gst) = match bk.state.as_str() {
                    "failed" | "refused" => ("✕ ", palette::red()),
                    "interrupted" => ("! ", palette::amber()),
                    "parked" => ("◆ ", palette::ink2()),
                    _ => ("⟳ ", palette::muted()),
                };
                let start = p.begin();
                p.push(lr(
                    vec![Span::raw("    "), Span::styled(glyph, gst), Span::styled(format!("{:<10}", bk.kind), palette::ink()), Span::styled(bk.line.clone(), palette::ink2())],
                    Some(muted(bk.when.clone())),
                    width,
                ));
                p.end(start, Item::Background { cluster: g, ix: i });
            }
            p.leave();
        }
        p.leave();
    }
    // The log: what he dumped here and what ran here (F4, msg 1). → opens it.
    let n = log_entries(app).len();
    let start = p.begin();
    p.head();
    p.push(lr(vec![Span::styled("log ▸", palette::ink2())], (n > 0).then(|| muted(n.to_string())), width));
    p.end(start, Item::Footer(Footer::Log));
    below(app, p, width);
}

// ── Tasks ──────────────────────────────────────────────────────────────────

/// THE TASKS TAB (b964d571 msg 23: "let's have a Tasks tab for DAG stuff"):
/// the cluster's execution graph, the Engine band that was on Status — its
/// tasks and milestones as a DAG, each cluster below its own band under ^G
/// — then `n below`.
fn tasks_page(app: &App, p: &mut Page, width: usize) {
    let f = &app.forest;
    let tasks: usize = app.scope().iter().map(|&g| f.get(g).tasks.len()).sum();
    let ms: usize = app.scope().iter().map(|&g| f.get(g).milestones.len()).sum();
    let done: usize = app.scope().iter().map(|&g| f.get(g).tasks.iter().filter(|t| t.state == TaskState::Done).count()).sum();
    if band(app, p, Band::Engine, "Engine", look::meta([count(tasks, "task").remove(0), count(ms, "milestone").remove(0), look::seg("✓", done, "done", Tone::Plain)]), width, 0) {
        p.enter(Item::Band(Band::Engine));
        engine_page(app, p, width);
        p.leave();
    }
    below(app, p, width);
}

/// One ripple row: running, parked or pending — always one line.
fn ripple_row(app: &App, p: &mut Page, g: ClusterId, serial: u32, width: usize) {
    let f = &app.forest;
    let Some(t) = f.task(g, serial) else { return };
    let x = f.get(g);
    let id = if g == app.selected { t.written_id() } else { format!("{}/{}", x.name, t.written_id()) };
    let item = Item::Ripple { cluster: g, serial };
    let start = p.begin();
    match x.task_ripple(t) {
        RippleState::Running => {
            let mut text = t.title.clone();
            if let Some(d) = &t.doing {
                text = format!("{text} — {d}");
            }
            strong(p, vec![Span::raw("    "), Span::styled("▶ ", palette::code()), bold(format!("{id}  "))], &text, palette::ink(), task_token(t), width, false);
        }
        RippleState::Parked => {
            // Why the gate refused it, and what it could run on (0142).
            let mut right = Vec::new();
            if let Some(pk) = &t.parked {
                let st = if pk.needs_config() { palette::red() } else { palette::ink2() };
                right.push(Span::styled(pk.phrase.clone(), st));
                if crate::cauldron::designs::PARKED_ROW_CARRIES_ELIGIBLE && !pk.eligible.is_empty() {
                    right.push(muted(format!("  {}", pk.eligible.join(" · "))));
                }
                right.push(muted(format!("  {}", pk.since)));
            }
            let lead = vec![Span::raw("    "), Span::styled("◆ ", palette::amber()), bold(format!("{id}  "))];
            let rw: usize = right.iter().map(sw).sum();
            let tw = width.saturating_sub(lead.iter().map(sw).sum::<usize>() + rw + 2).max(1);
            let mut spans = lead;
            spans.push(Span::styled(crop(&t.title, tw), palette::ink()));
            let used: usize = spans.iter().map(sw).sum();
            spans.push(Span::raw(" ".repeat(width.saturating_sub(used + rw))));
            spans.extend(right);
            p.push(Line::from(spans));
        }
        st => {
            let waiting: Vec<String> = t.depends_on.iter().filter(|d| x.tasks.iter().any(|o| o.serial == **d && o.state != TaskState::Done)).map(|d| format!("T-{d}")).collect();
            let mut right = Vec::new();
            if !waiting.is_empty() {
                right.push(format!("← {}", waiting.join(" ")));
            }
            if !t.blocked_by.is_empty() {
                right.push(format!("■ {}", t.blocked_by.join(" ")));
            }
            if st == RippleState::Pending {
                if let Some(r) = &t.retry {
                    right.push(format!("↻ retry {r}"));
                }
            }
            let (glyph, gst) = match (st, t.state) {
                (_, TaskState::Blocked) => ("■ ", palette::amber()),
                (RippleState::Parked, _) => ("◆ ", palette::amber()),
                (RippleState::Pending, _) => ("◇ ", palette::muted()),
                _ => ("○ ", palette::muted()),
            };
            let rst = if t.state == TaskState::Blocked { palette::amber() } else { palette::muted() };
            strong(p, vec![Span::raw("    "), Span::styled(glyph, gst), bold(format!("{id}  "))], &t.title, palette::ink(), (!right.is_empty()).then(|| Span::styled(right.join(" · "), rst)), width, false);
        }
    }
    p.end(start, item);
}

/// A ripple's run on a cluster's Status tab (running, or parked by the
/// gate): one line; → opens its run page.
fn run_row(app: &App, p: &mut Page, id: &str, width: usize) {
    let Some((ix, r)) = app.rstore.indexed_runs().find(|(_, r)| r.id == id) else { return };
    let (glyph, gst) = if r.state == "parked" { ("◆ ", palette::amber()) } else { ("▶ ", palette::code()) };
    let start = p.begin();
    let right = [r.account.as_str(), r.elapsed.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
    strong(p, vec![Span::raw("    "), Span::styled(glyph, gst), bold(format!("{:<10}", r.ripple))], &r.line, palette::ink2(), (!right.is_empty()).then(|| muted(right)), width, false);
    p.end(start, Item::Run(ix));
}

// ── Engine ─────────────────────────────────────────────────────────────────

fn task_style(s: TaskState) -> Style {
    match s {
        TaskState::Done => palette::muted(),
        TaskState::Active => palette::code(),
        TaskState::Ready => palette::ink(),
        TaskState::Blocked => palette::amber(),
    }
}

fn ms_word(s: MsState) -> &'static str {
    match s {
        MsState::Done => "done",
        MsState::Active => "active",
        MsState::Pending => "pending",
        MsState::Blocked => "blocked",
    }
}

fn ms_style(s: MsState) -> Style {
    match s {
        MsState::Done => palette::muted(),
        MsState::Active => palette::code(),
        MsState::Pending => palette::ink2(),
        MsState::Blocked => palette::amber(),
    }
}

/// A task's right-hand token: the run (account · elapsed · retry) while
/// running, the report when done, the blocker while blocked.
fn task_token(t: &Task) -> Option<Span<'static>> {
    match t.state {
        TaskState::Active => {
            let mut parts: Vec<String> = t.account.iter().cloned().collect();
            parts.extend(t.elapsed.clone());
            if let Some(r) = &t.retry {
                parts.push(format!("retry {r}"));
            }
            (!parts.is_empty()).then(|| Span::styled(parts.join(" · "), palette::code()))
        }
        TaskState::Done => t.report.clone().map(muted),
        TaskState::Blocked => Some(Span::styled(format!("■ {}", t.blocked_by.join(" ")), palette::amber())),
        TaskState::Ready => None,
    }
}


fn engine_page(app: &App, p: &mut Page, width: usize) {
    let f = &app.forest;
    for g in app.scope() {
        let x = f.get(g);
        if x.tasks.is_empty() && x.milestones.is_empty() {
            continue;
        }
        // Under aggregate each cluster is a band of the Engine.
        let mine = g != app.selected;
        if mine {
            let done = x.tasks.iter().filter(|t| t.state == TaskState::Done).count();
            let meta = look::meta([count(x.tasks.len(), "task").remove(0), count(x.milestones.len(), "milestone").remove(0), look::seg("✓", done, "done", Tone::Plain)]);
            if !band(app, p, Band::EngineCluster(g), &x.name, meta, width, 1) {
                continue;
            }
            p.enter(Item::Band(Band::EngineCluster(g)));
        }
        let binding: Vec<String> = f.binding_directives(g).into_iter().map(|(dg, ix)| f.get(dg).directives[ix].id.clone()).collect();
        let graph = engine::layout(x, &binding);
        let margin_w = graph.lanes * 2;
        // The graph sits two columns in from its band's title (V1; 0217
        // B5): Engine's title is at 2, a cluster's band under aggregate at 4.
        let inset = if mine { 6 } else { 4 };
        let width = width.saturating_sub(inset);
        let from = p.mark();
        for (i, row) in graph.rows.iter().enumerate() {
            match &row.kind {
                Kind::Connector => p.push(Line::from(muted(graph.margin(i, " ")))),
                Kind::Directive(id) => {
                    let Some(Item::Directive { cluster: dg, ix }) = directive_item(app, id) else { continue };
                    let d = &f.get(dg).directives[ix];
                    let it = Item::Directive { cluster: dg, ix };
                    let start = p.begin();
                    p.head();
                    // A row like the objective's: no leader to the cite (V3).
                    let lead = vec![muted(graph.margin(i, " ")), Span::raw(" "), bold(format!("{}  ", d.id))];
                    strong(p, lead, &d.text, palette::ink(), Some(muted(d.cite.clone())), width, false);
                    p.end(start, it);
                }
                Kind::Objective(s) => {
                    let Some(o) = x.objective(*s) else { continue };
                    let (done, of) = x.objective_progress(*s);
                    let it = Item::Objective { cluster: g, serial: *s };
                    let start = p.begin();
                    let lead = vec![muted(graph.margin(i, " ")), Span::raw(" "), bold(format!("{}  ", o.written_id()))];
                    strong(p, lead, &o.title, palette::ink().add_modifier(Modifier::BOLD), Some(muted(format!("{done}/{of}"))), width, false);
                    p.end(start, it);
                }
                Kind::Node(Node::Milestone(s)) => {
                    let Some(m) = x.milestone(*s) else { continue };
                    let st = x.milestone_state(m);
                    let (done, of) = x.milestone_progress(m);
                    let it = Item::Milestone { cluster: g, serial: *s };
                    let start = p.begin();
                    let margin = graph.margin(i, "\u{0}");
                    let lead = glyph_margin(&margin, st.glyph(), ms_style(st));
                    let mut lead = lead;
                    lead.push(Span::raw(" "));
                    lead.push(bold(format!("{}  ", m.written_id())));
                    let text_style = if st == MsState::Done { palette::muted() } else { palette::ink() };
                    strong(p, lead, &m.handle, text_style, Some(muted(format!("{done}/{of}"))), width, false);
                    p.end(start, it);
                }
                Kind::Node(Node::Task(s)) => {
                    let Some(t) = f.task(g, *s) else { continue };
                    let it = Item::Task { cluster: g, serial: *s };
                    let start = p.begin();
                    let margin = graph.margin(i, "\u{0}");
                    let mut lead = glyph_margin(&margin, t.state.glyph(), task_style(t.state));
                    lead.push(Span::raw("   "));
                    lead.push(bold(format!("{}  ", t.written_id())));
                    let text_style = if t.state == TaskState::Done { palette::muted() } else { palette::ink() };
                    strong(p, lead, &t.title, text_style, task_token(t), width, false);
                    wait_lines(app, p, g, t, &graph.under(i), margin_w + 3, width);
                    p.end(start, it);
                }
            }
        }
        p.shift(from, inset);
        if mine {
            p.leave();
        }
        p.blank();
    }
    // Aggregate: the waits that cross from one cluster in scope to another,
    // each its own row (→ opens the waiting task's trace page).
    let scope = app.scope();
    let waits = if app.aggregate { f.waits(&scope, true) } else { Vec::new() };
    if !waits.is_empty() {
        let open = band(app, p, Band::EngineAcross, "Across clusters", vec![muted(look::noun(waits.len(), "wait"))], width, 1);
        for (g, s, o, l) in waits.into_iter().filter(|_| open) {
            let Some(t) = f.task(g, s) else { continue };
            let (there, state) = local_word(app, o, l);
            let start = p.begin();
            let lead = vec![Span::styled(format!("{} ", t.state.glyph()), task_style(t.state)), bold(format!("{}/{}  ", f.get(g).name, t.written_id()))];
            strong(p, lead, &format!("⇠ {}/{} {there}", f.get(o).name, pages_local_id(l)), palette::ink(), Some(muted(state)), width, false);
            p.end(start, Item::Task { cluster: g, serial: s });
        }
        p.blank();
    }
}

/// `T-4` / `M-2`.
fn pages_local_id(l: Local) -> String {
    match l {
        Local::T(n) => format!("T-{n}"),
        Local::M(n) => format!("M-{n}"),
        Local::Q(n) => format!("Q-{n}"),
        Local::Ha(n) => format!("HA-{n}"),
        Local::Obj(n) => format!("OBJ-{n}"),
    }
}

/// What a waited-on task or milestone is called, and its state word.
fn local_word(app: &App, o: ClusterId, l: Local) -> (String, String) {
    let x = app.forest.get(o);
    match l {
        Local::T(n) => x.tasks.iter().find(|t| t.serial == n).map_or((String::new(), String::new()), |t| (t.title.clone(), t.state.label().to_string())),
        Local::M(n) => x.milestone(n).map_or((String::new(), String::new()), |m| {
            let st = match x.milestone_state(m) {
                MsState::Done => "done",
                MsState::Active => "active",
                MsState::Pending => "pending",
                MsState::Blocked => "blocked",
            };
            (m.handle.clone(), st.to_string())
        }),
        _ => (String::new(), String::new()),
    }
}

/// Under a task on the Engine: what it waits on in OTHER clusters (`⇠`),
/// and what in other clusters waits on it (`⇢`), with each one's state.
fn wait_lines(app: &App, p: &mut Page, g: ClusterId, t: &Task, margin: &str, indent: usize, width: usize) {
    let f = &app.forest;
    let pad = indent.saturating_sub(display_width(margin));
    for w in &t.waits_on {
        let Some(l) = Local::parse(w) else { continue };
        let Some(o) = f.owner(l) else { continue };
        let (there, state) = local_word(app, o, l);
        let text = format!("⇠ waits on {w} {there} · {state} · {}", f.get(o).name);
        p.push(Line::from(vec![muted(margin.to_string()), Span::raw(" ".repeat(pad)), Span::styled(crop(&text, width.saturating_sub(indent)), palette::ink2())]));
    }
    for (o, s) in f.waited_by(g, Local::T(t.serial)) {
        let Some(x) = f.task(o, s) else { continue };
        let text = format!("⇢ {} {} waits on it · {} · {}", x.written_id(), x.title, x.state.label(), f.get(o).name);
        p.push(Line::from(vec![muted(margin.to_string()), Span::raw(" ".repeat(pad)), Span::styled(crop(&text, width.saturating_sub(indent)), palette::ink2())]));
    }
}

/// The rail margin as spans, the node's `\0` placeholder drawn as its glyph
/// in its state's colour and every rail muted.
fn glyph_margin(margin: &str, glyph: &str, style: Style) -> Vec<Span<'static>> {
    match margin.split_once('\u{0}') {
        Some((a, b)) => vec![muted(a.to_string()), Span::styled(glyph.to_string(), style), muted(b.to_string())],
        None => vec![muted(margin.to_string())],
    }
}

// ── Regions (on a trace page) ────────────────────────────────────────────────────────────────

/// A region's live rollup: ready/active tasks, blocking questions, open
/// issues, then nodes — zero counts left out.
fn region_rollup(app: &App, r: RegionId) -> String {
    let c = &app.corpus;
    let members: Vec<&crate::cauldron::corpus::Node> = c.members[r].iter().map(|&n| &c.nodes[n]).collect();
    let count = |k: &str, states: &[&str]| members.iter().filter(|n| n.kind == k && (states.is_empty() || states.contains(&n.state.as_str()))).count();
    let mut parts = Vec::new();
    for (n, word) in [
        (count("task", &["ready", "active"]), "task"),
        (count("question", &["open"]), "question"),
        (count("issue", &["open"]), "issue"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {word}{}", plural(n)));
        }
    }
    let n = members.len();
    if n > 0 {
        parts.push(format!("{n} node{}", plural(n)));
    }
    parts.join(" · ")
}

fn schema_token(app: &App, r: RegionId) -> Span<'static> {
    let v = app.corpus.regions[r].schema_version;
    let style = if v < app.corpus.schema_version { palette::amber() } else { palette::muted() };
    Span::styled(format!("v{v}"), style)
}

/// One region: `R-6   <handle>   v5`, the rollup on the second row (with the
/// other clusters that claim it), and the boundary when `full`.
fn region_entry(app: &App, p: &mut Page, r: RegionId, others: &[ClusterId], full: bool, width: usize) {
    let c = &app.corpus;
    let reg = &c.regions[r];
    let lead = format!("{:<6}", reg.id);
    let lw = display_width(&lead);
    let mut roll = region_rollup(app, r);
    if !others.is_empty() {
        let names: Vec<String> = others.iter().map(|&g| app.forest.get(g).name.clone()).collect();
        roll = if roll.is_empty() { format!("also {}", names.join(", ")) } else { format!("{roll} · also {}", names.join(", ")) };
    }
    let tok = schema_token(app, r);
    let tw = width.saturating_sub(lw + sw(&tok) + 2).max(1);
    let rows = md::lay(&reg.handle, tw, palette::ink(), palette::code());
    let n_rows = rows.len();
    for (j, row) in rows.into_iter().enumerate() {
        let mut spans = if j == 0 { vec![bold(lead.clone())] } else { vec![Span::raw(" ".repeat(lw))] };
        spans.extend(row.spans);
        let right = match j {
            0 => Some(tok.clone()),
            1 if !roll.is_empty() => Some(muted(roll.clone())),
            _ => None,
        };
        p.push(lr(spans, right, width));
    }
    if n_rows < 2 && !roll.is_empty() {
        p.push(lr(vec![], Some(muted(roll)), width));
    }
    if full {
        sub(p, lw, &reg.boundary, palette::ink2(), width);
        // Where it sits in the narrows DAG, in the trace grammar.
        let up: Vec<&str> = reg.narrows.iter().map(|&x| c.regions[x].id.as_str()).collect();
        if !up.is_empty() {
            p.push(Line::from(vec![Span::raw(" ".repeat(lw)), muted(format!("↑ {}", up.join(" ")))]));
        }
    }
}

/// One soup node as a row: `IS-14  open  <handle>`.
fn node_row(app: &App, p: &mut Page, n: NodeId, indent: usize, width: usize) {
    let node = &app.corpus.nodes[n];
    let start = p.begin();
    p.push(lr(
        vec![
            Span::raw(" ".repeat(indent)),
            bold(format!("{:<8}", node.id)),
            Span::styled(format!("{:<11}", crop(&node.state, 10)), palette::ink2()),
            Span::styled(node.handle.clone(), palette::ink()),
        ],
        None,
        width,
    ));
    p.end(start, Item::Node(n));
}

// ── the trace page ─────────────────────────────────────────────────────────

/// The page a trace entry is about, and the row expanded in place on it.
fn trace_target(app: &App) -> (Item, Option<Item>) {
    let top = app.trace.last().expect("a trace is open").0;
    if is_trace_root(top) {
        return (top, None);
    }
    let root = app.trace.iter().rev().map(|t| t.0).find(|&t| is_trace_root(t));
    match root {
        Some(r) => (r, Some(top)),
        None => (top, None),
    }
}

fn trace_page(app: &App, p: &mut Page, width: usize) {
    let f = &app.forest;
    let mut crumbs = vec![app.ktab.label().to_string()];
    crumbs.extend(app.trace.iter().map(|t| item_id(app, t.0)));
    p.push(Line::from(muted(crop(&crumbs.join(" › "), width))));
    let (root, expanded) = trace_target(app);
    if root == Item::Log {
        log_page(app, p, expanded, width);
        return;
    }
    const COL: usize = 9;
    // The node's own strong line, with what it needs and what blocks it.
    let start = p.begin();
    let mut needs: Vec<String> = Vec::new();
    let mut blockers: Vec<String> = Vec::new();
    let mut below: Vec<Item> = Vec::new();
    let mut edges: Vec<crate::cauldron::model::Edge> = Vec::new();
    let mut body: Option<String> = None;
    match root {
        Item::Task { cluster, serial } => {
            let x = f.get(cluster);
            if let Some(t) = f.task(cluster, serial) {
                let right = match t.state {
                    TaskState::Blocked => Some(Span::styled("blocked", palette::amber())),
                    _ => task_token(t),
                };
                strong(p, vec![Span::styled(format!("{} ", t.state.glyph()), task_style(t.state)), bold(format!("{:<6}", t.written_id()))], &t.title, palette::ink(), right, width, true);
                needs = t.depends_on.iter().map(|d| format!("T-{d}")).collect();
                blockers = t.blocked_by.clone();
                edges = t.edges.clone();
                let _ = x;
            }
        }
        Item::Milestone { cluster, serial } => {
            let x = f.get(cluster);
            if let Some(m) = x.milestone(serial) {
                let st = x.milestone_state(m);
                let (d, of) = x.milestone_progress(m);
                strong(p, vec![Span::styled(format!("{} ", st.glyph()), ms_style(st)), bold(format!("{:<6}", m.written_id()))], &m.handle, palette::ink(), Some(muted(format!("{d}/{of}"))), width, true);
                needs = m.depends_on.clone();
                edges = m.edges.clone();
                below = m.tasks.iter().map(|&t| Item::Task { cluster, serial: t }).collect();
            }
        }
        Item::Objective { cluster, serial } => {
            let x = f.get(cluster);
            if let Some(o) = x.objective(serial) {
                let (d, of) = x.objective_progress(serial);
                strong(p, vec![Span::raw("  "), bold(format!("{:<6}", o.written_id()))], &o.title, palette::ink().add_modifier(Modifier::BOLD), Some(muted(format!("{d}/{of}"))), width, true);
                let mut ms: Vec<_> = x.milestones.iter().filter(|m| m.objective == serial).collect();
                ms.sort_by_key(|m| m.serial);
                below = ms.into_iter().map(|m| Item::Milestone { cluster, serial: m.serial }).collect();
            }
        }
        Item::Directive { cluster, ix } => {
            let d = &f.get(cluster).directives[ix];
            strong(p, vec![Span::raw("  "), bold(format!("{:<6}", d.id))], &d.text, palette::ink(), Some(muted(d.cite.clone())), width, true);
            for (g, x) in f.clusters.iter().enumerate() {
                for o in x.objectives.iter().filter(|o| o.serves == d.id) {
                    below.push(Item::Objective { cluster: g, serial: o.serial });
                }
            }
        }
        Item::Ha { cluster, serial } => {
            if let Some(h) = f.get(cluster).human_action(serial) {
                let glyph = match h.status {
                    HaStatus::Open => ("● ", palette::amber()),
                    HaStatus::Draft => ("◐ ", palette::ink2()),
                    HaStatus::Resolved => ("✓ ", palette::muted()),
                    HaStatus::Retired => ("✕ ", palette::muted()),
                };
                strong(p, vec![Span::styled(glyph.0, glyph.1), bold(format!("{:<6}", h.written_id()))], &h.ask, palette::ink(), Some(muted(h.filed.clone())), width, true);
                blockers = h.blocks.iter().map(|b| format!("■ {b}")).collect();
                body = Some(h.body.clone()).filter(|b| !b.trim().is_empty());
            }
        }
        _ => {}
    }
    let mut own = Vec::new();
    if !needs.is_empty() {
        own.push(muted(format!("← {}   ", needs.join(" "))));
    }
    if !blockers.is_empty() && !matches!(root, Item::Ha { .. }) {
        own.push(Span::styled(format!("■ {}", blockers.join(" ")), palette::amber()));
    } else if !blockers.is_empty() {
        own.push(Span::styled(blockers.join(" "), palette::amber()));
    }
    if !own.is_empty() {
        let mut spans = vec![Span::raw(" ".repeat(COL - 1))];
        spans.extend(own);
        p.push(Line::from(spans));
    }
    p.end(start, root);
    if let Some(b) = body {
        p.blank();
        sub(p, COL - 1, &b, palette::ink2(), width);
    }
    let t = trace_of(app, root);
    // Up the chain.
    if !t.chain.is_empty() {
        p.blank();
        p.head();
        for c in t.chain {
            trace_row(app, p, c, "↑ ", width);
        }
    }
    // Down: what serves it, or what it holds.
    if !below.is_empty() {
        p.blank();
        p.head();
        for c in below {
            trace_row(app, p, c, "↓ ", width);
        }
    }
    // His message, quoted.
    if let Some(cite) = &t.msg {
        p.blank();
        p.head();
        let start = p.begin();
        let idx = message_index(app, cite);
        let date = idx.map(|i| app.messages[i].1.date.clone());
        p.push(lr(vec![Span::styled(cite.clone(), palette::ink2())], date.map(muted), width));
        if let Some(i) = idx {
            let text = format!("\u{201c}{}\u{201d}", app.messages[i].1.text.trim());
            for row in wrap_plain(&text, width.saturating_sub(5).max(1)) {
                p.push(Line::from(vec![Span::raw("  "), Span::styled(row, palette::ink2())]));
            }
        }
        // An id with no stored text is still shown (never an error), and is
        // not an item: there is nothing to quote.
        if let Some(i) = idx {
            p.end(start, Item::Message(i));
        }
    }
    // What blocks it: questions, human actions, contradictions, soup nodes.
    let cluster = match root {
        Item::Task { cluster, .. } | Item::Milestone { cluster, .. } | Item::Objective { cluster, .. } | Item::Ha { cluster, .. } => Some(cluster),
        _ => None,
    };
    let mut block_items: Vec<Item> = Vec::new();
    if let Some(g) = cluster {
        let mut refs: Vec<String> = Vec::new();
        if let Item::Task { serial, .. } = root {
            refs = f.task(g, serial).map(|t| t.blocked_by.clone()).unwrap_or_default();
        }
        if let Item::Milestone { serial, .. } = root {
            refs = f.get(g).milestone(serial).map(|m| m.depends_on.iter().filter(|d| d.starts_with("Q-")).cloned().collect()).unwrap_or_default();
        }
        if let Item::Ha { serial, .. } = root {
            refs = f.get(g).human_action(serial).map(|h| h.blocks.clone()).unwrap_or_default();
        }
        for r in refs {
            if let Some(it) = local_item(g, &r) {
                block_items.push(it);
            } else if let Some((repo, node)) = app.repos.locate(&r) {
                block_items.push(Item::Bp { repo, node });
            } else if let Some(n) = app.corpus.find_node(&r) {
                block_items.push(Item::Node(n));
            }
        }
    }
    if !block_items.is_empty() {
        p.blank();
        p.head();
        for b in block_items {
            trace_row(app, p, b, "", width);
            if Some(b) == expanded {
                expansion(app, p, b, width);
                // The expansion is part of the row it opened.
                if let Some(r) = p.items.last_mut() {
                    r.end = p.lines.len();
                }
            }
        }
    }
    // Into the blueprint.
    let links: Vec<(String, Item)> = edges
        .iter()
        .filter_map(|e| {
            let word = match e.kind.as_str() {
                "builds" => "builds",
                "depends_on" => "waits on",
                "clears" => "clears",
                _ => return None,
            };
            app.repos.locate(&e.to).map(|(repo, node)| (word.to_string(), Item::Bp { repo, node }))
        })
        .collect();
    if !links.is_empty() {
        p.blank();
        p.head();
        for (word, it) in links {
            let Item::Bp { repo, node } = it else { continue };
            let n = &app.repos.repos[repo].nodes[node];
            let start = p.begin();
            p.push(lr(
                vec![muted(format!("{word:<9}")), bold(format!("{}  ", n.short())), Span::styled(n.handle.clone(), palette::ink2())],
                Some(muted(format!("{} · {}", app.repos.repos[repo].name, n.state))),
                width,
            ));
            if Some(it) == expanded {
                expansion(app, p, it, width);
            }
            p.end(start, it);
        }
    }
    // Its regions.
    let regions: Vec<RegionId> = t.regions.iter().filter_map(|r| app.corpus.find_region(r)).collect();
    if !regions.is_empty() {
        p.blank();
        p.head();
        for r in regions {
            let start = p.begin();
            region_entry(app, p, r, &[], false, width);
            p.end(start, Item::Region(r));
            if Some(Item::Region(r)) == expanded {
                for &n in &app.corpus.members[r] {
                    node_row(app, p, n, COL, width);
                }
            }
        }
    }
}

/// One row of a trace page: `↑ M-4    <handle>   <state>`.
fn trace_row(app: &App, p: &mut Page, it: Item, arrow: &str, width: usize) {
    let f = &app.forest;
    let lead = |id: String| vec![Span::raw(""), muted(format!("{arrow:<2}")), bold(format!("{:<7}", id))];
    let start = p.begin();
    match it {
        Item::Milestone { cluster, serial } => {
            let x = f.get(cluster);
            if let Some(m) = x.milestone(serial) {
                let (d, of) = x.milestone_progress(m);
                strong(p, lead(m.written_id()), &m.handle, palette::ink(), Some(muted(format!("{d}/{of}"))), width, false);
            }
        }
        Item::Objective { cluster, serial } => {
            let x = f.get(cluster);
            if let Some(o) = x.objective(serial) {
                let (d, of) = x.objective_progress(serial);
                strong(p, lead(o.written_id()), &o.title, palette::ink(), Some(muted(format!("{d}/{of}"))), width, false);
            }
        }
        Item::Directive { cluster, ix } => {
            let d = &f.get(cluster).directives[ix];
            strong(p, lead(d.id.clone()), &d.text, palette::ink(), Some(muted(d.cite.clone())), width, false);
        }
        Item::Task { cluster, serial } => {
            if let Some(t) = f.task(cluster, serial) {
                let mut l = lead(t.written_id());
                l[1] = Span::styled(format!("{} ", t.state.glyph()), task_style(t.state));
                strong(p, l, &t.title, palette::ink(), task_token(t), width, false);
            }
        }
        Item::Question { cluster, serial } => {
            if let Some(q) = f.get(cluster).question(serial) {
                let mut l = lead(q.written_id());
                l[1] = if q.blocking { Span::styled("◆ ", palette::amber()) } else { Span::raw("  ") };
                strong(p, l, &q.question, palette::ink(), None, width, false);
                p.push(Line::from(vec![Span::raw(" ".repeat(11)), Span::styled(format!("▸ {}", q.lean_name()), palette::ink2())]));
            }
        }
        Item::Ha { cluster, serial } => {
            if let Some(h) = f.get(cluster).human_action(serial) {
                let mut l = lead(h.written_id());
                l[1] = Span::styled("● ", palette::amber());
                strong(p, l, &h.ask, palette::ink(), Some(muted(h.filed.clone())), width, false);
            }
        }
        Item::Bp { repo, node } => {
            let n = &app.repos.repos[repo].nodes[node];
            let mut l = lead(n.short().to_string());
            l[1] = Span::styled(format!("{} ", n.glyph()), palette::amber());
            strong(p, l, n.meta_str("line").unwrap_or(&n.handle), palette::ink(), Some(muted(app.repos.repos[repo].name.clone())), width, false);
        }
        Item::Node(n) => {
            let node = &app.corpus.nodes[n];
            strong(p, lead(node.id.clone()), &node.handle, palette::ink(), Some(muted(node.state.clone())), width, false);
        }
        _ => {}
    }
    if p.lines.len() > start {
        p.end(start, it);
    }
}

/// A row opened in place on a trace page: a question's options, a blueprint
/// entry's handle, a soup node's body.
fn expansion(app: &App, p: &mut Page, it: Item, width: usize) {
    match it {
        Item::Question { cluster, serial } => {
            let Some(q) = app.forest.get(cluster).question(serial) else { return };
            let (lean, _) = q.lean();
            for (i, o) in q.options.iter().enumerate() {
                let (name, rest) = Question::option_parts(o);
                let mark = if lean == Some(i + 1) { "▸ " } else { "  " };
                p.push(lr(
                    vec![Span::raw(" ".repeat(9)), Span::styled(mark, palette::amber()), muted(format!("{}  ", i + 1)), Span::styled(format!("{name:<23} "), palette::ink()), Span::styled(rest.to_string(), palette::ink2())],
                    None,
                    width,
                ));
            }
        }
        Item::Bp { repo, node } => {
            let n = &app.repos.repos[repo].nodes[node];
            sub(p, 11, &n.handle, palette::ink2(), width);
            p.push(Line::from(vec![Span::raw(" ".repeat(11)), muted(n.id.clone())]));
        }
        Item::Node(n) => sub(p, 11, &app.corpus.nodes[n].body, palette::ink2(), width),
        _ => {}
    }
}

// ── the log ────────────────────────────────────────────────────────────────

/// One line of a cluster's log.
#[derive(Clone)]
pub enum LogEntry<'a> {
    Sent(&'a crate::cauldron::store::Send),
    /// A run, with its index in `ripples::Store::indexed_runs`.
    Run(usize, &'a crate::cauldron::ripples::Run),
}

/// The cluster's log (msg 1: "I can look at a log and understand what
/// happened"): every send that LANDED on the clusters this page covers (its
/// `targets`), and every ripple run that TOUCHED them — ran on one, or wrote a
/// node in a region one claims — newest first. The clusters covered are the
/// page's own, or everything below too under aggregate (^G).
pub fn log_entries(app: &App) -> Vec<LogEntry<'_>> {
    let scope = app.scope();
    let keys: Vec<&str> = scope.iter().map(|&g| app.forest.get(g).key.as_str()).collect();
    let touches = |r: &crate::cauldron::ripples::Run| {
        keys.contains(&r.cluster.as_str())
            || r.written_nodes().into_iter().filter_map(|n| app.corpus.find_node(n)).any(|n| {
                app.corpus.nodes[n].in_region.iter().any(|reg| scope.iter().any(|&g| app.corpus.claims.get(g).is_some_and(|c| c.contains(reg))))
            })
    };
    let mut out: Vec<(Option<chrono::DateTime<chrono::Local>>, LogEntry)> = Vec::new();
    for s in app.sends.sends.iter().filter(|s| s.targets.iter().any(|t| keys.contains(&t.as_str()))) {
        let at = chrono::DateTime::parse_from_rfc3339(&s.at).ok().map(|t| t.with_timezone(&chrono::Local));
        out.push((at, LogEntry::Sent(s)));
    }
    for (ix, r) in app.rstore.indexed_runs().filter(|(_, r)| touches(r)) {
        out.push((r.instant(), LogEntry::Run(ix, r)));
    }
    // Newest first; anything with no time at the end. Stable, so a send and
    // a run at one instant keep the order they were read in.
    out.sort_by_key(|e| std::cmp::Reverse(e.0));
    out.into_iter().map(|(_, e)| e).collect()
}

/// The log's day word for an instant: `today`, `yesterday`, or its date.
fn log_day(t: Option<chrono::DateTime<chrono::Local>>) -> String {
    let Some(t) = t else { return String::new() };
    let (d, today) = (t.date_naive(), chrono::Local::now().date_naive());
    if d == today {
        "today".into()
    } else if Some(d) == today.pred_opt() {
        "yesterday".into()
    } else {
        d.format("%Y-%m-%d").to_string()
    }
}

/// The log page: the crumb (drawn by the trace page), then one strong row per
/// entry under its day, newest first. A send's row is its time, its text and
/// where it landed, with its tags on the line under it; → opens the whole
/// text in place. A run's row is the runs page's own row (glyph, time,
/// ripple, what it did, its id); → opens its run page.
fn log_page(app: &App, p: &mut Page, expanded: Option<Item>, width: usize) {
    // The glyph, the clock, then a 12-column id (a send's number, a run's
    // ripple), so every row's text starts in one column.
    const LEAD: usize = 21;
    let here = app.forest.get(app.selected).key.as_str();
    let mut day = String::new();
    let entries: Vec<_> = log_entries(app)
        .into_iter()
        .map(|e| {
            let (t, item) = match &e {
                LogEntry::Sent(s) => (chrono::DateTime::parse_from_rfc3339(&s.at).ok().map(|t| t.with_timezone(&chrono::Local)), Item::Sent(s.seq)),
                LogEntry::Run(ix, r) => (r.instant(), Item::Run(*ix)),
            };
            (t, item, e)
        })
        .collect();
    let mut shown = true;
    for (i, (t, item, e)) in entries.iter().cloned().enumerate() {
        let d = log_day(t);
        // Each day is a band (audit 0177 V3: no divider rule), open until he
        // closes it, counting what it holds.
        if d != day || i == 0 {
            let date = t.map(|t| t.date_naive());
            let n = entries.iter().filter(|(u, ..)| u.map(|u| u.date_naive()) == date).count();
            p.blank();
            let label = if d.is_empty() { "Undated".to_string() } else { look::sentence(&d) };
            shown = band(app, p, Band::LogDay(date), &label, vec![muted(look::noun(n, "entry"))], width, 0);
            day = d;
        }
        if !shown {
            continue;
        }
        let clock = t.map_or_else(|| "     ".to_string(), |t| t.format("%H:%M").to_string());
        let start = p.begin();
        match e {
            LogEntry::Sent(s) => {
                let landed: Vec<String> = s.targets.iter().map(|k| app.cluster_name(k)).collect();
                let lead = vec![Span::styled("› ", palette::amber()), muted(format!("{clock}  ")), muted(format!("{:<12}", format!("#{}", s.seq)))];
                let opened = expanded == Some(item);
                let text = if opened { s.text.trim().to_string() } else { s.text.split_whitespace().collect::<Vec<_>>().join(" ") };
                let right = muted(landed.join(" · "));
                if opened {
                    p.push(lr(lead, Some(right), width));
                    for row in wrap_plain(&text, width.saturating_sub(LEAD).max(1)) {
                        let mut spans = vec![Span::raw(" ".repeat(LEAD))];
                        spans.extend(tagged_spans(&row, palette::ink()));
                        p.push(Line::from(spans));
                    }
                } else {
                    let lw: usize = lead.iter().map(sw).sum();
                    let tw = width.saturating_sub(lw + sw(&right) + 2);
                    let mut left = lead;
                    left.extend(tagged_spans(&crop(&text, tw), palette::ink()));
                    p.push(lr(left, Some(right), width));
                }
                if !s.tags.is_empty() {
                    p.push(Line::from(vec![Span::raw(" ".repeat(LEAD)), muted(crop(&s.tags.join(" "), width.saturating_sub(LEAD)))]));
                }
            }
            LogEntry::Run(_, r) => {
                let (g, gst) = crate::cauldron::bus::state_glyph(&r.state);
                let what = if r.cite.is_empty() { r.line.clone() } else { format!("{} → {}", r.cite, r.line) };
                let right = if r.cluster == here { r.id.clone() } else { format!("{} · {}", app.cluster_name(&r.cluster), r.id) };
                let lead = vec![Span::styled(format!("{g} "), gst), muted(format!("{clock}  ")), bold(format!("{:<12}", r.ripple))];
                strong(p, lead, &what, palette::ink2(), Some(muted(right)), width, false);
            }
        }
        p.end(start, item);
    }
}

// ── Repos ──────────────────────────────────────────────────────────────────

/// The icon beside a path that `open`s it (msg 165: "like the open file
/// icon ... an arrow pointing up diagonal").
pub const OPEN_ICON: &str = "↗";

/// `locked · built`, `draft`, `adopted`.
fn state_word(n: &crate::cauldron::blueprint::BpNode) -> String {
    if n.built && n.state != "built" {
        format!("{} · built", n.state)
    } else {
        n.state.clone()
    }
}

/// The `←` working-on line for an entry.
fn working_line(app: &App, id: &str) -> Option<String> {
    let w = working_on(&app.forest, id);
    if w.is_empty() {
        return None;
    }
    let parts: Vec<String> = w.iter().map(|x| format!("{} {} {}", app.forest.path(x.cluster).replace(" / ", "/"), x.item, x.state)).collect();
    Some(format!("← {}", parts.join(" · ")))
}

/// A secondary entry of a repo's blueprint (a violation, drag, a
/// contradiction, a migration, a capability, a schema, an event) as a row
/// of its BLUEPRINT band on the Artifacts tab, `depth` levels in.
pub(crate) fn secondary_entry(app: &App, p: &mut Page, r: RepoId, i: usize, depth: usize, width: usize) {
    let repo = &app.repos.repos[r];
    let n = &repo.nodes[i];
    let ind = "  ".repeat(depth);
    let (glyph, name, line, tail, gst): (&str, String, String, String, Style) = match n.kind.as_str() {
        "violation" => (n.glyph(), n.meta_str("kind").unwrap_or(&n.kind).to_string(), n.meta_str("line").unwrap_or(&n.handle).to_string(), format!("{} site{}  since {}", n.meta_u64("sites").unwrap_or(0), plural(n.meta_u64("sites").unwrap_or(0) as usize), n.meta_str("since").unwrap_or("")), palette::amber()),
        "drag" => {
            let inv: Vec<String> = repo.resolve(n.out("involves")).into_iter().map(|x| repo.nodes[x].short().to_string()).collect();
            (n.glyph(), short_id(&n.id).trim_start_matches("DRAG-").to_string(), inv.join(" · "), format!("{} inst.  tolerated {}", n.meta_u64("instances").unwrap_or(0), n.meta_str("tolerated").unwrap_or("")), palette::ink2())
        }
        "contradiction" => (n.glyph(), short_id(&n.id).trim_start_matches("CONTRA-").to_string(), n.meta_str("line").unwrap_or(&n.handle).to_string(), format!("since {}", n.meta_str("since").unwrap_or("")), palette::amber()),
        "migration" => (if n.state == "undescribed" { "◌" } else { " " }, n.meta_str("at").unwrap_or("").to_string(), n.meta_str("slug").unwrap_or(&n.handle).to_string(), format!("{}  built {}", n.state, n.out("built").count()), palette::ink2()),
        "capability" => (n.glyph(), n.short().trim_start_matches("CAP-").to_string(), n.handle.clone(), [n.meta_str("provenance").unwrap_or(""), n.meta_str("verify").unwrap_or("")].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · "), palette::ink2()),
        _ => (" ", n.short().to_string(), n.handle.clone(), repo.inbound(i, "owns").first().map(|&o| repo.nodes[o].short().to_string()).unwrap_or_default(), palette::ink2()),
    };
    let start = p.begin();
    p.push(lr(
        vec![Span::raw(ind), Span::styled(format!("{glyph} "), gst), bold(format!("{:<22} ", crop(&name, 21))), Span::styled(line, palette::ink2())],
        (!tail.is_empty()).then(|| muted(tail)),
        width,
    ));
    p.end(start, Item::Bp { repo: r, node: i });
}

/// A module on the repo page: one row collapsed; expanded, its interface,
/// what it owns, what nags about it, and who is working on it.
pub(crate) fn module_rows(app: &App, p: &mut Page, r: RepoId, m: usize, depth: usize, width: usize) {
    let repo = &app.repos.repos[r];
    let n = &repo.nodes[m];
    let open = app.rexpanded.contains(&(r, m));
    let ind = " ".repeat(depth * 2);
    let nags = repo.nags_touching(m);
    let right = (!nags.is_empty()).then(|| {
        let mut kinds: Vec<(&str, &str, usize)> = Vec::new();
        for &s in &nags {
            let (g, k) = (repo.nodes[s].glyph(), repo.nodes[s].kind.as_str());
            match kinds.iter_mut().find(|(x, ..)| *x == g) {
                Some(x) => x.2 += 1,
                None => kinds.push((g, k, 1)),
            }
        }
        // A noun count each (msg 403): `● 1 violation · ◇ 2 drags`.
        Span::styled(kinds.iter().map(|(g, k, c)| format!("{g} {}", look::noun(*c, if *k == "migration" { "undescribed migration" } else { k }))).collect::<Vec<_>>().join(" · "), palette::amber())
    });
    let start = p.begin();
    let lead = vec![
        Span::raw(ind.clone()),
        muted(if open { "▾ " } else { "▸ " }),
        bold(format!("{:<22} ", n.short())),
        Span::styled(format!("{:<16} ", state_word(n)), palette::ink2()),
    ];
    strong(p, lead, &n.handle, palette::ink(), right, width, true);
    if !open {
        p.end(start, Item::Bp { repo: r, node: m });
        for s in repo.submodules(m) {
            module_rows(app, p, r, s, depth + 1, width);
        }
        return;
    }
    if let Some(w) = working_line(app, &n.id) {
        p.push(Line::from(vec![Span::raw(format!("{ind}    ")), muted(crop(&w, width.saturating_sub(ind.len() + 4)))]));
    }
    p.end(start, Item::Bp { repo: r, node: m });
    let row = |p: &mut Page, i: usize, glyph: &str, text: String, tail: Option<String>| {
        let x = &repo.nodes[i];
        let s = p.begin();
        p.push(lr(
            vec![Span::raw(format!("{ind}    ")), Span::styled(glyph.to_string(), palette::amber()), Span::styled(format!("{:<20} ", x.short()), palette::ink()), Span::styled(text, palette::ink2())],
            tail.map(muted),
            width,
        ));
        p.end(s, Item::Bp { repo: r, node: i });
    };
    if let Some(i) = repo.interface_of(m) {
        let ops: Vec<String> = repo.nodes[i].meta.get("ops").and_then(|o| o.as_array()).map(|a| a.iter().filter_map(|o| o.get("slug").and_then(|s| s.as_str()).map(String::from)).collect()).unwrap_or_default();
        row(p, i, "", ops.join(" · "), Some(format!("{} op{}", ops.len(), plural(ops.len()))));
    }
    for i in repo.resolve(n.out("owns")) {
        row(p, i, "", repo.nodes[i].handle.clone(), None);
    }
    for i in nags {
        let x = &repo.nodes[i];
        let tail = x.meta_u64("sites").map(|s| format!("{s} site{}", plural(s as usize))).or_else(|| x.meta_u64("instances").map(|s| format!("{s} inst.")));
        row(p, i, &format!("{} ", x.glyph()), x.meta_str("line").unwrap_or(&x.handle).to_string(), tail);
    }
    for s in repo.submodules(m) {
        module_rows(app, p, r, s, depth + 1, width);
    }
}

/// One blueprint entry's page: its full id, state and handle, its computed
/// inverses (owns · fronted · uses · emits · owner · users · consumers), the
/// secondary nodes that involve it, who is working on it, what built it.
fn entry_page(app: &App, p: &mut Page, r: RepoId, i: usize, width: usize) {
    let repo = &app.repos.repos[r];
    let n = &repo.nodes[i];
    p.push(Line::from(muted(format!("Nodes › {} › {}", repo.name, n.short()))));
    let start = p.begin();
    let mut head = format!("{} · {}", n.kind, state_word(n));
    if let Some(m) = n.meta_str("built_for") {
        head.push_str(&format!(" for {}", short_id(m)));
    }
    if n.kind == "interface" {
        if let Some(&m) = repo.resolve(n.out("fronts")).first() {
            head.push_str(&format!(" · fronts {}", repo.nodes[m].short()));
        }
    }
    // The entry's title line: its id, its kind and state flush right (no rule, V3).
    p.push(lr(vec![bold(n.id.clone())], Some(muted(head)), width));
    wrap(&mut p.lines, &n.handle, width, 2, palette::ink());
    if n.kind == "module" {
        let mut parts = Vec::new();
        if let Some(pp) = &n.part_of {
            parts.push(format!("part of {}", short_id(pp)));
        }
        if let (Some(d), Some(l)) = (n.meta_u64("definitions"), n.meta_u64("linked")) {
            parts.push(format!("{d} definitions · {l} linked · {} unlinked", d.saturating_sub(l)));
        }
        if !parts.is_empty() {
            p.push(Line::from(vec![Span::raw("  "), muted(parts.join(" · "))]));
        }
    }
    for k in ["kind", "line", "sites", "instances", "since", "tolerated", "at", "slug", "provenance", "verify"] {
        if let Some(v) = n.meta.get(k) {
            let v = v.as_str().map(String::from).unwrap_or_else(|| v.to_string());
            p.push(Line::from(vec![Span::raw("  "), muted(format!("{k:<11}")), Span::styled(v, palette::ink2())]));
        }
    }
    p.end(start, Item::Bp { repo: r, node: i });
    p.blank();
    // Interface operations, two lines each.
    if let Some(ops) = n.meta.get("ops").and_then(|o| o.as_array()) {
        p.head();
        for o in ops {
            let slug = o.get("slug").and_then(|s| s.as_str()).unwrap_or("");
            let purpose = o.get("purpose").and_then(|s| s.as_str()).unwrap_or("");
            let cons = o.get("constructor").and_then(|c| c.as_bool()).unwrap_or(false);
            p.push(lr(vec![Span::raw("  "), bold(format!("{slug:<10}")), Span::styled(purpose.to_string(), palette::ink2())], cons.then(|| muted("constructor")), width));
            let list = |k: &str| o.get(k).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| short_id(x).to_string()).collect::<Vec<_>>().join(" · ")).unwrap_or_default();
            let mut io = vec![Span::raw(" ".repeat(12))];
            for (k, w) in [("in", "in  "), ("out", "out  "), ("err", "err  ")] {
                let v = list(k);
                if !v.is_empty() {
                    io.push(muted(w));
                    io.push(Span::styled(format!("{v}   "), palette::ink2()));
                }
            }
            if io.len() > 1 {
                p.push(lr(io, None, width));
            }
        }
        p.blank();
    }
    let block = |p: &mut Page, word: &str, ids: Vec<usize>, with_owner: bool| {
        if ids.is_empty() {
            return;
        }
        p.head();
        for (j, x) in ids.into_iter().enumerate() {
            let e = &repo.nodes[x];
            let start = p.begin();
            let owner = if with_owner { repo.inbound(x, "owns").first().map(|&o| repo.nodes[o].short().to_string()) } else { None };
            let text = owner.unwrap_or_else(|| e.handle.clone());
            p.push(lr(
                vec![Span::raw("  "), muted(format!("{:<11}", if j == 0 { word } else { "" })), Span::styled(format!("{:<22} ", e.short()), palette::ink()), Span::styled(text, palette::ink2())],
                None,
                width,
            ));
            p.end(start, Item::Bp { repo: r, node: x });
        }
    };
    block(p, "owns", repo.resolve(n.out("owns")), false);
    if n.kind == "module" {
        block(p, "fronted", repo.interface_of(i).into_iter().collect(), false);
    }
    block(p, "uses", repo.resolve(n.out("uses")), true);
    block(p, "emits", repo.resolve(n.out("emits")), false);
    block(p, "owner", repo.inbound(i, "owns"), false);
    block(p, "fronts", repo.resolve(n.out("fronts")), false);
    let users: Vec<usize> = repo.inbound(i, "uses");
    block(p, if n.kind == "interface" { "consumers" } else { "users" }, users, false);
    block(p, "involves", repo.resolve(n.out("involves")), false);
    block(p, "built", repo.resolve(n.out("built")), false);
    // The secondary nodes that involve it.
    let inv: Vec<usize> = repo.inbound(i, "involves");
    if !inv.is_empty() {
        p.blank();
        p.head();
        for x in inv {
            let e = &repo.nodes[x];
            let tail = e.meta_u64("sites").map(|s| format!("{s} site{}", plural(s as usize))).or_else(|| e.meta_u64("instances").map(|s| format!("{s} instances"))).or_else(|| e.meta_str("verify").map(String::from));
            let start = p.begin();
            p.push(lr(
                vec![Span::raw("  "), Span::styled(format!("{} ", e.glyph()), if e.nags() { palette::amber() } else { palette::ink2() }), bold(format!("{:<28} ", crop(&e.id, 27))), Span::styled(e.meta_str("line").unwrap_or(&e.handle).to_string(), palette::ink2())],
                tail.map(muted),
                width,
            ));
            p.end(start, Item::Bp { repo: r, node: x });
        }
    }
    let work = working_on(&app.forest, &n.id);
    let built_by: Vec<usize> = repo.inbound(i, "built");
    if !work.is_empty() || !built_by.is_empty() {
        p.blank();
    }
    for (j, w) in work.iter().enumerate() {
        let mut tail = w.state.clone();
        if let Some(s) = &w.since {
            tail.push_str(&format!("   since {s}"));
        }
        if let Some(rp) = &w.ripple {
            tail.push_str(&format!("   ripple {rp} running"));
        }
        p.push(lr(
            vec![Span::raw("  "), muted(format!("{:<12}", if j == 0 { "working on" } else { "" })), Span::styled(format!("{} {}", app.forest.path(w.cluster).replace(" / ", "/"), w.item), palette::ink())],
            Some(muted(tail)),
            width,
        ));
    }
    if !built_by.is_empty() {
        let mut bb = built_by;
        bb.sort_by(|&a, &b| repo.nodes[b].meta_str("at").cmp(&repo.nodes[a].meta_str("at")));
        let parts: Vec<String> = bb
            .iter()
            .map(|&x| {
                let e = &repo.nodes[x];
                let at = e.meta_str("at").unwrap_or("");
                let at = at.get(5..).unwrap_or(at);
                if e.state == "undescribed" {
                    format!("MIG {at} ◌ undescribed")
                } else {
                    format!("MIG {at} {}", e.state)
                }
            })
            .collect();
        p.push(Line::from(vec![Span::raw("  "), muted(format!("{:<12}", "built by")), Span::styled(crop(&parts.join(" · "), width.saturating_sub(14)), palette::ink2())]));
    }
}
