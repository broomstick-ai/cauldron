//! Drawing. Every colour and style comes from the harness face's palette
//! (`tui_render::palette`), and every wrapped paragraph from its markdown
//! layout (`tui_render::md::lay`), so this face reads as the same family.
//!
//! The view also RECORDS where it drew each clickable thing (`App::hits`), so a
//! mouse click is resolved against exactly the frame he is looking at.

use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use tui_render::md;
use tui_render::palette;

use crate::cauldron::app::{App, CTab, Focus, Hit, KTab, Modal, Title, KEYMAP};
use crate::cauldron::menu::{Add, Entry, Flat};
use crate::cauldron::settings::{show, Ty};
use crate::cauldron::look;
use crate::cauldron::tags;

/// The divider's grabber: one glyph, at the middle of the line.
pub const GRABBER: &str = "⣿";

pub fn draw(frame: &mut Frame, app: &mut App) {
    // Every colour below is read from the theme in force, so setting it here,
    // each frame, is what makes the switch live.
    palette::set_theme(app.theme());
    app.hits.clear();
    let area = frame.area();
    // V1.1: two tab bars, the global one on top and the environment's
    // under it.
    let [top, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    frame.render_widget(Block::new().style(palette::base()), area);
    if app.first_run.is_some() {
        draw_first_run(frame, app, area);
        if app.modal.is_some() {
            draw_modal(frame, app, area);
        }
        return;
    }
    draw_top(frame, app, top);
    match app.ctab {
        CTab::Cauldron => draw_surface(frame, app, body),
        CTab::Threads => draw_threads(frame, app, body),
        _ => draw_dash(frame, app, body),
    }
    if app.dropdown.is_some() {
        draw_dropdown(frame, app, area);
    }
    if app.modal.is_some() {
        draw_modal(frame, app, area);
    }
}

/// The words of the first-run screen's one action (V1.2: his "create your
/// first environment", in the face's word for one).
pub const FIRST_RUN_ACTION: &str = "+ create your first cauldron";

/// **THE FIRST-RUN SCREEN** (V1.2): no environment exists, so there is no
/// name for the top left and no tab to show — the frame is empty but for
/// one centred action, selected. No sentence, no logo, no welcome (§1).
fn draw_first_run(frame: &mut Frame, app: &mut App, area: Rect) {
    let label = format!(" {FIRST_RUN_ACTION} ");
    let w = (display_width(&label) as u16).min(area.width);
    let at = Rect { x: area.x + (area.width - w) / 2, y: area.y + area.height / 2, width: w, height: 1.min(area.height) };
    let style = palette::code().add_modifier(Modifier::BOLD).bg(palette::c().sel_bg);
    frame.render_widget(Paragraph::new(Line::from(Span::styled(label, style))), at);
    app.first_run_at = at;
}

/// **THE TWO TAB BARS** (V1.1; release plan R-V1 item 1, product spec
/// §12: *"two tab bars like on top of each other ... different colors, like
/// the global above and then the environment below"*). Row 0 is the GLOBAL
/// bar — what was Config: TUI · Devices · Providers · Accounts — on
/// [`look::global_bar_bg`]; row 1 the ENVIRONMENT bar on [`look::env_bar_bg`]:
/// the environment's dropdown at its left (it scopes that bar), then
/// Cauldron · Threads · Status · Permissions · Settings. No caption names
/// either bar, no tab carries a number, and exactly one tab of the two bars
/// is lit: the one shown.
fn draw_top(frame: &mut Frame, app: &mut App, area: Rect) {
    let [global, envr] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    draw_global_bar(frame, app, global);
    draw_env_bar(frame, app, envr);
}

/// The tabs `tabs` flush right in `r` on `bg`, `sep_w` 3 (` | `) or 1
/// (`|`), each recorded as its click target.
fn draw_tabs(frame: &mut Frame, app: &mut App, r: Rect, tabs: &[CTab], labels: &[String], sep_w: usize, bg: ratatui::style::Color) {
    let sep = || Span::styled(if sep_w == 3 { format!(" {} ", look::STRIP_SEP) } else { look::STRIP_SEP.to_string() }, palette::muted().bg(bg));
    let mut x = r.x;
    let mut spans = Vec::new();
    for (i, t) in tabs.iter().copied().enumerate() {
        if i > 0 {
            spans.push(sep());
            x += sep_w as u16;
        }
        let style = if t == app.ctab {
            palette::ink().add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            palette::muted()
        };
        let label = labels[i].clone();
        let w = display_width(&label) as u16;
        spans.push(Span::styled(label, style.bg(bg)));
        if x < r.right() {
            app.hits.push((Rect { x, y: r.y, width: w.min(r.right() - x), height: 1 }, Hit::CTab(t)));
        }
        x += w;
    }
    frame.render_widget(Paragraph::new(Line::from(spans)).style(Style::new().bg(bg)), r);
}

/// Row 0: the global bar, its tabs flush right as the environment bar's
/// are, roomy (` | `, a column of margin) while that fits, else tight.
fn draw_global_bar(frame: &mut Frame, app: &mut App, area: Rect) {
    let bg = look::global_bar_bg();
    frame.render_widget(Block::new().style(Style::new().bg(bg)), area);
    let labels: Vec<String> = CTab::GLOBAL.iter().map(|t| app.named(t.label())).collect();
    let words_w = labels.iter().map(|l| display_width(l)).sum::<usize>();
    let gaps = labels.len() - 1;
    let roomy = words_w + 3 * gaps + 1;
    let (sep_w, tabs_w) = if roomy <= area.width as usize { (3usize, roomy) } else { (1, words_w + gaps) };
    let rw = (tabs_w as u16).min(area.width);
    let r = Rect { x: area.right().saturating_sub(rw), width: rw, ..area };
    draw_tabs(frame, app, r, &CTab::GLOBAL, &labels, sep_w, bg);
}

/// Row 1: the environment bar. At its left the ENVIRONMENT, and nothing
/// before it — the cauldron's name as the cauldrons dropdown (msg 165) —
/// then its tabs on the right, no numbers. The product's name that once led
/// it (msg 235: "Let's turn it back to just the name of the product,
/// cauldron") is gone: 8e4fca10 msg 457, "Let's remove the product name from
/// the top left and just have the environment up there at the top left."
fn draw_env_bar(frame: &mut Frame, app: &mut App, area: Rect) {
    let bg = look::env_bar_bg();
    frame.render_widget(Block::new().style(Style::new().bg(bg)), area);
    // Menu items are parted by a `|` (b964d571 msg 23), as on every strip.
    let env = app.env.name.clone();
    let labels: Vec<String> = CTab::ENV.iter().map(|t| app.named(t.label())).collect();
    let words_w = labels.iter().map(|l| display_width(l)).sum::<usize>();
    let gaps = labels.len() - 1;
    // The tabs NEVER crop while they can fit (re-audit 10 D8): roomy — ` | `
    // between them, a column of margin on the right, a column's gap after
    // the dropdown — while that fits beside the whole dropdown; else tight —
    // `|`, flush right, no gap; and where even tight does not fit, the
    // environment's NAME crops, never a tab.
    let full = format!(" {env} ");
    let roomy = words_w + 3 * gaps + 1;
    let fits = display_width(&full) + 3 + 1 + roomy <= area.width as usize;
    let (sep_w, tabs_w, gap) = if fits { (3usize, roomy, 1u16) } else { (1, words_w + gaps, 0) };
    // What the tabs, the ` ▾ ` and the name's own two spaces leave it.
    let room = (area.width as usize).saturating_sub(tabs_w + 3 + gap as usize + 2);
    let name = if display_width(&env) <= room { full } else { format!(" {} ", crop(&env, room.max(1))) };
    // One column of margin, as the tabs keep on the right, while it fits.
    let lead: u16 = u16::from(display_width(&name) + 3 + 1 + gap as usize + tabs_w <= area.width as usize);
    let mut left = vec![Span::styled(" ".repeat(lead as usize), Style::new().bg(bg))];
    left.push(Span::styled(name.clone(), palette::ink().add_modifier(Modifier::BOLD).bg(palette::c().code_bg)));
    left.push(Span::styled(" ▾ ", palette::ink().bg(palette::c().chevron_bg)));
    let env_x = area.x + lead;
    let env_w = (display_width(&name) + 3) as u16;
    app.hits.push((Rect { x: env_x, y: area.y, width: env_w.min(area.right().saturating_sub(env_x)), height: 1 }, Hit::Env));
    app.dropdown_anchor = (env_x, area.y + 1);
    // Narrower still, the tabs start after the dropdown and are cut at the
    // right edge: they never draw over it.
    let left_w = env_x - area.x + env_w + gap;
    let rw = (tabs_w as u16).min(area.width.saturating_sub(left_w));
    let r = Rect { x: area.right().saturating_sub(rw), width: rw, ..area };
    frame.render_widget(Paragraph::new(Line::from(left)).style(Style::new().bg(bg)), area);
    draw_tabs(frame, app, r, &CTab::ENV, &labels, sep_w, bg);
}

/// The cauldrons dropdown: NOT a modal (msg 165: "It shouldn't even use the
/// same code as the modal ... create a dropdown tool with a top left anchor
/// position"). No title; its left edge on the name's; as wide as the longest
/// name, at least 20; each cauldron marked on or off, not changed here; `+`
/// last, a new one.
fn draw_dropdown(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(d) = app.dropdown.clone() else { return };
    let name_w = d.entries.iter().map(|(n, _)| display_width(n)).max().unwrap_or(0).max(20);
    // A border, a space, the name, two spaces, the state, a space, a border.
    let w = (name_w + 13) as u16;
    let h = d.entries.len() as u16 + 3;
    let (x, y) = app.dropdown_anchor;
    let x = x.min(area.right().saturating_sub(w));
    let r = Rect { x, y, width: w.min(area.width), height: h.min(area.bottom().saturating_sub(y)) };
    frame.render_widget(Clear, r);
    let block = Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(palette::ink2());
    let inner = block.inner(r);
    frame.render_widget(block.style(palette::base()), r);
    app.hits.push((r, Hit::Modal));
    let mut lines = Vec::new();
    for i in 0..=d.entries.len() {
        let (text, state, base) = match d.entries.get(i) {
            Some((n, on)) => {
                let st = if n == &app.env.name { palette::ink().add_modifier(Modifier::BOLD) } else { palette::ink() };
                (format!(" {n:<name_w$}  "), Some(*on), st)
            }
            None => (format!(" {:<name_w$}  ", "+"), None, palette::code()),
        };
        let sel = i == d.selected;
        let style = if sel { base.bg(palette::c().sel_bg) } else { base };
        let mut spans = vec![Span::styled(text, style)];
        match state {
            Some(on) => {
                let st = if on { palette::code() } else { palette::muted() };
                let st = if sel { st.bg(palette::c().sel_bg) } else { st };
                spans.push(Span::styled(format!("{:<8}", crate::cauldron::manage::enabled_word(on)), st));
            }
            None => spans.push(Span::styled("        ", style)),
        }
        spans.push(Span::styled(" ", style));
        let y = inner.y + lines.len() as u16;
        lines.push(Line::from(spans));
        if y < inner.bottom() {
            app.hits.push((Rect { x: inner.x, y, width: inner.width, height: 1 }, Hit::Drop(i)));
        }
    }
    frame.render_widget(Paragraph::new(lines).style(palette::base()), inner);
}

// ── the Surface tab ───────────────────────────────────────────────────────

/// A box's legend at its top right (msg 235: "There's a legend saying surface
/// and bucket at the top right of each one of them"), recorded as a title
/// the DEV rename reaches.
fn legend(app: &mut App, area: Rect, t: Title, style: Style) -> Line<'static> {
    let word = format!(" {} ", app.named(t.word()));
    let w = display_width(&word) as u16;
    // Ratatui draws a right-aligned title one cell in from the corner.
    let x = area.right().saturating_sub(w + 1);
    app.hits.push((Rect { x, y: area.y, width: w.min(area.width), height: 1 }, Hit::Title(t)));
    Line::from(Span::styled(word, style)).right_aligned()
}

