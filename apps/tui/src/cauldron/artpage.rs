//! **The Nodes tab (it was Soup, msg 235; then Contents, msg 387), drawn** (8e4fca10 msg 186: "maybe we merge soup and
//! artifacts ... we can have different bands for regions ... a band for
//! artifacts ... a band for like repos ... call it just directories").
//! REGIONS · ARTIFACTS (it was CREATED) · REPOS · DIRECTORIES, each a band
//! whose header is ONE line — glyph, name, its path and `↗`, a rule, its
//! metadata on the right — so a closed page reads as a table of contents
//! and every level of nesting costs one row (the header IS the line between
//! it and its body). FILE SYSTEM is config information: Config › Cauldron ›
//! Directories lists its roots.
//!
//! A region opens to its nodes — artifacts among them, since an artifact is
//! a soup node wrapping a file (his Q11) — and a node opens its body in
//! place. Under the search (^F), REGIONS holds only the regions the query
//! reaches, each open on the nodes it matched.
//!
//! A repo opens to WORKTREES (status bands ACTIVE · IDLE · MERGED · MISSING,
//! a row per worktree with the same columns on every row), BLUEPRINT (the
//! 0071 repo page, cut into bands, closed) and FILES. A directory opens to
//! its FILES. CREATED holds a band per kind of artifact, newest first, and
//! MISSING. FILE SYSTEM is the same graph as a tree — the cauldron home and
//! every registered root, a repo carrying a virtual `worktrees/` — and is the
//! same on every cluster.
//!
//! What is open at first is `designs::ARTIFACTS_OPEN`: along the ACTIVE
//! path — a band opens by itself only when something active is inside it (a
//! worktree a run holds, an artifact still being written). Every row with a
//! path has two click targets: its name copies the path, its `↗` runs
//! `open`. The face writes nothing here (`designs::WORKTREE_ACTIONS`).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use tui_render::palette;

use crate::cauldron::app::{App, Hit, Item};
use crate::cauldron::artifacts::{self, art_state, kind_band, list_dir, short_age, size_word, ArtState, Artifact, Worktree, WtState};
use crate::cauldron::designs::{self, ArtifactsOpen};
use crate::cauldron::look::{self, Card, Tone};
use crate::cauldron::model::TaskState;
use crate::cauldron::pages::{age, Page, OPEN_ICON};
use crate::cauldron::tags::Tag;
use crate::cauldron::view::{crop, display_width, plural};

/// The id of an Artifacts row: a hash of its key.
pub fn hash(key: &str) -> u64 {
    let mut h = DefaultHasher::new();
    key.hash(&mut h);
    h.finish()
}

/// What one Artifacts row is, for the keys: its key, whether → opens it in
/// place (a band, a directory, a leaf with detail rows), its path and its tag.
#[derive(Debug, Clone)]
pub struct ArtRow {
    pub item: Item,
    pub key: String,
    pub opens: bool,
    pub path: Option<String>,
    pub tag: Option<Tag>,
    /// Whether it is open now (a card's height no longer says so).
    pub open: bool,
}

/// How many rows a list shows before `n more ▸` (0069's footer grammar).
const LIST_ROWS: usize = 16;

fn muted(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), palette::muted())
}
fn bold(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), palette::ink().add_modifier(Modifier::BOLD))
}
fn sw(s: &Span) -> usize {
    display_width(&s.content)
}

pub fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}

/// `s` padded or cropped to `w` columns.
fn col(s: &str, w: usize) -> String {
    let c = crop(s, w);
    let pad = w.saturating_sub(display_width(&c));
    format!("{c}{}", " ".repeat(pad))
}

struct A<'a> {
    app: &'a App,
    w: usize,
}

impl A<'_> {
    /// Whether `key` is open: its default, turned by him.
    fn open(&self, key: &str, default: bool) -> bool {
        default != self.app.art_toggled.contains(key)
    }

    /// Register a selectable row ending now.
    fn end(&self, p: &mut Page, start: usize, row: ArtRow) {
        p.arts.push(row.clone());
        p.end_item(start, row.item);
    }

    /// The copy and open targets of `path` on the NEXT line, the name at
    /// column `at` and `w` wide, the `↗` at `icon`.
    fn hotspots(&self, p: &mut Page, path: &str, at: usize, w: usize, icon: usize) {
        let line = p.lines.len();
        let ix = p.paths.len() as u16;
        p.paths.push(path.to_string());
        p.hotspots.push((line, at, w, Hit::CopyAt(ix)));
        p.hotspots.push((line, icon, display_width(OPEN_ICON), Hit::OpenAt(ix)));
    }

    /// A band's header: `▾ NAME  path ↗ ────── meta`. Returns whether it is
    /// open. `path` gets the two click targets.
    #[allow(clippy::too_many_arguments)]
    fn band(&self, p: &mut Page, depth: usize, item: Item, key: &str, name: Span<'static>, path: Option<&str>, meta: Vec<Span<'static>>, default_open: bool, tag: Option<Tag>) -> bool {
        let open = self.open(key, default_open);
        let start = p.begin();
        if depth == 0 {
            p.head();
        }
        let ind = "  ".repeat(depth);
        let mut left = vec![Span::raw(ind.clone()), muted(if open { "▾ " } else { "▸ " }), name];
        if let Some(path) = path {
            let shown = tilde(path);
            let at: usize = left.iter().map(sw).sum::<usize>() + 2;
            self.hotspots(p, path, at, display_width(&shown), at + display_width(&shown) + 1);
            left.push(Span::raw("  "));
            left.push(Span::styled(shown, if self.app.flash.as_deref() == Some(path) { palette::green() } else { palette::code() }));
            left.push(Span::styled(format!(" {OPEN_ICON}"), palette::code()));
        }
        // THE band line (`look`): the name and its path on the left, the
        // metadata flush right, NO rule between them (msg 186: "We
        // shouldn't have those lines pointing to the status all the way on
        // the right"). The name and its path stand; the metadata crops to
        // what is left.
        let lw: usize = left.iter().map(sw).sum();
        let meta = look::fit_spans(meta, self.w.saturating_sub(lw + 2));
        p.push(look::lr(left, meta, self.w));
        self.end(p, start, ArtRow { item, key: key.to_string(), opens: true, path: path.map(str::to_string), tag, open });
        open
    }

    /// `n more ▸`: → shows the rest of the list.
    fn more(&self, p: &mut Page, depth: usize, key: &str, n: usize) {
        let start = p.begin();
        p.push(Line::from(vec![Span::raw("  ".repeat(depth)), muted(format!("{n} more ▸"))]));
        let k = format!("more:{key}");
        self.end(p, start, ArtRow { item: Item::Art(hash(&k)), key: k, opens: true, path: None, tag: None, open: false });
    }

    /// How many of `n` rows of list `key` show.
    fn shown(&self, key: &str, n: usize) -> usize {
        if self.app.art_toggled.contains(&format!("more:{key}")) {
            n
        } else {
            n.min(LIST_ROWS)
        }
    }
}