/// THE SURFACE TAB (msg 235, his final word: "If you are in the cauldron
/// tab, then the top half of it, of the box, is the surface. The bottom half
/// of the box is the bucket ... instead of the root tab being called
/// cauldron, let's name the root tab surface. And then what we were
/// previously calling surface, with the legend item saying surface, now
/// let's say the legend is soup"; 8e4fca10 msg 457 named the tab Cauldron
/// and the legend `surface` — "So we have the bucket and we have the
/// surface"): two boxes from the line under the top bar to the last — the
/// SURFACE (legend `surface`) above (its clusters in the sidebar, the selected
/// one's page beside it), the BUCKET below — and between them a divider
/// with a grabber that drags the bucket taller or shorter.
fn draw_surface(frame: &mut Frame, app: &mut App, area: Rect) {
    app.surface_area = area;
    // The bucket's box: its text rows, three to six as it fills, or as tall
    // as its divider was dragged; the soup keeps eight rows at least.
    let text_w = area.width.saturating_sub(4).max(1) as usize;
    let room = area.height.saturating_sub(2 + 8);
    let rows = app.bucket_rows(text_w, room);
    let bucket_h = (rows + 2).min(area.height);
    let [soup, bucket] = Layout::vertical([Constraint::Min(1), Constraint::Length(bucket_h)]).areas(area);
    draw_soup(frame, app, soup);
    draw_bucket(frame, app, bucket, rows as usize);
}

fn draw_soup(frame: &mut Frame, app: &mut App, area: Rect) {
    let lit = app.focus != Focus::Input;
    let title = legend(app, area, Title::Surface, if lit { palette::ink2() } else { palette::muted() });
    let block = Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(palette::muted()).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    app.clusters_area = inner;
    let tree_w = app.sidebar_width(inner.width);
    let [tree, page] = Layout::horizontal([Constraint::Length(tree_w), Constraint::Min(1)]).areas(inner);
    draw_tree(frame, app, tree);
    draw_content(frame, app, page);
    draw_divider(frame, app, tree);
}

/// The divider is the sidebar's right-hand column: a line, with ONE grabber
/// at its vertical middle. Only the grabber takes a press-and-drag (F52); the
/// rest of the column is the sidebar's.
fn draw_divider(frame: &mut Frame, app: &mut App, tree: Rect) {
    if tree.width == 0 || tree.height == 0 {
        return;
    }
    let col = Rect { x: tree.right() - 1, width: 1, ..tree };
    let mid = Rect { y: col.y + col.height / 2, height: 1, ..col };
    let style = if app.dragging { palette::amber() } else { palette::ink2() };
    frame.render_widget(Paragraph::new(Span::styled(GRABBER, style)), mid);
    app.hits.push((mid, Hit::Divider));
}

/// The sidebar: its header (`Clusters`, msg 235: "The sidebar says clusters",
/// and a `+` on the right that makes a ROOT cluster), a line under it, then
/// the tree — every cluster with a `+` at its right that makes one under it
/// (msg 165: "you just see a bunch of pluses"). Under ^G the clusters the
/// page covers — the selected one and everything below it — all carry the
/// selection's background, the selected one lit: that is how the page says
/// it covers them, with no word on any tab strip (msg 235: "Don't have any
/// metadata in the tab header").
fn draw_tree(frame: &mut Frame, app: &mut App, area: Rect) {
    let w = area.width.saturating_sub(1) as usize;
    let vis = app.visible();
    // The header and its line take two rows; the switch at the foot and its
    // line, two more, when there is room for a cluster as well.
    let foot = area.height >= 5;
    let h = (area.height as usize).saturating_sub(if foot { 4 } else { 2 });
    let mut lines = Vec::new();
    let plus_style = |on: bool| if on && app.focus == Focus::Tree { palette::code().add_modifier(Modifier::BOLD).bg(palette::c().sel_bg) } else if on { palette::code().add_modifier(Modifier::UNDERLINED) } else { palette::code() };
    let head = format!(" {}", app.named(Title::Sidebar.word()));
    let pad = w.saturating_sub(display_width(&head) + 2);
    app.hits.push((Rect { x: area.x, y: area.y, width: (display_width(&head) as u16).min(area.width), height: 1 }, Hit::Title(Title::Sidebar)));
    lines.push(Line::from(vec![
        Span::styled(head, palette::header()),
        Span::raw(" ".repeat(pad)),
        Span::styled("+", plus_style(app.on_add_root)),
        Span::raw(" "),
    ]));
    if w >= 2 {
        app.hits.push((Rect { x: area.x + w.saturating_sub(2) as u16, y: area.y, width: 1, height: 1 }, Hit::AddRoot));
    }
    lines.push(Line::from(Span::styled("─".repeat(w), palette::muted())));
    let at = if app.on_add_root { 0 } else if app.on_below { vis.len().saturating_sub(1) } else { vis.iter().position(|&g| g == app.selected).unwrap_or(0) };
    let mut sel_row: Option<u16> = None;
    let mut below_row: Option<(u16, u16)> = None;
    let mut covered: Vec<u16> = Vec::new();
    let scope = app.scope();
    let top = at.saturating_sub(h.saturating_sub(1));
    for row in (top..vis.len()).take(h) {
        let y = area.y + 2 + (row - top) as u16;
        let id = vis[row];
        let g = app.forest.get(id);
        let marker = if g.children.is_empty() {
            "·"
        } else if app.is_expanded(id) {
            "▾"
        } else {
            "▸"
        };
        let indent = "  ".repeat(g.depth);
        let left = crop(&format!(" {indent}{marker} {}", g.name), w.saturating_sub(5));
        // No marker but the chevron (msg 186: "I don't know what that square
        // means. Just remove it"); the selection is a background, painted
        // once the tree is drawn.
        let pad = w.saturating_sub(display_width(&left) + 2);
        let selected = id == app.selected && !app.on_add_root && !app.on_below;
        if selected {
            sel_row = Some(y);
        } else if app.aggregate && scope.contains(&id) {
            covered.push(y);
        }
        // The sidebar's sections are bold: its roots, or under the whole
        // cauldron its children — and the whole cauldron itself.
        let base = if selected || g.depth <= app.forest.section_depth() { palette::ink().add_modifier(Modifier::BOLD) } else { palette::ink2() };
        lines.push(Line::from(vec![Span::styled(format!("{left}{}", " ".repeat(pad)), base), Span::styled("+", palette::muted()), Span::styled(" ", base)]));
        app.hits.push((Rect { x: area.x, y, width: (w.saturating_sub(2)) as u16, height: 1 }, Hit::TreeRow(id)));
        app.hits.push((Rect { x: area.x + w.saturating_sub(2) as u16, y, width: 1, height: 1 }, Hit::AddChild(id)));
        let mx = area.x + 1 + (g.depth * 2) as u16;
        if !g.children.is_empty() && mx < area.right() {
            app.hits.push((Rect { x: mx, y, width: 2, height: 1 }, Hit::Disclosure(id)));
        }
    }
    // THE SWITCH AT THE FOOT (b964d571 msg 23: "a toggle at the bottom of
    // the cluster sidebar to trigger showing child soup contents in parent
    // cluster views"): `Aggregated clusters` — what ^G and `n below` turn, the
    // whole cauldron appearing as every cluster's root while it is on.
    // Its name flips it; its `on` and `off` set it; the cursor reaches it as
    // the row after the last cluster.
    if foot {
        while lines.len() < area.height as usize - 2 {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled("─".repeat(w), palette::muted())));
        let t = crate::cauldron::toggle::Toggle::new(app.aggregate);
        let tw = t.width();
        let name = crop(" Aggregated clusters", w.saturating_sub(tw + 2));
        let nw = display_width(&name);
        let pad = w.saturating_sub(nw + tw + 1);
        let y = area.bottom() - 1;
        let mut spans = vec![Span::styled(name, palette::ink2()), Span::raw(" ".repeat(pad))];
        spans.extend(t.spans());
        lines.push(Line::from(spans));
        app.hits.push((Rect { x: area.x, y, width: (nw + pad) as u16, height: 1 }, Hit::Below));
        let tx = area.x + (nw + pad) as u16;
        for ((zx, zw), on) in t.hotspots().into_iter().zip([true, false]) {
            let x = tx + zx as u16;
            if x < area.x + w as u16 {
                app.hits.push((Rect { x, y, width: (zw as u16).min(area.x + w as u16 - x), height: 1 }, Hit::BelowOpt(on)));
            }
        }
        if app.on_below {
            sel_row = None;
            covered.clear();
            below_row = Some((y, nw as u16));
        }
    }
    let block = Block::new().borders(Borders::RIGHT).border_style(palette::muted());
    frame.render_widget(Paragraph::new(lines).block(block).style(palette::base()), area);
    if let Some((y, nw)) = below_row {
        look::paint(frame.buffer_mut(), Rect { x: area.x, y, width: nw, height: 1 }, look::sel_bg(app.focus == Focus::Tree));
    }
    for y in covered {
        look::paint(frame.buffer_mut(), Rect { x: area.x, y, width: w as u16, height: 1 }, palette::c().sel_bg_dim);
    }
    if let Some(y) = sel_row {
        look::paint(frame.buffer_mut(), Rect { x: area.x, y, width: w as u16, height: 1 }, look::sel_bg(app.focus == Focus::Tree));
    }
}

/// The cluster tabs: the strip at the TOP of the page, on the strip's own
/// background, as Status's and Config's are (msg 235: "The surface content
/// panel has a header above the tabs. Let's nix that because there's no
/// useful information there anymore. So we'll just scoot the tabs up two
/// lines and give them the same background color as you can see in status,
/// system, and config tabs"). When they do not fit, they tighten — first the
/// padding goes, then the long names shorten — so the last tab is never cut
/// off.
fn draw_ktabs(frame: &mut Frame, app: &mut App, area: Rect) {
    let titles: Vec<String> = KTab::ALL.iter().map(|&t| app.ktab_title(t)).collect();
    let shorten = |short: bool| -> Vec<String> { titles.iter().zip(KTab::ALL).map(|(x, t)| if short { x.replacen(&app.named(t.label()), t.short(), 1) } else { x.clone() }).collect() };
    let short = look::strip_width(&shorten(false)) > area.width as usize;
    let labels = shorten(short);
    let shown = KTab::ALL.iter().position(|t| *t == app.ktab).unwrap_or(0);
    let (line, hotspots) = look::strip(&labels, shown, app.focus == Focus::Content, false, area.width as usize);
    for ((x, w), t) in hotspots.into_iter().zip(KTab::ALL) {
        let x = area.x + x as u16;
        if x < area.right() {
            app.hits.push((Rect { x, y: area.y, width: (w as u16).min(area.right() - x), height: 1 }, Hit::KTab(t)));
        }
    }
    frame.render_widget(Paragraph::new(line).style(palette::base()), area);
}

fn draw_content(frame: &mut Frame, app: &mut App, area: Rect) {
    // The strip, then the page under it. No header and no line between
    // them: the strip's background is the edge (msg 235).
    let [tabs, inner] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    draw_ktabs(frame, app, tabs);
    // No gutter: the selected item is a background (msg 186). A column is
    // kept clear on each side, so nothing touches the frame.
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let width = inner.width as usize;
    let page = crate::cauldron::pages::page(app, width);
    draw_page(frame, app, inner, page);
}