/// The worktrees of repo `name`.
pub fn worktrees<'a>(app: &'a App, name: &str) -> Vec<&'a Worktree> {
    app.worktrees.iter().filter(|w| w.repo == name).collect()
}

/// The task holding a worktree: an active task whose run says it is in it.
pub fn holder(app: &App, wt: &Worktree) -> Option<(String, String, String)> {
    for g in &app.forest.clusters {
        for t in &g.tasks {
            if t.state == TaskState::Active && t.worktree.as_deref() == Some(wt.path.as_str()) {
                return Some((t.written_id(), t.title.clone(), [t.elapsed.clone().unwrap_or_default(), t.account.clone().unwrap_or_default()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ")));
            }
        }
    }
    None
}

pub fn wt_state(app: &App, wt: &Worktree) -> WtState {
    if holder(app, wt).is_some() {
        WtState::Active
    } else if !wt.present {
        WtState::Missing
    } else if wt.ahead == 0 {
        WtState::Merged
    } else {
        WtState::Idle
    }
}

/// Whether an artifact's maker is running (a task, by its soup id `T-n`).
fn maker_running(app: &App, a: &Artifact) -> bool {
    let Some(n) = a.made_by.strip_prefix("T-").and_then(|n| n.parse::<u32>().ok()) else { return false };
    let Some(g) = app.forest.owner(crate::cauldron::model::Local::T(n)) else { return false };
    app.forest.task(g, n).is_some_and(|t| t.state == TaskState::Active)
}

pub fn artifact_state(app: &App, a: &Artifact) -> ArtState {
    art_state(a, maker_running(app, a))
}

/// The artifacts on this cluster's CREATED band: those `for` a node on a
/// cluster in scope, newest first.
fn created(app: &App) -> Vec<&Artifact> {
    let scope = app.scope();
    let mut v: Vec<&Artifact> = app.arts.artifacts.iter().filter(|a| a.cluster(&app.forest).is_some_and(|g| scope.contains(&g))).collect();
    v.sort_by(|a, b| b.created.cmp(&a.created));
    v
}

/// The open rule at first render (`designs::ARTIFACTS_OPEN`).
fn default_open(active: bool, repos_level: bool) -> bool {
    match designs::ARTIFACTS_OPEN {
        ArtifactsOpen::ActivePath => active,
        ArtifactsOpen::ReposOpen => repos_level,
        ArtifactsOpen::AllClosed => false,
    }
}

/// The Nodes tab at `width`.
pub fn page(app: &App, p: &mut Page, width: usize) {
    let a = A { app, w: width };
    let q = app.search.trim();
    if app.searching || !q.is_empty() {
        let style = if app.searching { palette::amber() } else { palette::muted() };
        let mut spans = vec![Span::styled("search ", style), Span::styled(app.search.clone(), palette::ink())];
        if app.searching {
            spans.push(Span::styled("▏", palette::amber()));
        }
        p.push(Line::from(spans));
    }
    regions_band(&a, p);
    created_band(&a, p);
    repos_band(&a, p);
    directories_band(&a, p);
    // What the clusters below claim and this one does not — regions and
    // repos — as one bare count, which → turns aggregate on (as on every tab).
    if !app.aggregate {
        let here = app.selected;
        let c = &app.corpus;
        let mine: Vec<usize> = app.repos.claims[here].iter().map(|c| c.repo).collect();
        let mut repos = std::collections::HashSet::new();
        let mut regions = std::collections::HashSet::new();
        for &g in &app.forest.subtree(here)[1..] {
            repos.extend(app.repos.claims[g].iter().filter(|c| !mine.contains(&c.repo)).map(|c| c.repo));
            regions.extend(c.claims[g].iter().filter(|r| !c.claims[here].contains(r)).copied());
        }
        let n = repos.len() + regions.len();
        if n > 0 {
            let start = p.begin();
            p.head();
            p.push(Line::from(muted(format!("{n} below"))));
            p.end_item(start, Item::Footer(crate::cauldron::app::Footer::Below));
        }
    }
}

/// REGIONS: the regions the cluster claims (under the search, those the
/// query reaches), each a band opening to its nodes and its artifacts.
fn regions_band(a: &A, p: &mut Page) {
    let app = a.app;
    let c = &app.corpus;
    let searching = !app.search.trim().is_empty();
    let hits = app.region_hits();
    let here = app.selected;
    let list = app.cluster_regions();
    let nodes: usize = hits.iter().map(|(r, n)| if searching { n.len() } else { c.members[*r].len() }).sum();
    let meta = look::meta([look::seg("", hits.len(), &format!("region{}", plural(hits.len())), Tone::Plain), look::seg("", nodes, &format!("node{}", plural(nodes)), Tone::Plain)]);
    if !a.band(p, 0, Item::Art(hash("regions")), "regions", bold("Regions"), None, meta, true, None) {
        return;
    }
    p.enter(Item::Art(hash("regions")));
    for (r, matched) in hits {
        let reg = &c.regions[r];
        let via = list.iter().find(|g| g.region == r).map(|g| g.via.clone()).unwrap_or_default();
        let others: Vec<String> = c.claimants[r].iter().copied().filter(|g| !via.contains(g) && *g != here).map(|g| app.forest.get(g).name.clone()).collect();
        let arts: Vec<&Artifact> = app.arts.artifacts.iter().filter(|x| x.regions.contains(&reg.id)).collect();
        let mut meta: Vec<Span<'static>> = region_rollup(app, r, arts.len()).split(" · ").map(|x| muted(x.to_string())).collect();
        if !others.is_empty() {
            meta.push(muted(format!("also {}", others.join(", "))));
        }
        let v = reg.schema_version;
        meta.push(look::word(format!("v{v}"), if v < c.schema_version { Tone::Warn } else { Tone::Plain }));
        let meta = look::meta(meta);
        let key = format!("region:{}", reg.id);
        // The handle crops so the band's metadata stands — never below its
        // id and first word: past that it is the metadata (its noun counts
        // long since msg 403) that crops.
        let mw: usize = meta.iter().map(sw).sum();
        let room = a.w.saturating_sub(mw + 2 + 4 + 2).max(16);
        let label = Span::styled(crop(&format!("{:<7}{}", reg.id, reg.shown_handle()), room), palette::ink());
        if !a.band(p, 1, Item::Region(r), &key, label, None, meta, searching, Some(Tag::thing("region", &reg.id))) {
            continue;
        }
        p.enter(Item::Region(r));
        // Its boundary, in his words, then what narrows it — part of the
        // region's band, never a row of its own.
        for row in crate::cauldron::view::wrap_plain(&reg.boundary, a.w.saturating_sub(6).max(10)) {
            p.push(Line::from(vec![Span::raw("      "), Span::styled(row, palette::ink2())]));
        }
        let up: Vec<&str> = reg.narrows.iter().map(|&x| c.regions[x].id.as_str()).collect();
        if !up.is_empty() {
            p.push(Line::from(vec![Span::raw("      "), muted(format!("↑ {}", up.join(" ")))]));
        }
        if let Some(rg) = p.items.last_mut() {
            rg.end = p.lines.len();
        }
        let shown: Vec<usize> = if searching { matched } else { c.members[r].clone() };
        for n in shown {
            node_row(a, p, n, 2);
        }
        if !searching {
            let lkey = format!("rarts:{}", reg.id);
            let k = a.shown(&lkey, arts.len());
            for x in &arts[..k] {
                // A card sits two columns in from its band's title (V1;
                // 0217 B5), as on every other page.
                artifact_row_at(a, p, x, 3);
            }
            if k < arts.len() {
                a.more(p, 2, &lkey, arts.len() - k);
            }
        }
        p.leave();
    }
    p.leave();
}

/// A region's rollup: ready/active tasks, open questions and issues, nodes,
/// artifacts — zero counts left out.
fn region_rollup(app: &App, r: usize, arts: usize) -> String {
    let c = &app.corpus;
    let members: Vec<&crate::cauldron::corpus::Node> = c.members[r].iter().map(|&n| &c.nodes[n]).collect();
    let count = |k: &str, states: &[&str]| members.iter().filter(|n| n.kind == k && (states.is_empty() || states.contains(&n.state.as_str()))).count();
    let mut parts = Vec::new();
    for (n, word) in [(count("task", &["ready", "active"]), "task"), (count("question", &["open"]), "question"), (count("issue", &["open"]), "issue")] {
        if n > 0 {
            parts.push(format!("{n} {word}{}", plural(n)));
        }
    }
    parts.push(format!("{} node{}", members.len(), plural(members.len())));
    if arts > 0 {
        parts.push(look::noun(arts, "artifact"));
    }
    parts.join(" · ")
}

/// A soup node: ONE line — id · kind · state · handle — and → opens its
/// body in place (its trace, then its text).
/// A soup node as a CARD (msg 186: "Everything in the soup will be a node
/// ... get everything to match the question card"): its top border is its
/// band — id · kind · handle, its state the legend and its border's colour —
/// and → opens its body inside it (its trace, then its text).
fn node_row(a: &A, p: &mut Page, n: usize, depth: usize) {
    let app = a.app;
    let node = &app.corpus.nodes[n];
    let key = format!("node:{}", node.id);
    let open = a.open(&key, false);
    let start = p.begin();
    let ind = 2 * depth;
    let cw = a.w.saturating_sub(ind);
    let border = look::status_border(&node.state);
    // As the question card: its id the legend, its kind and state at the
    // top left, its handle inside.
    let name = vec![Span::styled(node.kind.clone(), palette::ink2()), muted(" · "), Span::styled(node.state.clone(), border)];
    p.push(look::card_band_top(Card::Node, open, name, vec![Span::styled(node.id.clone(), palette::ink2())], border, cw));
    let body = p.begin();
    let iw = look::inside(cw);
    crate::cauldron::view::wrap(&mut p.lines, &node.handle, iw, 0, palette::ink());
    if open {
        let regions: Vec<String> = node.in_region.iter().map(|&r| app.corpus.regions[r].id.clone()).collect();
        let mut parts: Vec<String> = node.cite.iter().cloned().collect();
        parts.push(regions.join(" "));
        p.push(fit(vec![muted(format!("↑ {}", parts.join(" · ")))], iw));
        for row in crate::cauldron::view::wrap_plain(&node.body, iw) {
            p.push(Line::from(Span::styled(row, palette::ink2())));
        }
    }
    look::card_close(Card::Node, &mut p.lines, body, border, cw);
    look::indent(&mut p.lines, start, ind);
    a.end(p, start, ArtRow { item: Item::Node(n), key, opens: true, path: None, tag: Some(Tag::thing("node", &node.id)), open });
}

fn repos_band(a: &A, p: &mut Page) {
    let app = a.app;
    let repos = app.cluster_repos();
    let active: usize = repos.iter().map(|r| worktrees(app, &app.repos.repos[r.repo].name).into_iter().filter(|w| wt_state(app, w) == WtState::Active).count()).sum();
    let total: usize = repos.iter().map(|r| worktrees(app, &app.repos.repos[r.repo].name).len()).sum();
    let meta = if active > 0 { format!("{} · {active} active worktree{}", look::noun(repos.len(), "repo"), plural(active)) } else { format!("{} · {total} worktree{}", look::noun(repos.len(), "repo"), plural(total)) };
    if !a.band(p, 0, Item::Art(hash("repos")), "repos", bold("Repos"), None, vec![muted(meta)], default_open(active > 0, true), None) {
        return;
    }
    p.enter(Item::Art(hash("repos")));
    for gr in repos {
        let repo = &app.repos.repos[gr.repo];
        let wts = worktrees(app, &repo.name);
        let active = wts.iter().any(|w| wt_state(app, w) == WtState::Active);
        let mut meta = vec![muted(format!("{} · {} worktree{} · {}", repo.at_word(), wts.len(), plural(wts.len()), repo.bp_word()))];
        for (k, g, st) in [("violation", "●", palette::amber()), ("drag", "◇", palette::ink2()), ("migration", "◌", palette::ink2())] {
            if !repo.live(k).is_empty() {
                meta.push(Span::styled(format!(" {g}"), st));
            }
        }
        let key = format!("repo:{}", repo.name);
        let tag = Some(Tag::thing("repo", &repo.name));
        if !a.band(p, 1, Item::Repo(gr.repo), &key, Span::styled(repo.name.clone(), palette::ink().add_modifier(Modifier::BOLD)), Some(&repo.path), meta, default_open(active, true), tag) {
            continue;
        }
        p.enter(Item::Repo(gr.repo));
        worktrees_band(a, p, &repo.name, &wts);
        blueprint_band(a, p, gr.repo);
        files_band(a, p, 2, &format!("files:{}", repo.path), &repo.path, Some(&repo.name));
        p.leave();
    }
    p.leave();
}

fn worktrees_band(a: &A, p: &mut Page, repo: &str, wts: &[&Worktree]) {
    let app = a.app;
    let by = |s: WtState| wts.iter().copied().filter(|w| wt_state(app, w) == s).collect::<Vec<_>>();
    let n = |s: WtState| by(s).len();
    let meta = format!("{} · {} active · {} idle · {} merged · {} missing", look::noun(wts.len(), "worktree"), n(WtState::Active), n(WtState::Idle), n(WtState::Merged), n(WtState::Missing));
    let key = format!("wts:{repo}");
    if !a.band(p, 2, Item::Art(hash(&key)), &key, bold("Worktrees"), None, vec![muted(meta)], default_open(n(WtState::Active) > 0, true), None) {
        return;
    }
    p.enter(Item::Art(hash(&key)));
    for st in WtState::ALL {
        let list = by(st);
        let bkey = format!("wts:{repo}:{}", st.word());
        // An ACTIVE band opens whenever it holds something (0146 open_rule).
        let def = st == WtState::Active && !list.is_empty() && designs::ARTIFACTS_OPEN != ArtifactsOpen::AllClosed;
        if !a.band(p, 3, Item::Art(hash(&bkey)), &bkey, bold(look::sentence(st.word())), None, vec![muted(look::noun(list.len(), "worktree"))], def, None) {
            continue;
        }
        p.enter(Item::Art(hash(&bkey)));
        let k = a.shown(&bkey, list.len());
        for wt in &list[..k] {
            worktree_row(a, p, wt, st);
        }
        if k < list.len() {
            a.more(p, 4, &bkey, list.len() - k);
        }
        p.leave();
    }
    p.leave();
}

/// A worktree: ONE line, the same columns on every row — glyph · name ↗ ·
/// branch · ahead · changed · age · holder — and → opens its detail rows in
/// place.
fn worktree_row(a: &A, p: &mut Page, wt: &Worktree, st: WtState) {
    let app = a.app;
    let key = format!("wt:{}", wt.path);
    let open = a.open(&key, false);
    let start = p.begin();
    let ind = "  ".repeat(4);
    let gst = match st {
        WtState::Active => palette::code(),
        WtState::Idle => palette::ink(),
        _ => palette::muted(),
    };
    let name = col(wt.name(), 24);
    let at = display_width(&ind) + 2;
    a.hotspots(p, &wt.path, at, display_width(name.trim_end()), at + 25);
    let branch = wt.branch.clone().unwrap_or_else(|| "(detached)".into());
    let held = holder(app, wt);
    let age_w = wt.last_commit.as_ref().map_or(String::new(), |c| age(&c.at));
    let name_style = if app.flash.as_deref() == Some(wt.path.as_str()) { palette::green() } else { palette::ink() };
    let mut spans = vec![
        Span::raw(ind.clone()),
        Span::styled(format!("{} ", st.glyph()), gst),
        Span::styled(name, name_style),
        Span::styled(format!(" {OPEN_ICON} "), palette::code()),
        Span::styled(col(&branch, 22), palette::ink2()),
        Span::styled(format!("{:>4}", format!("+{}", wt.ahead)), palette::ink2()),
        Span::styled(format!("{:>5}", if wt.dirty > 0 { format!("±{}", wt.dirty) } else { String::new() }), palette::amber()),
        muted(format!("{:>5}", age_w)),
    ];
    if let Some((t, _, _)) = &held {
        spans.push(Span::styled(format!("  {t}"), palette::code()));
    }
    p.push(fit(spans, a.w));
    // Opened, a TABLE of its fields (msg 186).
    if open {
        let d = display_width(&ind) + 2;
        let kw = 11;
        let shown = tilde(&wt.path);
        a.hotspots(p, &wt.path, d + kw + 3, display_width(&shown), d + kw + 3 + display_width(&shown) + 1);
        let ink2 = |s: String| Span::styled(s, palette::ink2());
        let base = wt.base.as_deref().map_or("—".to_string(), |b| b.chars().take(8).collect());
        let mut rows = vec![
            ("path".to_string(), vec![Span::styled(shown, palette::code()), Span::styled(format!(" {OPEN_ICON}"), palette::code())]),
            ("head".to_string(), vec![ink2(wt.head.chars().take(8).collect())]),
            ("base".to_string(), vec![ink2(base)]),
            ("ahead".to_string(), vec![ink2(wt.ahead.to_string())]),
            ("behind main".to_string(), vec![ink2(wt.behind.to_string())]),
            ("changed".to_string(), vec![Span::styled(if wt.dirty == 0 { "clean".to_string() } else { wt.dirty.to_string() }, if wt.dirty > 0 { palette::amber() } else { palette::ink2() })]),
        ];
        if let Some((t, title, run)) = &held {
            rows.push(("held by".into(), vec![bold(format!("{t} ")), Span::styled(title.clone(), palette::ink()), muted(format!("  ▶ {run}"))]));
        }
        if let Some(c) = &wt.last_commit {
            rows.push(("last commit".into(), vec![muted(format!("{}  ", age(&c.at))), ink2(format!("\"{}\"", c.subject))]));
        }
        if !wt.present {
            rows.push(("directory".into(), vec![Span::styled("gone; git still lists it".to_string(), palette::amber())]));
        }
        for art in app.arts.artifacts.iter().filter(|x| x.location.starts_with(&format!("{}/", wt.path))) {
            let st = artifact_state(app, art);
            rows.push(("artifact".into(), vec![Span::styled(format!("{} ", st.glyph()), palette::ink2()), Span::styled(art.name().to_string(), palette::ink()), muted(format!("  {}", art.kind))]));
        }
        let rows: Vec<(String, Vec<Span<'static>>)> = rows.into_iter().map(|(k, v)| (format!("{k:<kw$}"), v)).collect();
        for l in look::table(rows, d, a.w) {
            p.push(l);
        }
    }
    a.end(p, start, ArtRow { item: Item::Art(hash(&key)), key, opens: true, path: Some(wt.path.clone()), tag: Some(Tag::thing("node", &wt.id)), open });
}

/// The 0071 repo page, nested and cut into bands: MODULES and the secondary
/// sections, each a band; a module row is unchanged and → on one still
/// opens its page.
fn blueprint_band(a: &A, p: &mut Page, r: usize) {
    let app = a.app;
    let repo = &app.repos.repos[r];
    let key = format!("bp:{}", repo.name);
    let mut meta = vec![muted(format!("{} · adopted {} · {} module{}", repo.bp_word().trim_start_matches("bp "), if repo.adopted.is_empty() { "—" } else { &repo.adopted }, repo.count("module"), plural(repo.count("module"))))];
    if let Some(pct) = repo.linked_pct() {
        meta.push(muted(format!(" · {pct}% linked")));
    }
    for (k, g, st) in [("violation", "●", palette::amber()), ("drag", "◇", palette::ink2()), ("migration", "◌", palette::ink2())] {
        let n = repo.live(k).len();
        if n > 0 {
            meta.push(Span::styled(format!(" · {g} {}", look::noun(n, if k == "migration" { "undescribed migration" } else { k })), st));
        }
    }
    if !a.band(p, 2, Item::Art(hash(&key)), &key, bold("Blueprint"), None, meta, false, None) {
        return;
    }
    p.enter(Item::Art(hash(&key)));
    let mods = repo.top_modules();
    let mkey = format!("{key}:modules");
    if a.band(p, 3, Item::Art(hash(&mkey)), &mkey, bold("Modules"), None, vec![muted(look::noun(repo.count("module"), "module"))], false, None) {
        p.enter(Item::Art(hash(&mkey)));
        for m in mods {
            crate::cauldron::pages::module_rows(app, p, r, m, 4, a.w);
        }
        p.leave();
    }
    for (word, kind, glyphs) in [("Violations", "violation", true), ("Drag", "drag", true), ("Contradictions", "contradiction", true), ("Migrations", "migration", false), ("Capabilities", "capability", false), ("Schemas", "schema", false), ("Events", "event", false)] {
        let ids: Vec<usize> = if glyphs { repo.live(kind) } else if kind == "migration" { repo.migrations() } else { repo.of_kind(kind).collect() };
        if ids.is_empty() {
            continue;
        }
        let skey = format!("{key}:{kind}");
        let meta = if kind == "migration" { format!("{} · ◌ {} undescribed", look::noun(ids.len(), kind), repo.live("migration").len()) } else { look::noun(ids.len(), kind) };
        if !a.band(p, 3, Item::Art(hash(&skey)), &skey, bold(word), None, vec![muted(meta)], false, None) {
            continue;
        }
        p.enter(Item::Art(hash(&skey)));
        for i in ids {
            crate::cauldron::pages::secondary_entry(app, p, r, i, 4, a.w);
        }
        p.leave();
    }
    p.leave();
}

/// A FILES band: the directory at `path`, listed lazily — a row per entry,
/// directories opening in place; a repo root carries the virtual
/// `worktrees/` holding its linked worktrees.
fn files_band(a: &A, p: &mut Page, depth: usize, key: &str, path: &str, repo: Option<&str>) {
    let entries = list_dir(path);
    let newest = entries.iter().map(|e| e.age_s).min();
    let meta = format!("{} entr{}{}", entries.len(), if entries.len() == 1 { "y" } else { "ies" }, newest.map_or(String::new(), |s| format!(" · {}", short_age(s))));
    if !a.band(p, depth, Item::Art(hash(key)), key, bold("Files"), Some(path), vec![muted(meta)], false, None) {
        return;
    }
    p.enter(Item::Art(hash(key)));
    dir_rows(a, p, depth + 1, path, repo, entries);
    p.leave();
}

fn dir_rows(a: &A, p: &mut Page, depth: usize, path: &str, repo: Option<&str>, entries: Vec<artifacts::DirEntry>) {
    let app = a.app;
    let lkey = format!("ls:{path}");
    let dirs: Vec<&artifacts::DirEntry> = entries.iter().filter(|e| e.dir).collect();
    let files: Vec<&artifacts::DirEntry> = entries.iter().filter(|e| !e.dir).collect();
    let all: Vec<&artifacts::DirEntry> = dirs.into_iter().chain(files).collect();
    let k = a.shown(&lkey, all.len());
    let mut virtual_done = false;
    for e in &all[..k] {
        // The virtual `worktrees/` sits after the real directories.
        if !e.dir && !virtual_done {
            virtual_done = true;
            if let Some(r) = repo {
                worktrees_dir(a, p, depth, r);
            }
        }
        if e.dir {
            let key = format!("dir:{}", e.path);
            let n = Item::Art(hash(&key));
            let arts_here = app.arts.artifacts.iter().filter(|x| x.location.starts_with(&format!("{}/", e.path))).count();
            let mut meta = Vec::new();
            if arts_here > 0 {
                meta.push(Span::styled(format!("◈ {} · ", look::noun(arts_here, "artifact")), palette::ink2()));
            }
            meta.push(muted(format!("{} · {}", look::noun(e.size, "entry"), short_age(e.age_s))));
            if a.band(p, depth, n, &key, Span::styled(format!("{}/", e.name), palette::ink()), None, meta, false, Some(Tag::thing("path", &tilde(&e.path)))) {
                p.enter(n);
                dir_rows(a, p, depth + 1, &e.path, None, list_dir(&e.path));
                p.leave();
            }
        } else {
            file_row(a, p, depth, &e.name, &e.path, &format!("{} · {}", size_word(e.size), short_age(e.age_s)));
        }
    }
    if !virtual_done {
        if let Some(r) = repo {
            worktrees_dir(a, p, depth, r);
        }
    }
    if k < all.len() {
        a.more(p, depth, &lkey, all.len() - k);
    }
}

fn file_row(a: &A, p: &mut Page, depth: usize, name: &str, path: &str, meta: &str) {
    let start = p.begin();
    let ind = "  ".repeat(depth) + "  ";
    let at = display_width(&ind);
    a.hotspots(p, path, at, display_width(name), at + display_width(name) + 1);
    let style = if a.app.flash.as_deref() == Some(path) { palette::green() } else { palette::ink() };
    let left = vec![Span::raw(ind), Span::styled(name.to_string(), style), Span::styled(format!(" {OPEN_ICON}"), palette::code())];
    let lw: usize = left.iter().map(sw).sum();
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(a.w.saturating_sub(lw + display_width(meta)))));
    spans.push(muted(meta.to_string()));
    p.push(fit(spans, a.w));
    let key = format!("file:{path}");
    a.end(p, start, ArtRow { item: Item::Art(hash(&key)), key, opens: false, path: Some(path.to_string()), tag: Some(Tag::thing("path", &tilde(path))), open: false });
}

/// A repo root's virtual `worktrees/`: its linked worktrees, each a
/// directory opening to its files.
fn worktrees_dir(a: &A, p: &mut Page, depth: usize, repo: &str) {
    let app = a.app;
    let wts = worktrees(app, repo);
    if wts.is_empty() {
        return;
    }
    let key = format!("vwt:{repo}:{depth}");
    let n = Item::Art(hash(&key));
    if !a.band(p, depth, n, &key, Span::styled("worktrees/", palette::ink()), None, vec![muted(look::noun(wts.len(), "worktree"))], false, None) {
        return;
    }
    p.enter(n);
    let mut list = wts.clone();
    list.sort_by_key(|w| (wt_state(app, w) != WtState::Active, w.name().to_string()));
    let k = a.shown(&key, list.len());
    for wt in &list[..k] {
        let dkey = format!("dir:{}:{depth}", wt.path);
        let held = holder(app, wt).map(|(t, ..)| t);
        let meta = match held {
            Some(t) => vec![Span::styled(format!("▶ {t}"), palette::code())],
            None => vec![muted(wt_state(app, wt).word().to_lowercase())],
        };
        let dn = Item::Art(hash(&dkey));
        if a.band(p, depth + 1, dn, &dkey, Span::styled(format!("{}/", wt.name()), palette::ink()), Some(&wt.path), meta, false, Some(Tag::thing("node", &wt.id))) {
            p.enter(dn);
            dir_rows(a, p, depth + 2, &wt.path, None, list_dir(&wt.path));
            p.leave();
        }
    }
    if k < list.len() {
        a.more(p, depth + 1, &key, list.len() - k);
    }
    p.leave();
}

fn directories_band(a: &A, p: &mut Page) {
    let app = a.app;
    let scope: Vec<String> = app.scope().iter().map(|&g| app.forest.get(g).key.clone()).collect();
    let dirs: Vec<_> = app.arts.directories.iter().filter(|d| d.claimed_by.iter().any(|c| scope.contains(c))).collect();
    if !a.band(p, 0, Item::Art(hash("directories")), "directories", bold("Directories"), None, vec![muted(look::noun(dirs.len(), "directory"))], false, None) {
        return;
    }
    p.enter(Item::Art(hash("directories")));
    for d in dirs {
        let entries = list_dir(&d.path);
        let newest = entries.iter().map(|e| e.age_s).min();
        let key = format!("dirroot:{}", d.path);
        let n = Item::Art(hash(&key));
        let meta = format!("{} entr{}{}", entries.len(), if entries.len() == 1 { "y" } else { "ies" }, newest.map_or(String::new(), |s| format!(" · {}", short_age(s))));
        if a.band(p, 1, n, &key, Span::styled(d.name.clone(), palette::ink().add_modifier(Modifier::BOLD)), Some(&d.path), vec![muted(meta)], false, Some(Tag::thing("path", &tilde(&d.path)))) {
            p.enter(n);
            dir_rows(a, p, 2, &d.path, None, entries);
            p.leave();
        }
    }
    p.leave();
}

fn created_band(a: &A, p: &mut Page) {
    let app = a.app;
    let arts = created(app);
    let writing = arts.iter().filter(|x| artifact_state(app, x) == ArtState::BeingWritten).count();
    let mut meta = vec![muted(look::noun(arts.len(), "artifact"))];
    if writing > 0 {
        meta.push(Span::styled(format!(" · {writing} being written"), palette::code()));
    }
    if !a.band(p, 0, Item::Art(hash("created")), "created", bold("Artifacts"), None, meta, default_open(writing > 0, false), None) {
        return;
    }
    p.enter(Item::Art(hash("created")));
    // One band per kind present, by count; MISSING last.
    let mut kinds: Vec<(String, usize)> = Vec::new();
    for x in arts.iter().filter(|x| artifact_state(app, x) != ArtState::Missing) {
        match kinds.iter_mut().find(|(k, _)| *k == x.kind) {
            Some((_, n)) => *n += 1,
            None => kinds.push((x.kind.clone(), 1)),
        }
    }
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut bands: Vec<(String, Vec<&Artifact>)> = kinds.into_iter().map(|(k, _)| (k.clone(), arts.iter().copied().filter(|x| x.kind == k && artifact_state(app, x) != ArtState::Missing).collect())).collect();
    let missing: Vec<&Artifact> = arts.iter().copied().filter(|x| artifact_state(app, x) == ArtState::Missing).collect();
    if !missing.is_empty() {
        bands.push(("missing".into(), missing));
    }
    for (kind, list) in bands {
        let bkey = format!("created:{kind}");
        let writing = list.iter().filter(|x| artifact_state(app, x) == ArtState::BeingWritten).count();
        // Its kind's noun (msg 403): `22 reports`; the missing are artifacts.
        let mut meta = vec![muted(look::noun(list.len(), if kind == "missing" { "artifact" } else { &kind }))];
        if writing > 0 {
            meta.push(Span::styled(format!(" · {writing} being written"), palette::code()));
        }
        let n = Item::Art(hash(&bkey));
        let word = if kind == "missing" { "Missing".to_string() } else { kind_band(&kind) };
        if !a.band(p, 1, n, &bkey, bold(word), None, meta, default_open(writing > 0, false), None) {
            continue;
        }
        p.enter(n);
        let k = a.shown(&bkey, list.len());
        for x in &list[..k] {
            artifact_row(a, p, x);
        }
        if k < list.len() {
            a.more(p, 2, &bkey, list.len() - k);
        }
        p.leave();
    }
    p.leave();
}

/// An artifact: ONE line — glyph · name ↗ · its handle · who made it and
/// when — and → opens its detail rows in place.
fn artifact_row(a: &A, p: &mut Page, x: &Artifact) {
    artifact_row_at(a, p, x, 3);
}

/// [`artifact_row`] at `depth` (inside its kind's band, or a region, whose
/// title is at `depth - 1`, so the card is at its title + 2): the
/// ARTIFACT CARD (msg 186: "one card type for artifacts") — square corners,
/// its top border its band: `┌─ ▸ ◇ name ↗  handle ──── maker age ─┐`, the
/// border its state's colour. → opens its details inside it, as a TABLE.
fn artifact_row_at(a: &A, p: &mut Page, x: &Artifact, depth: usize) {
    let app = a.app;
    let key = format!("art:{}", x.id);
    let open = a.open(&key, false);
    let st = artifact_state(app, x);
    let start = p.begin();
    let ind = 2 * depth;
    let cw = a.w.saturating_sub(ind);
    let (gst, border) = match st {
        ArtState::BeingWritten => (palette::code(), palette::code()),
        ArtState::Missing => (palette::muted(), palette::red()),
        ArtState::Stale => (palette::amber(), palette::amber()),
        ArtState::Present => (palette::ink2(), palette::muted()),
    };
    let maker = x.made_by.rsplit('/').next().unwrap_or(&x.made_by).to_string();
    let right = vec![muted(format!("{maker} {}", age(&x.created)))];
    let name_style = if app.flash.as_deref() == Some(x.location.as_str()) { palette::green() } else { palette::ink().add_modifier(Modifier::BOLD) };
    let name = x.name().to_string();
    // `┌─ ` then the chevron and the glyph: the name's copy target and its `↗`.
    let at = ind + 3 + 2 + 2;
    a.hotspots(p, &x.location, at, display_width(&name), at + display_width(&name) + 1);
    let left = vec![Span::styled(format!("{} ", st.glyph()), gst), Span::styled(name, name_style), Span::styled(format!(" {OPEN_ICON}"), palette::code())];
    p.push(look::card_band_top(Card::Artifact, open, left, right, border, cw));
    let body = p.begin();
    let iw = look::inside(cw);
    crate::cauldron::view::wrap(&mut p.lines, &x.handle, iw, 0, palette::ink2());
    if open {
        // The path, left-cropped so the file name always shows.
        let full = tilde(&x.location);
        let kw = 8;
        let room = iw.saturating_sub(kw + 3 + 2);
        let shown = if display_width(&full) > room { format!("…{}", full.chars().rev().take(room.saturating_sub(1)).collect::<Vec<_>>().into_iter().rev().collect::<String>()) } else { full };
        // `│ ` then the table's key column and its ` │ `.
        let pc = ind + 2 + kw + 3;
        a.hotspots(p, &x.location, pc, display_width(&shown), pc + display_width(&shown) + 1);
        let size = x.verified.as_ref().map_or("—".to_string(), |v| size_word(v.bytes));
        let state = match st {
            ArtState::BeingWritten => "being written",
            ArtState::Present => "present",
            ArtState::Stale => "changed since it was verified",
            ArtState::Missing => "missing",
        };
        // What it IS (his Q11): a soup node wrapping a file.
        let regions = if x.regions.is_empty() { "no region".to_string() } else { x.regions.join(" ") };
        // Where it sits, computed from its path.
        let wt = app.worktrees.iter().filter(|w| x.location.starts_with(&format!("{}/", w.path))).max_by_key(|w| w.path.len());
        let repo = app.repos.repos.iter().filter(|r| x.location.starts_with(&format!("{}/", r.path))).max_by_key(|r| r.path.len());
        let place = match (wt, repo) {
            (Some(w), _) => format!("{} · worktree {}", w.repo, w.name()),
            (None, Some(r)) => format!("{} · {}", r.name, if r.branch.is_empty() { "—" } else { &r.branch }),
            (None, None) => "no registered repo".into(),
        };
        let verified = x.verified.as_ref().map_or("not verified".to_string(), |v| format!("{} ago", age(&v.at)));
        let ink2 = |s: String| Span::styled(s, palette::ink2());
        let mut rows = vec![
            ("path".to_string(), vec![Span::styled(shown, palette::code()), Span::styled(format!(" {OPEN_ICON}"), palette::code())]),
            ("node".to_string(), vec![ink2(format!("{} node {} · wraps a file", designs::ARTIFACT_HOME, x.id))]),
            ("regions".to_string(), vec![ink2(regions)]),
            ("kind".to_string(), vec![ink2(x.kind.clone())]),
            ("size".to_string(), vec![ink2(size)]),
            ("state".to_string(), vec![Span::styled(state.to_string(), gst)]),
            ("written".to_string(), vec![ink2(format!("{} ago by {}", age(&x.created), x.made_by))]),
            ("for".to_string(), vec![ink2(x.for_.clone())]),
            ("in".to_string(), vec![ink2(place)]),
            ("verified".to_string(), vec![ink2(verified)]),
        ];
        if let Some(sup) = &x.supersedes {
            rows.push(("replaces".to_string(), vec![ink2(sup.clone())]));
        }
        let rows: Vec<(String, Vec<Span<'static>>)> = rows.into_iter().map(|(k, v)| (format!("{k:<kw$}"), v)).collect();
        for l in look::table(rows, 0, iw) {
            p.push(l);
        }
    }
    look::card_close(Card::Artifact, &mut p.lines, body, border, cw);
    look::indent(&mut p.lines, start, ind);
    a.end(p, start, ArtRow { item: Item::Art(hash(&key)), key, opens: true, path: Some(x.location.clone()), tag: Some(Tag::thing("node", &x.id)), open });
}

/// Spans cropped to `width`.
fn fit(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    crate::cauldron::view::fit(spans, width)
}