/// THE THREADS TAB (msg 235: "let's add another top level tab called
/// threads ... And then the whole threads application can be moved inside
/// of that threads tab"): the threads app in a box like every other tab's.
fn draw_threads(frame: &mut Frame, app: &mut App, area: Rect) {
    let (said, said_style) = status_line(app);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(palette::muted())
        .title_bottom(Line::from(Span::styled(said, said_style)).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    crate::cauldron::threads::draw(frame, app, inner);
}

/// Draw `page` into `inner` with the selection gutter, scrolling the selected
/// item into view, and record a click region for every visible item row (and
/// every click hotspot inside a row).
fn draw_page(frame: &mut Frame, app: &mut App, inner: Rect, mut page: crate::cauldron::pages::Page) {
    app.hotspot_paths = std::mem::take(&mut page.paths);
    let n_items = page.items.len();
    if app.item >= n_items {
        app.item = n_items.saturating_sub(1);
    }
    let h = inner.height as usize;
    if app.reveal {
        if let Some(r) = page.items.get(app.item) {
            if r.start < app.scroll + 1 {
                app.scroll = r.start.saturating_sub(1);
            } else if r.end > app.scroll + h {
                app.scroll = r.end.saturating_sub(h).min(r.start.saturating_sub(1));
            }
        }
        app.reveal = false;
    }
    let max = page.lines.len().saturating_sub(h);
    app.scroll = app.scroll.min(max);

    let sel = page.items.get(app.item).cloned().filter(|_| n_items > 0);
    let lines: Vec<Line> = std::mem::take(&mut page.lines);
    for (i, r) in page.items.iter().enumerate() {
        for row in r.clone() {
            if row >= app.scroll && row < app.scroll + h {
                let y = inner.y + (row - app.scroll) as u16;
                app.hits.push((Rect { x: inner.x, y, width: inner.width, height: 1 }, Hit::Item(i)));
            }
        }
    }
    // Hotspots go after the rows, so a click on one wins over its row.
    for &(row, col, w, hit) in &page.hotspots {
        if row >= app.scroll && row < app.scroll + h {
            let y = inner.y + (row - app.scroll) as u16;
            let x = inner.x + col as u16;
            if x < inner.right() {
                app.hits.push((Rect { x, y, width: (w as u16).min(inner.right() - x), height: 1 }, hit));
            }
        }
    }
    frame.render_widget(
        Paragraph::new(lines).scroll((app.scroll as u16, 0)).style(palette::base()),
        inner,
    );
    // The selection: a subtle background over the selected item's rows —
    // no bar, no gutter (msg 186).
    if let Some(r) = sel {
        paint_rows(frame, inner, r, app.scroll, app.focus == Focus::Content);
    }
}

/// Paint the rows `r` of a page scrolled to `top`, drawn into `inner`, with
/// the selection background.
fn paint_rows(frame: &mut Frame, inner: Rect, r: std::ops::Range<usize>, top: usize, focused: bool) {
    let h = inner.height as usize;
    let from = r.start.max(top);
    let to = r.end.min(top + h);
    if from >= to {
        return;
    }
    let area = Rect { y: inner.y + (from - top) as u16, height: (to - from) as u16, ..inner };
    look::paint(frame.buffer_mut(), area, look::sel_bg(focused));
}

// ── Status and Config ─────────────────────────────────────────────────────

/// What the last event said (a notice, green, for one event) or a standing
/// error (red): in the foot of the tab's box — the bucket's on Surface — so
/// nothing said goes unseen.
fn status_line(app: &App) -> (String, Style) {
    match (&app.error, &app.notice) {
        (Some(e), _) => (format!(" {e} "), palette::red()),
        // Green is for MOMENTARY confirmations only — the notice lasts one event.
        (None, Some(n)) => (format!(" {n} "), palette::green()),
        (None, None) => (String::new(), palette::muted()),
    }
}

fn draw_dash(frame: &mut Frame, app: &mut App, area: Rect) {
    let (said, said_style) = status_line(app);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(palette::muted())
        .title_bottom(Line::from(Span::styled(said, said_style)).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let inner = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    // No gutter: the selected row is a background (msg 186).
    let w = inner.width as usize;
    let mut tips: Vec<(usize, usize, usize)> = Vec::new();
    type Hotspots = Vec<(usize, usize, usize, Hit)>;
    let (lines, ranges, hotspots): (Vec<Line<'static>>, Vec<std::ops::Range<usize>>, Hotspots) = match app.ctab {
        CTab::Status => {
            let n = crate::cauldron::status::view(app, w).items.len();
            if app.rows[0] >= n {
                app.rows[0] = n.saturating_sub(1);
            }
            let v = crate::cauldron::status::view(app, w);
            (v.lines, v.items.into_iter().map(|i| i.range).collect(), v.hotspots)
        }
        // A Config row may take more than one line (a refusal under it):
        // the ▌ marks all of them.
        crate::cauldron::app::page_tabs!() => {
            let n = app.selectable().len();
            let at = crate::cauldron::menu::SETTINGS_ROW;
            if app.rows[at] >= n {
                app.rows[at] = n.saturating_sub(1);
            }
            let (mut l, r, t, z) = settings_entry_lines(app, w);
            // Under a row's modal the page draws no ⓘ: its column would
            // stand beside the modal's edge, a target it cannot be (task
            // 0209 D10).
            if matches!(app.modal, Some(Modal::Settings { .. })) {
                for line in &mut l {
                    for sp in line.spans.iter_mut().filter(|sp| sp.content == TIP) {
                        sp.content = " ".into();
                    }
                }
            } else {
                tips = t;
            }
            (l, r, z)
        }
        CTab::Cauldron | CTab::Threads => unreachable!("drawn by draw_surface and draw_threads"),
    };
    debug_assert_eq!(ranges.len(), app.dash_rows().len(), "drawn rows and App::dash_rows disagree");
    let h = inner.height as usize;
    let sel_range = ranges.get(app.row()).cloned().unwrap_or(0..1);
    let mut top = sel_range.start.saturating_sub(h.saturating_sub(2));
    if sel_range.end > top + h {
        top = sel_range.end.saturating_sub(h);
    }
    for (i, r) in ranges.iter().enumerate() {
        for line in r.clone() {
            if line >= top && line < top + h {
                let y = inner.y + (line - top) as u16;
                app.hits.push((Rect { x: inner.x, y, width: inner.width, height: 1 }, Hit::Row(i)));
            }
        }
    }
    for (line, col, i) in tips {
        if line >= top && line < top + h {
            let y = inner.y + (line - top) as u16;
            let x = inner.x + col as u16;
            if x < inner.right() {
                app.hits.push((Rect { x, y, width: 1, height: 1 }, Hit::Tip(i)));
            }
        }
    }
    for (line, col, zw, hit) in hotspots {
        if line >= top && line < top + h {
            let y = inner.y + (line - top) as u16;
            let x = inner.x + col as u16;
            if x < inner.right() {
                app.hits.push((Rect { x, y, width: (zw as u16).min(inner.right() - x), height: 1 }, hit));
            }
        }
    }
    // THE SAME SELECTION AS A CLUSTER'S PAGE (`draw_page`): a background —
    // but a table's row and a button draw their own (msg 235: the button,
    // "not like a whole row").
    let sel = ranges.get(app.row()).cloned();
    let own = app.ctab.is_page() && app.settings_entry().is_some_and(|e| e.is_button() || matches!(e, Entry::Row(_)));
    frame.render_widget(Paragraph::new(lines).scroll((top as u16, 0)).style(palette::base()), inner);
    if let (Some(r), false) = (sel, own) {
        paint_rows(frame, inner, r, top, app.focus != Focus::Input);
    }
}

/// The label column of a settings row.
const SETTING_LABEL_W: usize = 30;
/// The value column of a settings row — as wide as the widest switch
/// (`inherit  on  off`, `enabled  disabled`); what it defaults to starts
/// after it.
const SETTING_VALUE_W: usize = 20;

/// The label column in a row's modal: narrower than a tab's, so a strip of
/// six options (an endpoint's template, design 0199 §3) fits beside it.
const MODAL_LABEL_W: usize = 20;
/// The tooltip icon beside every setting (msg 165: "a little tooltip icon
/// to the right of each one"): a click opens what it means.
pub const TIP: &str = "ⓘ";
/// The revert glyph (msg 235: "you could have a little revert ASCII
/// character appear to the right of it"): shown only while a setting is
/// not its default; a click puts it back.
pub const REVERT: &str = "↺";

/// Every line of the Config tab, as its bands are open, the lines each
/// selectable entry takes, and where each setting's ⓘ is (its line, its
/// column, the index of its entry) — every row has one (V7) — and the click
/// hotspots inside rows (the strip's tabs, a switch's options, a ↺, a
/// button). No breadcrumb: the bands are the way in (msg 165).
pub type Laid = (Vec<Line<'static>>, Vec<std::ops::Range<usize>>, Vec<(usize, usize, usize)>, Vec<(usize, usize, usize, Hit)>);

pub fn settings_entry_lines(app: &App, width: usize) -> Laid {
    flat_lines(app, &app.entries(), width, Some(app.settings_sel()), false)
}

/// The lines of a table row's modal (its page's entries, `sel` selected).
pub fn modal_entry_lines(app: &App, page: &crate::cauldron::menu::SPage, sel: usize, width: usize) -> Laid {
    flat_lines(app, &app.modal_entries(page), width, Some(sel), true)
}

/// One row grammar (0057 §14, msg 235): `<label>  <value or switch>
/// <default>  ↺  ⓘ` — the value as it is, what its default is (`default
/// off`, or `inherits 80 %`), and ↺ only while it is not its default. A
/// band's header is `▸ label` and its summary flush right. A table's rows
/// are rows: each on its own background, its switches live, `›` at its end;
/// an action is a button. `sel` is the selected selectable row, which a row
/// or a button draws itself; `modal` says whose hits these are.
fn flat_lines(app: &App, entries: &[Flat], width: usize, sel: Option<usize>, modal: bool) -> Laid {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut ranges = Vec::new();
    let mut tips = Vec::new();
    let mut hotspots: Vec<(usize, usize, usize, Hit)> = Vec::new();
    let opt_hit = |row: usize, cell: usize, opt: usize| if modal { Hit::MOpt { row: row as u16, cell: cell as u8, opt: opt as u8 } } else { Hit::Opt { row: row as u16, cell: cell as u8, opt: opt as u8 } };
    let revert_hit = |row: usize| if modal { Hit::MRevert(row as u16) } else { Hit::Revert(row as u16) };
    let press_hit = |row: usize| if modal { Hit::MPress(row as u16) } else { Hit::Press(row as u16) };
    // The selectable rows so far: the index the next one has.
    let mut si = 0usize;
    for (ei, f) in entries.iter().enumerate() {
        let row_i = si;
        let selected = f.entry.selectable() && sel == Some(row_i);
        let ind = "  ".repeat(f.depth);
        // What a band holds sits two columns in from its title (msg 235:
        // "you need to add two spaces ... So that they look contained"): a
        // band inside it gets that from its chevron, every other line here.
        let lead = format!("  {ind}{}", if f.depth > 0 { "  " } else { "" });
        // The label column is as wide at every depth: a deeper band's values
        // sit further right, and its labels are never cut shorter.
        let lw = if modal { MODAL_LABEL_W } else { SETTING_LABEL_W }.min(width / 2).max(6);
        let label = |t: &str, style: Style| Span::styled(format!("{lead}{:<w$}", crop(t, lw - 1), w = lw), style);
        // A table: before the first of a run of rows, its column names.
        if let Entry::Row(page) = &f.entry {
            if !matches!(entries.get(ei.wrapping_sub(1)).map(|p| &p.entry), Some(Entry::Row(_))) {
                let run: Vec<&crate::cauldron::menu::SPage> = entries[ei..]
                    .iter()
                    .map_while(|x| match &x.entry {
                        Entry::Row(p) => Some(p),
                        _ => None,
                    })
                    .collect();
                let head = app.row_head(page);
                if !head.is_empty() {
                    let cw = table_widths(app, &run, &head, table_room(width, &lead));
                    let mut spans = vec![Span::raw(lead.clone()), Span::raw(" ")];
                    // A name narrowed with its column keeps the gap after it.
                    for (i, h) in head.iter().enumerate() {
                        spans.push(Span::styled(format!("{}  ", look::cell(h, cw[i])), palette::muted()));
                    }
                    out.push(fit(spans, width));
                }
            }
        }
        let start = out.len();
        match &f.entry {
            // A head drawn flat (a page of entries shown in place): THE band
            // line, open (`flatten` makes it a band on a Config tab).
            Entry::Head(t, meta) => {
                let meta = look::meta(meta.split(" · ").filter(|x| !x.is_empty()).map(|x| Span::styled(x.to_string(), palette::muted())));
                tips.push((out.len(), width.saturating_sub(1), ei));
                out.push(tipped(look::band_line(f.depth + 1, true, vec![look::name(t.clone())], meta, width.saturating_sub(2)), width));
            }
            // An always-expanded band: a dot where the chevron would be.
            Entry::Fixed(t, meta) => {
                let meta = look::meta(meta.split(" · ").filter(|x| !x.is_empty()).map(|x| Span::styled(x.to_string(), palette::muted())));
                tips.push((out.len(), width.saturating_sub(1), ei));
                out.push(tipped(look::fixed_band_line(f.depth + 1, vec![look::name(t.clone())], meta, width.saturating_sub(2)), width));
            }
            Entry::Gap => out.push(Line::default()),
            Entry::Fact(k, v) => {
                tips.push((out.len(), width.saturating_sub(1), ei));
                out.push(with_tip(vec![label(k, palette::muted()), Span::styled(v.clone(), palette::ink2())], width));
            }
            // THE band line (`look::band_line`): its name on the left, its
            // summary flush right, just short of the ⓘ column.
            Entry::Sub { label: l, summary, .. } => {
                let name = vec![Span::styled(format!("{:<w$}", crop(l, lw.saturating_sub(3)), w = lw.saturating_sub(2)), palette::ink().add_modifier(Modifier::BOLD))];
                let meta = look::meta(summary.split(" · ").map(|x| Span::styled(x.to_string(), palette::muted())));
                tips.push((out.len(), width.saturating_sub(1), ei));
                out.push(tipped(look::band_line(f.depth + 1, f.open == Some(true), name, meta, width.saturating_sub(2)), width));
            }
            // A table's row (msg 235): its name and its cells in left-aligned
            // columns on the row's background, `›` at its end, and under it
            // the app's background parting it from the next.
            Entry::Row(page) => {
                let run_start = entries[..ei].iter().rposition(|x| !matches!(x.entry, Entry::Row(_))).map_or(0, |i| i + 1);
                let run: Vec<&crate::cauldron::menu::SPage> = entries[run_start..]
                    .iter()
                    .map_while(|x| match &x.entry {
                        Entry::Row(p) => Some(p),
                        _ => None,
                    })
                    .collect();
                let head = app.row_head(page);
                let cw = table_widths(app, &run, &head, table_room(width, &lead));
                let bg = if selected { palette::c().sel_bg } else { palette::c().row_bg };
                let on_bg = |s: String, st: Style| Span::styled(s, st.bg(bg));
                let row_w = width.saturating_sub(2);
                let mut spans = vec![Span::raw(lead.clone()), on_bg(" ".into(), Style::new())];
                let mut x = display_width(&lead) + 1;
                // A value cut to its column keeps the column's gap: two
                // narrowed columns never abut (0211 R7).
                let gapped = |s: &str, w: usize| look::cell(&crop(s, w.saturating_sub(2)), w);
                spans.push(on_bg(gapped(&app.row_label(page), cw[0] + 2), palette::ink().add_modifier(Modifier::BOLD)));
                x += cw[0] + 2;
                let mut toggle_i = 0;
                for (ci, c) in app.row_cells(page).into_iter().enumerate() {
                    let w = cw.get(ci + 1).copied().unwrap_or(0) + 2;
                    match c {
                        crate::cauldron::menu::Cell::Toggle(bd) => {
                            let t = app.toggle_of(&bd);
                            for (oi, (zx, zw)) in t.hotspots().into_iter().enumerate() {
                                hotspots.push((out.len(), x + zx, zw, opt_hit(row_i, toggle_i, oi)));
                            }
                            toggle_i += 1;
                            let tw = t.width();
                            spans.extend(t.spans());
                            spans.push(on_bg(" ".repeat(w.saturating_sub(tw)), Style::new()));
                        }
                        crate::cauldron::menu::Cell::Text(s) => spans.push(on_bg(gapped(&s, w), palette::ink())),
                    }
                    x += w;
                }
                let used: usize = spans.iter().map(|s| display_width(&s.content)).sum();
                let chev = format!("{} ", look::ROW_CHEVRON);
                spans.push(on_bg(" ".repeat(row_w.saturating_sub(used + display_width(&chev))), Style::new()));
                spans.push(on_bg(chev, palette::muted()));
                tips.push((out.len(), width.saturating_sub(1), ei));
                out.push(with_tip(spans, width));
                out.push(look::row_edge(display_width(&lead), row_w, bg));
                // A switch on the row that refused is said under it (design
                // 0199: "refused on the row: `disabled in Providers`").
                if let Some((rb, why)) = &app.refusal {
                    if app.row_cells(page).iter().any(|c| matches!(c, crate::cauldron::menu::Cell::Toggle(b) if b == rb)) {
                        out.push(fit(vec![Span::styled(format!("{lead}    ✗ {why}"), palette::red())], width));
                    }
                }
            }
            Entry::Set(bd) => {
                let d = bd.def();
                let r = app.sget(bd);
                let list = d.ty == Ty::List;
                // A list is a band: its items and `+ add` are inside it.
                let name = app.row_name(bd);
                let shown_label = if list { format!("{}{}", if f.open == Some(true) { "▾ " } else { "▸ " }, name) } else { name };
                let name = crop(&shown_label, lw.saturating_sub(3));
                // A list's chevron stands where a band's does.
                let row_lead = if list { format!("  {ind}") } else { lead.clone() };
                let mut spans = vec![Span::styled(format!("{row_lead}{name}"), palette::ink()), Span::raw(" ".repeat(lw.saturating_sub(display_width(&name))))];
                let value_x = display_width(&row_lead) + lw;
                let tip_line = out.len();
                let editing = app.field.as_ref().filter(|x| x.bound == *bd);
                if let Some(x) = editing {
                    spans.push(Span::styled("› ", palette::amber()));
                    let text = if d.ty == Ty::Secret { "•".repeat(x.text.chars().count()) } else { x.text.clone() };
                    spans.push(Span::styled(text, palette::ink()));
                    spans.push(Span::styled("▏", palette::amber()));
                } else if matches!(d.ty, Ty::Toggle | Ty::LiveLogin) || app.strip_words(bd).is_some() {
                    let t = app.toggle_of(bd);
                    for (oi, (zx, zw)) in t.hotspots().into_iter().enumerate() {
                        hotspots.push((out.len(), value_x + zx, zw, opt_hit(row_i, 0, oi)));
                    }
                    spans.extend(t.spans());
                } else if list {
                    // A list opens to its items: its row says as much as the
                    // row has room for — its column when a default is said
                    // after it, else up to the ⓘ, never cut with room left
                    // (0211 R12).
                    let note_w = match app.default_note(bd) { n if n.is_empty() => 0, n => display_width(&n) + 2 };
                    let room = width.saturating_sub(value_x + 4 + note_w).max(SETTING_VALUE_W - 2);
                    spans.push(Span::styled(crop(&show(d, &r.val), room), palette::ink()));
                } else {
                    spans.push(Span::styled(app.value_text(bd), palette::ink()));
                }
                // What the default is lines up under every value.
                let used: usize = spans[2..].iter().map(|x| display_width(&x.content)).sum();
                spans.push(Span::raw(" ".repeat(SETTING_VALUE_W.saturating_sub(used))));
                let note = app.default_note(bd);
                // A default the row has no room for is said whole on the
                // line under it, at the value's column, never cut (0217 B13:
                // `defau…`, `inherits park n…` at 80 columns).
                let row_w: usize = spans.iter().map(|x| display_width(&x.content)).sum();
                let mut under: Option<String> = None;
                if !note.is_empty() {
                    if row_w + 2 + display_width(&note) + 4 > width {
                        under = Some(note.clone());
                    } else {
                        spans.push(Span::styled(format!("  {note}"), palette::muted()));
                    }
                }
                let ram = if d.note == crate::cauldron::settings::MACHINE_RAM_NOTE { crate::cauldron::settings::ram_note(&r.val) } else { d.note.to_string() };
                if !ram.is_empty() {
                    spans.push(Span::styled(format!("  {ram}"), palette::muted()));
                }
                if app.reverts(bd) {
                    let x: usize = spans.iter().map(|s| display_width(&s.content)).sum::<usize>() + 2;
                    if x + 1 < width.saturating_sub(2) {
                        hotspots.push((out.len(), x, 1, revert_hit(row_i)));
                    }
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(REVERT, palette::code()));
                }
                out.push(with_tip(spans, width));
                tips.push((tip_line, width.saturating_sub(1), ei));
                if let Some(note) = under {
                    out.push(fit(vec![Span::raw(" ".repeat(value_x)), Span::styled(note, palette::muted())], width));
                }
                let why = editing.and_then(|x| x.error.clone()).or_else(|| app.refusal.as_ref().filter(|(b, _)| b == bd).map(|(_, w)| w.clone()));
                if let Some(why) = why {
                    out.push(fit(vec![Span::styled(format!("{lead}    ✗ {why}"), palette::red())], width));
                }
            }
            // In a row's modal a list has no band of its own: its items stand
            // under the value column of the row they belong to.
            Entry::Item { list, index, text } => {
                let at = if modal { format!("{lead}{}", " ".repeat(lw)) } else { lead.clone() };
                tips.push((out.len(), width.saturating_sub(1), ei));
                let mut spans = vec![Span::styled(format!("{at}· {text}"), palette::ink())];
                if app.pending_remove.as_ref() == Some(&(list.concrete(), *index)) {
                    spans.push(Span::styled("   take it off the list? x again", palette::red()));
                }
                out.push(with_tip(spans, width));
            }
            // Every action is a BUTTON (msg 235): its words on its own
            // background, the row around it the page's.
            Entry::Add(_) | Entry::Run(_) | Entry::Do { .. } => {
                let (words, summary) = match &f.entry {
                    Entry::Add(Add::Account(_)) => ("+ add account".to_string(), String::new()),
                    Entry::Add(Add::Item(list)) => (crate::cauldron::menu::add_words(list), String::new()),
                    Entry::Run(bd) => (bd.def().label.to_string(), String::new()),
                    Entry::Do { label: l, summary, .. } => (l.clone(), summary.clone()),
                    _ => unreachable!("matched above"),
                };
                // `add endpoint` is dim while the endpoint lacks what its
                // template needs, the reason beside it (design 0199 §3).
                // A greyed source's button is dim and inert (R-V1 item 7).
                let waiting = app.entry_greyed(&f.entry) || matches!(&f.entry, Entry::Do { act: crate::cauldron::manage::Act::AddEndpoint { .. }, summary, .. } if !summary.is_empty());
                let b = if waiting { look::button_dim(&words, selected) } else { look::button(&words, selected) };
                let at = if modal && matches!(f.entry, Entry::Add(Add::Item(_))) { format!("{lead}{}", " ".repeat(lw)) } else { lead.clone() };
                hotspots.push((out.len(), display_width(&at), display_width(&b.content), press_hit(row_i)));
                let mut spans = vec![Span::raw(at), b];
                if !summary.is_empty() {
                    spans.push(Span::styled(format!("  {summary}"), palette::muted()));
                }
                tips.push((out.len(), width.saturating_sub(1), ei));
                out.push(with_tip(spans, width));
                if let (Entry::Run(bd), Some((rb, why))) = (&f.entry, &app.refusal) {
                    if rb == bd {
                        out.push(fit(vec![Span::styled(format!("{lead}    ✗ {why}"), palette::red())], width));
                    }
                }
            }
            Entry::Ref { label: l, summary, .. } => {
                tips.push((out.len(), width.saturating_sub(1), ei));
                // A path wider than the label column takes the room the row
                // has, and is never cut with room left (0211 R12).
                let room = width.saturating_sub(display_width(&lead) + display_width(summary) + 4).max(lw - 1);
                let lw2 = lw.max(display_width(l).min(room) + 2);
                let name = Span::styled(format!("{lead}{:<w$}", crop(l, lw2 - 2), w = lw2), palette::amber().add_modifier(Modifier::BOLD));
                out.push(with_tip(vec![name, Span::styled(summary.clone(), palette::ink())], width));
            }
            Entry::Para(text) => {
                for para in text.split('\n') {
                    wrap(&mut out, para, width, 4 + ind.len(), palette::ink2());
                }
            }
        }
        // An entry of a greyed inference source (R-V1 item 7) is drawn as
        // Codex's band is, muted, every line of it — never hidden. A
        // refusal said under it keeps its red.
        if app.entry_greyed(&f.entry) {
            for line in &mut out[start..] {
                for sp in &mut line.spans {
                    if sp.style.fg.is_some() && sp.style.fg != Some(palette::c().red) {
                        sp.style = sp.style.fg(palette::c().muted).remove_modifier(Modifier::BOLD);
                    }
                }
            }
        }
        if f.entry.selectable() {
            ranges.push(start..out.len());
            si += 1;
        }
    }
    (out, ranges, tips, hotspots)
}

/// What a table's columns may take of a row `width` wide after `lead`:
/// the row's last two columns (the ⓘ), its leading space and its `› `.
fn table_room(width: usize, lead: &str) -> usize {
    width.saturating_sub(2 + display_width(lead) + 1 + 2)
}

/// The widths of a table's columns: its rows' names, then each cell —
/// a switch as wide as it draws, a value as wide as its text — and each
/// column at least as wide as its name. When they do not fit in `room`
/// the widest TEXT column narrows first, a cell at a time (task 0209 D5):
/// a switch never loses an option and the row never loses its `›`.
fn table_widths(app: &App, run: &[&crate::cauldron::menu::SPage], head: &[&str], room: usize) -> Vec<usize> {
    let mut cw: Vec<usize> = head.iter().map(|h| display_width(h)).collect();
    let mut switch = vec![false; cw.len()];
    for p in run {
        let mut cells = vec![(display_width(&app.row_label(p)), false)];
        for c in app.row_cells(p) {
            cells.push(match c {
                crate::cauldron::menu::Cell::Toggle(bd) => (app.toggle_of(&bd).width(), true),
                crate::cauldron::menu::Cell::Text(s) => (display_width(&s), false),
            });
        }
        if cw.len() < cells.len() {
            cw.resize(cells.len(), 0);
            switch.resize(cells.len(), false);
        }
        for (i, (w, t)) in cells.into_iter().enumerate() {
            cw[i] = cw[i].max(w);
            switch[i] |= t;
        }
    }
    const MIN: usize = 4;
    while cw.iter().map(|w| w + 2).sum::<usize>() > room {
        let Some(i) = (0..cw.len()).filter(|&i| !switch[i] && cw[i] > MIN).max_by_key(|&i| cw[i]) else { break };
        cw[i] -= 1;
    }
    cw
}

/// A Config item's row: `spans` cut to leave the last two columns, and its
/// ⓘ in the rightmost one.
fn with_tip(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut l = look::fit_spans(spans, width.saturating_sub(2));
    let used: usize = l.iter().map(|s| display_width(&s.content)).sum();
    l.push(Span::raw(" ".repeat(width.saturating_sub(used + 1))));
    l.push(Span::styled(TIP, palette::muted()));
    Line::from(l)
}

/// A row already cut to leave the last two columns, its ⓘ in the rightmost.
fn tipped(l: Line<'static>, width: usize) -> Line<'static> {
    with_tip(l.spans, width)
}

/// Where the caret sits in a new-cluster field's text while it is wrapped
/// (a Unicode noncharacter, which no keyboard types).
const CARET: char = '\u{FDD0}';

/// Spans cropped to one row of `width`.
pub(crate) fn fit(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut used = 0;
    let mut out = Vec::new();
    for s in spans {
        let w = display_width(&s.content);
        if used + w > width {
            out.push(Span::styled(crop(&s.content, width.saturating_sub(used)), s.style));
            break;
        }
        used += w;
        out.push(s);
    }
    Line::from(out)
}

// ── modals ──────────────────────────────────────────────────────────────────

/// A centred box of `w`x`h`, cleared, bordered and titled; its inside is
/// recorded as `Hit::Modal` so a click there stays in the modal.
/// The modal (msg 186: "a thicker border all the way around it, and then a
/// box inside of it ... a different color too"): a THICK outer border in the
/// cool colour, its title on it, a rounded box inside, and the whole of it
/// on its own background ([`palette::c().modal_bg`], painted by [`draw_modal`]
/// once its content is drawn). The content sits one column in from the
/// inner box, and gets as many rows as before: the inner box's border takes
/// the rows the padding used to.
/// A form modal's two buttons (0214 A6: Enter submitted invisibly): what
/// Enter does, on its background, then `cancel`, which Esc presses — after a
/// blank row, at the foot of `lines`, with their hits at the row they land on.
fn form_buttons(app: &mut App, lines: &mut Vec<Line<'static>>, inner: Rect, verb: &str) {
    let (yes, no) = (format!(" {verb} "), " cancel ".to_string());
    let gap = "        ";
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(yes.clone(), palette::code().add_modifier(Modifier::BOLD).bg(palette::c().sel_bg)),
        Span::raw(gap),
        Span::styled(no.clone(), palette::code()),
    ]));
    let by = inner.y + lines.len() as u16 - 1;
    if by < inner.bottom() {
        let (yw, nw) = (display_width(&yes) as u16, display_width(&no) as u16);
        app.hits.push((Rect { x: inner.x, y: by, width: yw, height: 1 }, Hit::ConfirmButton(0)));
        app.hits.push((Rect { x: inner.x + yw + gap.len() as u16, y: by, width: nw, height: 1 }, Hit::ConfirmButton(1)));
    }
}

fn modal_box(frame: &mut Frame, app: &mut App, area: Rect, w: u16, h: u16, title: &str) -> Rect {
    let w = (w + 4).min(area.width.saturating_sub(4));
    let h = h.min(area.height.saturating_sub(2));
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height.saturating_sub(h)) / 2, width: w, height: h };
    frame.render_widget(Clear, r);
    let edge = Style::new().fg(palette::c().code).bg(palette::c().modal_bg);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(edge)
        .title(Span::styled(format!(" {title} "), palette::ink().bg(palette::c().modal_bg).add_modifier(Modifier::BOLD)));
    let outer = block.inner(r);
    frame.render_widget(block.style(palette::base().bg(palette::c().modal_bg)), r);
    // A column of the modal's colour between the thick border and the box.
    let outer = Rect { x: outer.x + 1, width: outer.width.saturating_sub(2), ..outer };
    let ibox = Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::new().fg(palette::c().modal_inner).bg(palette::c().modal_bg));
    let inner = ibox.inner(outer);
    frame.render_widget(ibox.style(palette::base().bg(palette::c().modal_bg)), outer);
    app.hits.push((r, Hit::Modal));
    app.modal_rect = Some(r);
    Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner }
}

fn draw_modal(frame: &mut Frame, app: &mut App, area: Rect) {
    app.modal_rect = None;
    draw_modal_body(frame, app, area);
    // Its own colour, under everything drawn on the chrome's.
    if let Some(r) = app.modal_rect {
        look::paint(frame.buffer_mut(), r, palette::c().modal_bg);
    }
}

fn draw_modal_body(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(modal) = app.modal.clone() else { return };
    match modal {
        // An inserted artifact's text, the path under the title.
        Modal::Artifact { title, path, lines, scroll } => {
            // As wide as its widest line (a design mockup is 120 columns,
            // task 0168), never narrower than 96 and never wider than the
            // screen: a line crops only when the screen cannot hold it.
            let widest = lines.iter().map(|l| display_width(l)).max().unwrap_or(0);
            let w = ((widest + 6).max(96) as u16).min(area.width.saturating_sub(4));
            let rows = (area.height.saturating_sub(10) as usize).clamp(3, 40);
            let tw = (w as usize).saturating_sub(6).max(10);
            // The path crops so its line count always stands.
            let count = format!("  {} lines", lines.len());
            let shown = crop(&crate::cauldron::artpage::tilde(&path), tw.saturating_sub(display_width(&count)));
            let mut out: Vec<Line> = vec![Line::from(vec![Span::styled(shown, palette::code()), Span::styled(count, palette::muted())]), Line::default()];
            for l in lines.iter().skip(scroll).take(rows) {
                out.push(Line::from(Span::styled(crop(l, tw), palette::ink2())));
            }
            let inner = modal_box(frame, app, area, w, out.len() as u16 + 4, &title);
            frame.render_widget(Paragraph::new(out).style(palette::base()), inner);
        }
        // A table's row, opened (msg 235): every setting it has, in the
        // Config row grammar, the selected one a background.
        Modal::Settings { page, sel } => {
            let w = 108u16.min(area.width.saturating_sub(8));
            let tw = (w as usize).saturating_sub(4).max(20);
            let (lines, ranges, tips, hotspots) = modal_entry_lines(app, &page, sel, tw);
            let rows = (area.height.saturating_sub(8) as usize).max(3);
            let shown = lines.len().min(rows);
            let title = app.row_title(&page);
            let inner = modal_box(frame, app, area, w, shown as u16 + 4, &title);
            let h = inner.height as usize;
            let r = ranges.get(sel).cloned().unwrap_or(0..1);
            let mut top = r.start.saturating_sub(h.saturating_sub(2));
            if r.end > top + h {
                top = r.end.saturating_sub(h);
            }
            for (i, rr) in ranges.iter().enumerate() {
                for line in rr.clone() {
                    if line >= top && line < top + h {
                        app.hits.push((Rect { x: inner.x, y: inner.y + (line - top) as u16, width: inner.width, height: 1 }, Hit::MRow(i as u16)));
                    }
                }
            }
            for (line, col, ei) in tips {
                if line >= top && line < top + h && (col as u16) < inner.width {
                    app.hits.push((Rect { x: inner.x + col as u16, y: inner.y + (line - top) as u16, width: 1, height: 1 }, Hit::MTip(ei as u16)));
                }
            }
            for (line, col, zw, hit) in hotspots {
                if line >= top && line < top + h && (col as u16) < inner.width {
                    let x = inner.x + col as u16;
                    app.hits.push((Rect { x, y: inner.y + (line - top) as u16, width: (zw as u16).min(inner.right() - x), height: 1 }, hit));
                }
            }
            let own = app.modal_entry().is_some_and(|e| e.is_button());
            frame.render_widget(Paragraph::new(lines).scroll((top as u16, 0)), inner);
            if !own {
                look::paint(frame.buffer_mut(), Rect { y: inner.y + r.start.saturating_sub(top) as u16, height: (r.end - r.start).min(h) as u16, ..inner }.intersection(inner), palette::c().sel_bg);
            }
        }
        Modal::Help { title, text } => {
            let w = 60u16.min(area.width.saturating_sub(4));
            let tw = (w as usize).saturating_sub(6).max(10);
            let mut lines: Vec<Line> = Vec::new();
            for r in wrap_plain(&text, tw) {
                lines.push(Line::from(Span::styled(r, palette::ink())));
            }
            let inner = modal_box(frame, app, area, w, lines.len() as u16 + 4, &title);
            frame.render_widget(Paragraph::new(lines).style(palette::base()), inner);
        }
        // V1.6: a login through the threads daemon. The code he types is
        // drawn as one dot a character — never as itself.
        Modal::Login { title, stage, code, .. } => {
            use crate::cauldron::accounts::{stage_lines, LoginStage};
            let w = 72u16.min(area.width.saturating_sub(4));
            let tw = (w as usize).saturating_sub(6).max(10);
            let mut lines: Vec<Line> = Vec::new();
            for (text, alarm) in stage_lines(&stage) {
                for r in wrap_plain(&text, tw) {
                    lines.push(Line::from(Span::styled(r, if alarm { palette::red() } else { palette::ink() })));
                }
            }
            let hint = match &stage {
                LoginStage::AwaitCode { .. } => {
                    lines.push(Line::from(vec![
                        Span::styled("› ", palette::amber()),
                        Span::styled("•".repeat(code.chars().count()), palette::ink()),
                        Span::styled("▏", palette::amber()),
                    ]));
                    "Enter sign in · Esc cancel the login"
                }
                LoginStage::Done { .. } => "Enter or Esc close",
                _ => "Esc cancel the login",
            };
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(hint, palette::muted())));
            let inner = modal_box(frame, app, area, w, lines.len() as u16 + 4, &title);
            frame.render_widget(Paragraph::new(lines).style(palette::base()), inner);
        }
        Modal::NewCluster { parent, name, boundaries, at, caret, error } => {
            let w = 64u16.min(area.width.saturating_sub(4));
            let tw = (w as usize).saturating_sub(8).max(10);
            // The caret is drawn where it is (F38): the field's text with a
            // CARET mark at the caret, split back out when the row is drawn.
            let with_caret = |t: &str, on: bool| -> String {
                let mut t = t.to_string();
                if on {
                    t.insert(caret.min(t.len()), CARET);
                }
                t
            };
            let mut brows: Vec<String> = Vec::new();
            for para in with_caret(&boundaries, at == 1).split('\n') {
                brows.extend(wrap_plain(para, tw));
            }
            let erows = error.as_ref().map_or(Vec::new(), |e| wrap_plain(e, tw + 4));
            let h = (10 + brows.len().max(3) + if erows.is_empty() { 0 } else { erows.len() + 1 }) as u16;
            // msg 235: "In the modal to create a new cluster, call it, you
            // know, a new cluster, and then a name for the cluster, and where
            // it says what belongs in it in your words, put the parentheses
            // around it, at the end saying the words boundary conditions".
            let title = match parent.as_deref().and_then(|p| app.forest.find(p)) {
                Some(g) => format!("New cluster under {}", app.forest.get(g).name),
                None => "New cluster".to_string(),
            };
            let inner = modal_box(frame, app, area, w, h, &title);
            let label = |t: &str, on: bool| Span::styled(t.to_string(), if on { palette::amber() } else { palette::muted() });
            let row_spans = |lead: &'static str, on: bool, row: String| -> Line<'static> {
                let mut spans = vec![Span::styled(lead, if on { palette::amber() } else { palette::muted() })];
                for (i, part) in row.split(CARET).enumerate() {
                    if i > 0 {
                        spans.push(Span::styled("▏", palette::amber()));
                    }
                    spans.push(Span::styled(part.to_string(), palette::ink()));
                }
                Line::from(spans)
            };
            let mut lines = vec![
                Line::from(label("name", at == 0)),
                row_spans("› ", at == 0, with_caret(&name, at == 0)),
                Line::default(),
                Line::from(label("what belongs in it, in your words (boundary conditions)", at == 1)),
            ];
            for (i, row) in brows.into_iter().enumerate() {
                lines.push(row_spans(if i == 0 { "› " } else { "  " }, at == 1, row));
            }
            if !erows.is_empty() {
                lines.push(Line::default());
                for row in erows {
                    lines.push(Line::from(Span::styled(row, palette::red())));
                }
            }
            form_buttons(app, &mut lines, inner, "create");
            frame.render_widget(Paragraph::new(lines).style(palette::base()), inner);
        }
        // A paste opened (0138 §4.4): its full text, editable, and its three
        // buttons. The head says what it is and its size.
        Modal::Chip { editor, at, confirm, .. } => {
            use crate::cauldron::dump::{thousands, ChipAt};
            let w = 84u16.min(area.width.saturating_sub(4));
            let tw = (w as usize).saturating_sub(6).max(10);
            let body = editor.text();
            let mut head = vec!["paste".to_string()];
            let lines_n = body.lines().count();
            if lines_n > 1 {
                head.push(format!("{lines_n} lines"));
            }
            head.push(format!("{} chars", thousands(body.chars().count())));
            let layout = editor.layout(tw);
            let (cr, cc) = editor.caret(tw);
            let rows = 10usize;
            let top = cr.saturating_sub(rows - 1);
            let mut lines: Vec<Line> = layout.iter().skip(top).take(rows).map(|r| Line::from(Span::styled(editor.row_text(*r).to_string(), palette::ink()))).collect();
            while lines.len() < rows.min(layout.len().max(3)) {
                lines.push(Line::default());
            }
            let text_rows = lines.len();
            lines.push(Line::default());
            let buttons = [(ChipAt::Insert, "insert as text"), (ChipAt::Keep, "keep"), (ChipAt::Discard, if confirm { "discard? Enter again" } else { "discard" })];
            let mut spans = Vec::new();
            let mut xs = Vec::new();
            let mut x = 0usize;
            for (i, (b, label)) in buttons.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw("        "));
                    x += 8;
                }
                let on = at == *b;
                // The button he is on is its background, never a marker (rule 10).
                let text = format!(" {label} ");
                let style = if on { palette::code().add_modifier(Modifier::BOLD).bg(palette::c().sel_bg) } else if *b == ChipAt::Discard && confirm { palette::red() } else { palette::code() };
                xs.push((x, display_width(&text)));
                x += display_width(&text);
                spans.push(Span::styled(text, style));
            }
            lines.push(Line::from(spans));
            let inner = modal_box(frame, app, area, w, lines.len() as u16 + 4, &head.join(" · "));
            let by = inner.y + text_rows as u16 + 1;
            for (i, (bx, bw)) in xs.into_iter().enumerate() {
                if by < inner.bottom() {
                    app.hits.push((Rect { x: inner.x + bx as u16, y: by, width: bw as u16, height: 1 }, Hit::ChipButton(i as u8)));
                }
            }
            frame.render_widget(Paragraph::new(lines).style(palette::base()), inner);
            if at == ChipAt::Text && cr >= top && cr - top < text_rows {
                frame.set_cursor_position(Position { x: inner.x + cc as u16, y: inner.y + (cr - top) as u16 });
            }
        }
        Modal::Confirm { title, body, error, .. } => {
            let w = 60u16.min(area.width.saturating_sub(4));
            let tw = (w as usize).saturating_sub(6).max(10);
            let mut lines: Vec<Line> = Vec::new();
            for row in &body {
                if row.is_empty() {
                    lines.push(Line::default());
                }
                for r in wrap_plain(row, tw) {
                    lines.push(Line::from(Span::styled(r, palette::ink())));
                }
            }
            if let Some(err) = &error {
                lines.push(Line::default());
                for r in wrap_plain(err, tw) {
                    lines.push(Line::from(Span::styled(r, palette::red())));
                }
            }
            // Its buttons (0211 R10): what it asks, which Enter presses, on
            // its background; then cancel, which Esc presses.
            lines.push(Line::default());
            let verb = title.split_whitespace().next().unwrap_or("confirm").to_lowercase();
            let (yes, no) = (format!(" {verb} "), " cancel ".to_string());
            let gap = "        ";
            lines.push(Line::from(vec![
                Span::styled(yes.clone(), palette::code().add_modifier(Modifier::BOLD).bg(palette::c().sel_bg)),
                Span::raw(gap),
                Span::styled(no.clone(), palette::code()),
            ]));
            let inner = modal_box(frame, app, area, w, lines.len() as u16 + 4, &title);
            let by = inner.y + lines.len() as u16 - 1;
            if by < inner.bottom() {
                let (yw, nw) = (display_width(&yes) as u16, display_width(&no) as u16);
                app.hits.push((Rect { x: inner.x, y: by, width: yw, height: 1 }, Hit::ConfirmButton(0)));
                app.hits.push((Rect { x: inner.x + yw + gap.len() as u16, y: by, width: nw, height: 1 }, Hit::ConfirmButton(1)));
            }
            frame.render_widget(Paragraph::new(lines).style(palette::base()), inner);
        }
        Modal::Bus { stack } => {
            // Laid out as wide as the modal's inside is (`modal_box`: eight
            // columns of borders and margins), the screen permitting.
            let width = ((crate::cauldron::bus::BUS_WIDTH + 8).min(area.width.saturating_sub(4) as usize)).saturating_sub(8).max(20);
            let Some(laid) = app.bus_laid(width) else { return };
            let (_, sel, scroll) = stack.last().cloned().unwrap_or((crate::cauldron::bus::BusPage::Run(String::new()), 0, 0));
            let sel = sel.min(laid.links.len().saturating_sub(1));
            let rows = (area.height.saturating_sub(8) as usize).clamp(4, 44);
            // The selected link is kept in sight; PgUp PgDn move the view.
            let mut top = scroll.min(laid.lines.len().saturating_sub(rows));
            if let Some((r, _)) = laid.links.get(sel) {
                if r.start < top {
                    top = r.start;
                } else if r.end > top + rows {
                    top = r.end - rows;
                }
            }
            if let Some(Modal::Bus { stack }) = &mut app.modal {
                if let Some(t) = stack.last_mut() {
                    t.2 = top;
                }
            }
            let depth = stack.len();
            let title = if depth > 1 { format!("{} · {depth} deep", laid.title) } else { laid.title.clone() };
            let shown = laid.lines.len().min(rows);
            let inner = modal_box(frame, app, area, width as u16 + 4, shown as u16 + 4, &title);
            let lines: Vec<Line> = laid.lines.iter().skip(top).take(inner.height as usize).cloned().collect();
            frame.render_widget(Paragraph::new(lines).style(palette::base()), inner);
            for (i, (r, _)) in laid.links.iter().enumerate() {
                for row in r.clone() {
                    if row >= top && row < top + inner.height as usize {
                        let y = inner.y + (row - top) as u16;
                        app.hits.push((Rect { x: inner.x, y, width: inner.width, height: 1 }, Hit::BusLink(i as u16)));
                    }
                }
            }
            if let Some((r, _)) = laid.links.get(sel) {
                paint_rows(frame, inner, r.clone(), top, true);
            }
        }
        // DEV ONLY — NOT FOR THE PRODUCTION VERSION (msg 235).
        Modal::DevRename { key, name } => {
            let inner = modal_box(frame, app, area, 44, 7, &format!("Rename {key} · dev only"));
            let mut lines = vec![Line::from(vec![Span::styled("› ", palette::amber()), Span::styled(name.clone(), palette::ink()), Span::styled("▏", palette::amber())])];
            form_buttons(app, &mut lines, inner, "rename");
            frame.render_widget(Paragraph::new(lines).style(palette::base()), inner);
        }
        Modal::NewCauldron { name, error }
        | Modal::AddAccount { name, error, .. }
        | Modal::AddItem { name, error, .. }
        | Modal::Name { name, error, .. } => {
            let title = match &app.modal {
                Some(Modal::Name { title, .. }) => title.clone(),
                Some(Modal::AddAccount { pool, .. }) => format!("Add an account to {}", crate::cauldron::menu::pool_label(pool)),
                Some(Modal::AddItem { list, .. }) => format!("Add to {}", list.def().label),
                _ => "Name the new cauldron".to_string(),
            };
            let erows = error.as_ref().map_or(0, |e| wrap_plain(e, 44).len() + 1) as u16;
            let inner = modal_box(frame, app, area, 44, 7 + erows, &title);
            let mut lines = vec![Line::from(vec![
                Span::styled("› ", palette::amber()),
                Span::styled(name.clone(), palette::ink()),
                Span::styled("▏", palette::amber()),
            ])];
            if let Some(err) = &error {
                lines.push(Line::default());
                for row in wrap_plain(err, inner.width as usize) {
                    lines.push(Line::from(Span::styled(row, palette::red())));
                }
            }
            // What Enter does, in one word (0214 A6).
            let verb = match title.split_whitespace().next().map(str::to_lowercase).as_deref() {
                Some("rename") => "rename",
                Some("register") => "register",
                Some("add") => "add",
                _ => "create",
            };
            form_buttons(app, &mut lines, inner, verb);
            frame.render_widget(Paragraph::new(lines).style(palette::base()), inner);
        }
        Modal::Picker { title, options, selected, for_ } => {
            // A setting's choices are said in words, as its row says them
            // (task 0209 D11); what is picked is still the option.
            let options: Vec<String> = match &for_ {
                crate::cauldron::menu::PickFor::Setting { bound } if matches!(bound.def().ty, Ty::Choice(_) | Ty::PoolRef { .. } | Ty::AccountRef) => {
                    options.iter().map(|o| crate::cauldron::settings::show(bound.def(), &crate::cauldron::settings::Val::Str(o.clone()))).collect()
                }
                _ => options,
            };
            let w = options.iter().map(|o| display_width(o)).max().unwrap_or(0).max(display_width(&title) + 4) + 8;
            let h = (options.len() as u16 + 4).min(area.height.saturating_sub(2));
            let inner = modal_box(frame, app, area, w as u16, h, &title);
            let rows = inner.height as usize;
            let top = selected.saturating_sub(rows.saturating_sub(1));
            let mut lines = Vec::new();
            for (i, o) in options.iter().enumerate().skip(top).take(rows) {
                let pad = (inner.width as usize).saturating_sub(display_width(o) + 2);
                let style = if i == selected { palette::ink().add_modifier(Modifier::BOLD).bg(palette::c().sel_bg) } else { palette::ink() };
                let line = Line::from(Span::styled(format!("  {o}{}", " ".repeat(pad)), style));
                let y = inner.y + lines.len() as u16;
                lines.push(line);
                app.hits.push((Rect { x: inner.x, y, width: inner.width, height: 1 }, Hit::Choice(i)));
            }
            frame.render_widget(Paragraph::new(lines).style(palette::base()), inner);
        }
        Modal::Palette { scroll } => {
            let keys_w = KEYMAP.iter().map(|(k, _)| display_width(k)).max().unwrap_or(0) + 3;
            let what_w = KEYMAP.iter().map(|(_, w)| display_width(w)).max().unwrap_or(0);
            let inner = modal_box(frame, app, area, (keys_w + what_w + 4) as u16, KEYMAP.len() as u16 + 4, "Keys");
            let lines: Vec<Line> = KEYMAP
                .iter()
                .skip(scroll)
                .map(|(k, what)| {
                    Line::from(vec![
                        Span::styled(format!("{k:<keys_w$}"), palette::code()),
                        Span::styled(what.to_string(), palette::ink()),
                    ])
                })
                .collect();
            frame.render_widget(Paragraph::new(lines).style(palette::base()), inner);
        }
    }
}

// ── the bucket ─────────────────────────────────────────────────────────────

/// THE BUCKET (msg 235: "The bucket is too downplayed ... It needs to look
/// like a text area that we are supposed to interact with ... make use of
/// the same border"): the bottom box of the Surface tab, its legend `bucket`
/// at the top right and `⧉ copy` once at the bottom left (b964d571 msg 23: "move
/// the bucket copy button to the bottom left"), now at the BOTTOM RIGHT and
/// drawn as a control set into the border, `┤ ⧉ copy ├`, never in the chip
/// style (8e4fca10 msg 457: "change the style of the copy button in the
/// bucket to be visually distinct from the references that are being dropped
/// into the bucket ... move it to the bottom right") — and on its top edge,
/// at the middle, the grabber that drags its height. Its border is amber
/// while it holds the keys; what the last event said is on its bottom edge,
/// at the left, cropped short of the copy. Inside, the text: no gutter, no ordinals (msg 235:
/// "we're still gonna give all of it to an agent that splits it all out by
/// span anyway. So ... we're actually gonna take it out"). Pastes and
/// references are drawn in the tag style, and a click on a paste opens it.
fn draw_bucket(frame: &mut Frame, app: &mut App, area: Rect, rows: usize) {
    app.hits.push((area, Hit::Input));
    app.chip_hits.clear();
    let focused = app.focus == Focus::Input && app.modal.is_none();
    let edge = if focused { palette::amber() } else { palette::muted() };
    let (said, said_style) = status_line(app);
    let title = legend(app, area, Title::Bucket, if focused { palette::amber().add_modifier(Modifier::BOLD) } else { palette::ink2() });
    // The copy is a control set into the frame — tees in the border's own
    // colour around bold ink words, on no ground — so it never reads as a
    // reference chip (code colour on the code ground) dropped in the text.
    const COPY: &str = " ⧉ copy ";
    let copy_w = display_width(COPY) + 2;
    // What was said stops a column short of the copy, so neither covers
    // the other.
    let said = crop(&said, (area.width as usize).saturating_sub(copy_w + 5));
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(edge)
        .title(title)
        .title_bottom(Line::from(Span::styled(said, said_style)).left_aligned())
        .title_bottom(
            Line::from(vec![
                Span::styled("┤", edge),
                Span::styled(COPY, palette::ink().add_modifier(Modifier::BOLD)),
                Span::styled("├", edge),
            ])
            .right_aligned(),
        );
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // Ratatui draws a right title one cell in from the corner.
    let copy_w = (copy_w as u16).min(area.width.saturating_sub(2));
    let copy_x = area.right().saturating_sub(1 + copy_w);
    let copy_y = area.bottom().saturating_sub(1);
    app.hits.push((Rect { x: copy_x, y: copy_y, width: copy_w, height: 1 }, Hit::BucketCopy));
    // The grabber, at the middle of the top edge: press and drag.
    let mid = Rect { x: area.x + area.width / 2, y: area.y, width: 1, height: 1 };
    frame.render_widget(Paragraph::new(Span::styled(GRABBER, if app.dragging_bucket { palette::amber() } else { palette::ink2() })), mid);
    app.hits.push((mid, Hit::BucketDivider));
    let text_area = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let tw = text_area.width.max(1) as usize;
    app.input_w = tw;
    let layout = app.composer.layout(tw);
    let (caret_row, caret_col) = app.composer.caret(tw);
    // The view stays where it is while the caret is in it, so a click
    // places the caret and the text stays still under the pointer; it moves
    // only as far as brings the caret back in (0225 F3). Never past the end:
    // when the text shrinks, the box stays full.
    let shown = rows.max(1);
    let mut top = app.bucket_top.min(layout.len().saturating_sub(shown));
    if caret_row < top {
        top = caret_row;
    } else if caret_row >= top + shown {
        top = caret_row + 1 - shown;
    }
    app.bucket_top = top;
    app.input_at = (Rect { height: text_area.height.min(rows as u16), ..text_area }, top);
    let lines_text = app.composer.lines().to_vec();
    let tag_ranges: Vec<Vec<(std::ops::Range<usize>, tags::Tag)>> = lines_text.iter().map(|l| tags::scan(l)).collect();
    let mut lines = Vec::new();
    for (i, r) in layout.iter().enumerate().skip(top).take(rows) {
        let row = app.composer.row_text(*r);
        let ranges: Vec<std::ops::Range<usize>> = tag_ranges[r.line].iter().map(|(x, _)| x.clone()).collect();
        lines.push(Line::from(styled_by_ranges(row, r.start, &ranges, palette::ink())));
        // A chip's visible part on this row is a click target.
        for (range, t) in &tag_ranges[r.line] {
            if t.chip_id().is_none() {
                continue;
            }
            let s = range.start.max(r.start);
            let e = range.end.min(r.start + row.len());
            if s >= e {
                continue;
            }
            let x = text_area.x + display_width(&lines_text[r.line][r.start..s]) as u16;
            let cw = display_width(&lines_text[r.line][s..e]) as u16;
            let y = text_area.y + (i - top) as u16;
            let ix = app.chip_hits.iter().position(|c| c == t).unwrap_or_else(|| {
                app.chip_hits.push(t.clone());
                app.chip_hits.len() - 1
            });
            app.hits.push((Rect { x, y, width: cw.min(text_area.right().saturating_sub(x)), height: 1 }, Hit::Chip(ix as u16)));
        }
    }
    frame.render_widget(Paragraph::new(lines).style(palette::base()), text_area);
    // The caret shows only while the bucket has the selection: that, and
    // its amber border, are how he sees where his keys go.
    if focused {
        frame.set_cursor_position(Position {
            x: text_area.x + caret_col as u16,
            y: text_area.y + (caret_row - top) as u16,
        });
    }
}

/// `text` (which starts at byte `offset` of its logical line) as spans, with
/// the bytes inside any of `ranges` (line-relative) in the tag style.
fn styled_by_ranges(
    text: &str,
    offset: usize,
    ranges: &[std::ops::Range<usize>],
    base: Style,
) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_tag = false;
    for (i, c) in text.char_indices() {
        let at = offset + i;
        let in_tag = ranges.iter().any(|r| r.contains(&at));
        if in_tag != cur_tag && !cur.is_empty() {
            out.push(Span::styled(std::mem::take(&mut cur), if cur_tag { tag_style() } else { base }));
        }
        cur_tag = in_tag;
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(Span::styled(cur, if cur_tag { tag_style() } else { base }));
    }
    out
}

fn tag_style() -> Style {
    palette::code().bg(palette::c().code_bg)
}

pub(crate) fn tagged_spans(text: &str, base: Style) -> Vec<Span<'static>> {
    let ranges: Vec<_> = tags::scan(text).into_iter().map(|(r, _)| r).collect();
    styled_by_ranges(text, 0, &ranges, base)
}

// ── helpers ────────────────────────────────────────────────────────────────

pub(crate) fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// A paragraph wrapped to `width`, indented by `indent` columns.
pub(crate) fn wrap(out: &mut Vec<Line<'static>>, text: &str, width: usize, indent: usize, style: Style) {
    for row in md::lay(text, width.saturating_sub(indent).max(1), style, palette::code()) {
        let mut spans = vec![Span::raw(" ".repeat(indent))];
        spans.extend(row.spans);
        out.push(Line::from(spans));
    }
}

/// Plain greedy word wrap (no markdown), so tags survive intact for styling.
pub(crate) fn wrap_plain(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let need = display_width(&line) + usize::from(!line.is_empty()) + display_width(word);
            if need > width && !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
            while display_width(&line) > width {
                let cut: String = line.chars().take(width.max(1)).collect();
                line = line[cut.len()..].to_string();
                out.push(cut);
            }
        }
        out.push(line);
    }
    out
}

pub(crate) fn display_width(s: &str) -> usize {
    tui_render::compose::str_width(s)
}

pub(crate) fn crop(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        if display_width(&out) + display_width(c.encode_utf8(&mut [0; 4])) + 1 > max {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use env::paths::Source;
    use env::Environment;

    use super::*;

    fn app(env: &str) -> App {
        static N: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!("rv10a-vw-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&root).unwrap();
        std::env::set_var("CAULDRON_TYPE_ONE", "off");
        let e = Environment { root: root.clone(), root_source: Source::CauldronHome, name: env.into(), name_source: Source::Default, data_root: root.join("data").join(env) };
        App::open(e, crate::cauldron::store::seed_dir(), true).expect("opens")
    }

    /// Every root tab's click target, by its label.
    fn tab_hits(a: &App) -> Vec<(CTab, Rect)> {
        a.hits.iter().filter_map(|(r, h)| if let Hit::CTab(t) = h { Some((*t, *r)) } else { None }).collect()
    }

    /// Row `y` at `w`: the text, and whether every tab of that row's bar is
    /// whole — on screen and as a click target as wide as its label, on that
    /// row. V1.1: row 0 the global bar, row 1 the environment bar.
    fn bar_row(a: &mut App, w: u16, y: usize) -> (String, bool) {
        let row = crate::cauldron::render_to_text(a, w, 24).unwrap().remove(y);
        let hits = tab_hits(a);
        let tabs: &[CTab] = if y == 0 { &CTab::GLOBAL } else { &CTab::ENV };
        let whole = tabs.iter().all(|t| {
            let label = a.named(t.label());
            let lw = display_width(&label) as u16;
            row.contains(&label) && hits.iter().any(|(h, r)| h == t && r.width == lw && r.right() <= w && r.y as usize == y)
        });
        (row, whole)
    }

    fn top(a: &mut App, w: u16) -> (String, bool) {
        bar_row(a, w, 1)
    }

    /// Re-audit 10 D8, with V1.1's bars: at 60 columns (V1.1 spec item 7)
    /// both bars are whole, prod and design, and the environment still leads
    /// its bar — whole where the bar leaves room, else its NAME crops and
    /// never a tab (`draw_env_bar`'s rule).
    #[test]
    fn d8_the_root_tabs_never_crop_at_60_columns() {
        for env in ["prod", "design"] {
            let mut a = app(env);
            let (row, whole) = top(&mut a, 60);
            assert!(whole, "{env}: {row:?}");
            assert!(row.ends_with("Cauldron|Threads|Status|Permissions|Settings"), "{env}: {row:?}");
            assert!(row.trim_start().starts_with(&format!("{env} ")), "{env}: the environment leads, whole: {row:?}");
            let (row, whole) = bar_row(&mut a, 60, 0);
            assert!(whole && row.ends_with("TUI | Devices | Providers | Accounts"), "{env}: {row:?}");
            // Wide, the roomy strip it always had.
            let (row, whole) = top(&mut a, 120);
            assert!(whole && row.ends_with("Cauldron | Threads | Status | Permissions | Settings"), "{env}: {row:?}");
        }
    }

    /// The sweep, 30..=200 columns: wherever the environment bar's tabs and
    /// the dropdown can share the row at all (the tight strip and ` x ▾ `),
    /// every tab is whole; a long environment name crops, the tabs never do.
    /// The global bar is whole wherever its tight strip fits.
    #[test]
    fn d8_tabs_whole_at_every_width_they_can_fit() {
        for env in ["prod", "design", "a-very-long-environment-name"] {
            let mut a = app(env);
            let tight = CTab::ENV.iter().map(|t| display_width(&a.named(t.label()))).sum::<usize>() + CTab::ENV.len() - 1;
            let gtight = CTab::GLOBAL.iter().map(|t| display_width(&a.named(t.label()))).sum::<usize>() + CTab::GLOBAL.len() - 1;
            for w in 30..=200u16 {
                let (row, whole) = top(&mut a, w);
                if w as usize >= tight + 3 + 3 {
                    assert!(whole, "{env} at {w}: {row:?}");
                }
                assert!(row.contains('▾'), "{env} at {w}: the dropdown is always there: {row:?}");
                let (row, whole) = bar_row(&mut a, w, 0);
                if w as usize >= gtight {
                    assert!(whole, "{env} at {w}: {row:?}");
                }
            }
        }
    }
}
