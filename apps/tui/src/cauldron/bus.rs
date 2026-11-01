//! **The event bus, explored** (8e4fca10 msg 235): what Status › Bus opens.
//!
//! > *"I want to be able to, like, explore in the status and see, like,
//! > events. I want to be able to see, you know, they need to be grouped by
//! > type ... if I click on one of those rows, then it opens up a modal
//! > where I can see more information about it. I can see the event,
//! > details, like the payload and the time. I can see any triggers that
//! > got activated. And then when I click on that trigger, I should be able
//! > to see details about it, what the handler was, the corresponding
//! > execution ... I want to see triggers. So I know what triggers are
//! > present and I can explore them, but I don't want to be able to modify
//! > them. Handlers, or this is like the handlers are the ripples ... And
//! > then I'll be able to discover executions depending on which path that
//! > I navigate."*
//!
//! The four first-class things are the EVENT, the TRIGGER (which, on an
//! event's class and condition, wakes a handler), the RIPPLE (the handler)
//! and the EXECUTION (a run). Status › Bus lists them as bands (`status`);
//! a row opens its PAGE here, in one modal, and every thing a page names is
//! a link to its own page: an event links the triggers it fired; a trigger
//! its ripple and — reached from an event — that firing's execution; a
//! ripple its triggers, what it calls and its executions; an execution its
//! ripple and its steps. Nothing here edits anything.
//!
//! The keys are the modal's (`docs/navigation.md` rule 8): ↑ ↓ move between
//! the links, → or Enter opens one onto the stack, ← steps back (on the
//! first page it closes), Esc closes, PgUp PgDn Home End scroll. The mouse:
//! a click selects a link, a click on the selected one opens it; the wheel
//! scrolls; a click outside closes.

use std::ops::Range;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use tui_render::palette;

use crate::cauldron::app::{App, Modal};
use crate::cauldron::look;
use crate::cauldron::ripples::{Event, Fire, Kind, RippleDef, Run, StepRec};
use crate::cauldron::view::{crop, display_width, wrap_plain};

/// A page of the bus modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusPage {
    /// One event: its time, its payload, the triggers it fired.
    Event(String),
    /// One trigger of a ripple (its index), reached from `event` when it is
    /// that event's firing.
    Trigger { ripple: String, trigger: usize, event: Option<String> },
    /// One ripple: a handler.
    Ripple(String),
    /// One execution: a run.
    Run(String),
    /// One step of a run: its path in the run's tree.
    Step(String, Vec<usize>),
    /// One ripple's execution on Status › Ledger (8e4fca10 msg 457): dump
    /// `seq`'s ripple at `path` (`ledger::Dump::exec_at`).
    Exec { seq: u64, path: Vec<usize> },
}

/// A page laid out: its title, its lines, and the lines each link takes.
pub struct Laid {
    pub title: String,
    pub lines: Vec<Line<'static>>,
    pub links: Vec<(Range<usize>, BusPage)>,
}

struct P<'a> {
    app: &'a App,
    w: usize,
    lines: Vec<Line<'static>>,
    links: Vec<(Range<usize>, BusPage)>,
}

fn muted(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), palette::muted())
}
fn ink(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), palette::ink())
}
fn ink2(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), palette::ink2())
}
fn bold(s: impl Into<String>) -> Span<'static> {
    Span::styled(s.into(), palette::ink().add_modifier(Modifier::BOLD))
}

/// `n` and its noun, singular or plural.
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `$0.034`, `$1.84`.
pub fn money(c: f64) -> String {
    if c < 0.1 {
        format!("${c:.3}")
    } else {
        format!("${c:.2}")
    }
}

/// A run's state, as a glyph and its colour.
pub fn state_glyph(state: &str) -> (&'static str, Style) {
    match state {
        "running" => ("▶", palette::code()),
        "failed" => ("✕", palette::red()),
        "parked" => ("◆", palette::amber()),
        "refused" => ("○", palette::muted()),
        _ => ("✓", palette::muted()),
    }
}

/// When an event was, as a row shows it: `14:31:04`, `yesterday 23:40:12`.
pub fn when(day: &str, at: &str) -> String {
    if day == "today" {
        at.to_string()
    } else {
        format!("{day} {at}")
    }
}

/// A ripple's kind, in his words.
pub fn kind_word(k: &str) -> &str {
    match k {
        "type_one" => "type one",
        "type_two" => "type two",
        // `shell` is what runs recorded before V1.9 say.
        "type_three" | "shell" => "type 3",
        _ => "",
    }
}

/// What wakes a ripple, short: `dump.received`, `transaction_committed · idle 10m`;
/// a ripple no trigger wakes is `called` (a type 3 calls it).
pub fn wakes(r: &RippleDef) -> String {
    let w: Vec<String> = r.triggers.iter().filter(|t| t.enabled).map(|t| if t.on == "schedule" { t.schedule.clone() } else { t.on.clone() }).collect();
    if w.is_empty() {
        "called".into()
    } else {
        w.join(" · ")
    }
}

/// What a trigger waits for besides its class: its condition, or its
/// schedule.
pub fn condition(t: &crate::cauldron::ripples::Trigger) -> String {
    if t.on == "schedule" {
        t.schedule.clone()
    } else if t.when.is_empty() {
        "always".into()
    } else {
        t.when.clone()
    }
}

/// What one firing did, in a few words: `▶ r-9c2e · running`,
/// `✓ ran · 41s`, `○ refused: where false`.
pub fn firing(app: &App, f: &Fire) -> Vec<Span<'static>> {
    let (g, st) = state_glyph(&f.state);
    let what = match (&f.run, f.state.as_str()) {
        (_, "refused") => "refused: its condition is false".to_string(),
        (Some(r), s) if app.rstore.run(r).is_some() => format!("{r} · {s}{}", if f.elapsed.is_empty() { String::new() } else { format!(" · {}", f.elapsed) }),
        // No execution on record: the row is no link, and says so by that.
        (_, s) => format!("{s}{}", if f.elapsed.is_empty() { String::new() } else { format!(" · {}", f.elapsed) }),
    };
    vec![Span::styled(format!("{g} "), st), ink2(what)]
}

impl P<'_> {
    fn line(&mut self, l: Line<'static>) {
        self.lines.push(l);
    }
    fn blank(&mut self) {
        self.lines.push(Line::default());
    }
    /// A section head: an always-open band (no chevron: it does not close).
    fn head(&mut self, name: &str, meta: Vec<Span<'static>>) {
        if !self.lines.is_empty() {
            self.blank();
        }
        let l = look::fixed_band_line(0, vec![look::name(name.to_string())], meta, self.w);
        self.line(l);
    }
    fn table(&mut self, rows: Vec<(String, Vec<Span<'static>>)>) {
        for l in look::table(rows, 2, self.w) {
            self.line(l);
        }
    }
    /// A link row: `left`, `right` flush right, opening `to`.
    fn link(&mut self, left: Vec<Span<'static>>, right: Vec<Span<'static>>, to: BusPage) {
        let s = self.lines.len();
        let mut l = vec![Span::raw("  ")];
        l.extend(left);
        let line = look::lr(l, right, self.w);
        self.line(line);
        self.links.push((s..self.lines.len(), to));
    }
    fn para(&mut self, text: &str) {
        for para in text.lines() {
            for row in wrap_plain(para, self.w.saturating_sub(4).max(10)) {
                self.line(Line::from(vec![Span::raw("  "), ink2(row)]));
            }
        }
    }
}

/// Lay out `page` at `width`.
pub fn page(app: &App, page: &BusPage, width: usize) -> Laid {
    let mut p = P { app, w: width, lines: Vec::new(), links: Vec::new() };
    let title = match page {
        BusPage::Event(id) => event_page(&mut p, id),
        BusPage::Trigger { ripple, trigger, event } => trigger_page(&mut p, ripple, *trigger, event.as_deref()),
        BusPage::Ripple(id) => ripple_page(&mut p, id),
        BusPage::Run(id) => run_page(&mut p, id),
        BusPage::Step(id, path) => step_page(&mut p, id, path),
        BusPage::Exec { seq, path } => exec_page(&mut p, *seq, path),
    };
    Laid { title, lines: p.lines, links: p.links }
}

fn gone(p: &mut P, what: &str) -> String {
    p.line(Line::from(muted(format!("{what} is not on the bus any more"))));
    what.to_string()
}

fn event_page(p: &mut P, id: &str) -> String {
    let app = p.app;
    let Some(e) = app.rstore.events().into_iter().find(|e| e.id == id) else { return gone(p, id) };
    let mut rows = vec![("class".to_string(), vec![ink(e.class.clone())]), ("time".to_string(), vec![ink(when(&e.day, &e.at))])];
    if !e.cluster.is_empty() {
        rows.push(("cluster".to_string(), vec![ink(app.cluster_name(&e.cluster))]));
    }
    if !e.cite.is_empty() {
        rows.push(("message".to_string(), vec![ink(e.cite.clone())]));
    }
    rows.push(("id".to_string(), vec![muted(e.id.clone())]));
    p.table(rows);
    // An empty band's metadata (`0 fields`, `0 triggers`) is all it says:
    // no empty-state sentence under it.
    p.head("Payload", vec![muted(count(e.payload.len(), "field", "fields"))]);
    if !e.payload.is_empty() {
        p.table(e.payload.iter().map(|(k, v)| (k.clone(), vec![ink(v.clone())])).collect());
    }
    p.head("Triggers fired", vec![muted(count(e.fired.len(), "trigger", "triggers"))]);
    let rw = e.fired.iter().map(|f| display_width(&f.ripple)).max().unwrap_or(0) + 2;
    for f in &e.fired {
        let left = vec![bold(look::cell(&f.ripple, rw)), muted(format!("trigger {}  ", f.trigger + 1))];
        p.link(left.into_iter().chain(firing(app, &f.fire)).collect(), Vec::new(), BusPage::Trigger { ripple: f.ripple.clone(), trigger: f.trigger, event: Some(e.id.clone()) });
    }
    format!("{} · {}", e.class, when(&e.day, &e.at))
}

fn trigger_page(p: &mut P, ripple: &str, ti: usize, event: Option<&str>) -> String {
    let app = p.app;
    let Some(r) = app.rstore.live.get(ripple).cloned() else { return gone(p, ripple) };
    let Some(t) = r.triggers.get(ti).cloned() else { return gone(p, &format!("{ripple} trigger {}", ti + 1)) };
    p.head("Handler", vec![muted(kind_word(&kind_str(r.kind)).to_string())]);
    p.link(vec![bold(r.id.clone()), muted(format!("  {}", crop(&r.job, p.w.saturating_sub(display_width(&r.id) + 8))))], Vec::new(), BusPage::Ripple(r.id.clone()));
    p.head("Trigger", vec![muted(if t.enabled { "enabled" } else { "disabled" })]);
    let gather = match (t.gather.as_str(), t.gather_k) {
        ("k_of_n", Some(k)) => format!("{k} of n"),
        (g, _) => g.replace('_', " "),
    };
    let mut rows = vec![("matches".to_string(), vec![ink(t.on.clone())])];
    if t.on == "schedule" {
        rows.push(("every".to_string(), vec![ink(t.schedule.clone())]));
    } else {
        rows.push(("when".to_string(), vec![ink(condition(&t))]));
        if !t.fields.is_empty() {
            rows.push(("watches".to_string(), vec![ink(t.fields.join(" · "))]));
        }
    }
    rows.push(("gathers".to_string(), vec![ink(gather)]));
    rows.push(("a newer input".to_string(), vec![ink(t.on_newer.replace('_', " "))]));
    rows.push(("at once".to_string(), vec![ink(if t.max_at_once == 0 { format!("no limit per {}", t.per) } else { format!("{} per {}", t.max_at_once, t.per) })]));
    p.table(rows);
    if !t.bind.is_empty() {
        p.head("Binds", vec![muted(count(t.bind.len(), "slot", "slots"))]);
        p.table(t.bind.iter().map(|b| (b.slot.clone(), vec![ink(format!("{} {}", b.source, b.path).trim().to_string())])).collect());
    }
    let fires = app.rstore.stats(ripple).fires.get(&ti.to_string()).cloned().unwrap_or_default();
    match event.and_then(|id| app.rstore.events().into_iter().find(|e| e.id == id)) {
        // Reached from an event: that firing's execution.
        Some(e) => {
            let f = e.fired.iter().find(|f| f.ripple == ripple && f.trigger == ti).map(|f| f.fire.clone());
            match f {
                Some(f) => {
                    p.head("Execution", vec![muted(when(&e.day, &e.at))]);
                    execution_row(p, &f);
                }
                None => p.head("Execution", vec![muted("not fired")]),
            }
        }
        None => {
            p.head("Fired", vec![muted(count(fires.len(), "time", "times"))]);
            let events = app.rstore.events();
            for f in &fires {
                let ev = events.iter().find(|e| e.fired.iter().any(|x| x.ripple == ripple && x.trigger == ti && x.fire == *f));
                let mut left = vec![ink2(format!("{}  ", look::cell(&when(&f.day, &f.at), 10)))];
                left.extend(firing(app, f));
                match (f.run.as_deref().filter(|r| app.rstore.run(r).is_some()), ev) {
                    (Some(r), _) => p.link(left, Vec::new(), BusPage::Run(r.to_string())),
                    (None, Some(e)) => p.link(left, Vec::new(), BusPage::Event(e.id.clone())),
                    (None, None) => {
                        let mut l = vec![Span::raw("  ")];
                        l.extend(left);
                        p.line(Line::from(l));
                    }
                }
            }
        }
    }
    format!("{} · trigger {} · {}", r.id, ti + 1, t.on)
}

fn kind_str(k: Kind) -> String {
    match k {
        Kind::TypeOne => "type_one",
        Kind::TypeTwo => "type_two",
        Kind::TypeThree => "type_three",
    }
    .to_string()
}

/// One firing's execution: its run, a link to the run's page when recorded.
fn execution_row(p: &mut P, f: &Fire) {
    let app = p.app;
    match f.run.as_deref().and_then(|r| app.rstore.run(r)) {
        Some(run) => {
            let (g, st) = state_glyph(&run.state);
            let id = run.id.clone();
            let line = run.line.clone();
            p.link(vec![Span::styled(format!("{g} "), st), bold(format!("{id}  ")), ink(line)], vec![muted(run.elapsed.clone())], BusPage::Run(id));
        }
        None => {
            let mut l = vec![Span::raw("  ")];
            l.extend(firing(app, f));
            p.line(Line::from(l));
        }
    }
}

fn ripple_page(p: &mut P, id: &str) -> String {
    let app = p.app;
    let Some(r) = app.rstore.live.get(id).cloned() else { return gone(p, id) };
    let st = app.rstore.stats(id);
    let mut rows = vec![("kind".to_string(), vec![ink(r.kind.label().to_string())])];
    if !r.job.is_empty() {
        rows.push(("does".to_string(), vec![ink(r.job.clone())]));
    }
    if let Some(role) = &r.role {
        rows.push(("role".to_string(), vec![ink(role.clone())]));
    }
    rows.push(("enabled".to_string(), vec![ink(if r.enabled { "yes" } else { "no" })]));
    rows.push(("runs".to_string(), vec![ink(format!("{} today · {} this week", st.today, st.week))]));
    p.table(rows);
    // `0 triggers` is all it says: its CALLED BY band names the type 3.
    p.head("Triggers", vec![muted(count(r.triggers.len(), "trigger", "triggers"))]);
    for (ti, t) in r.triggers.iter().enumerate() {
        p.link(vec![bold(format!("{}  ", t.on)), ink2(condition(t))], vec![muted(if t.enabled { "" } else { "disabled" })], BusPage::Trigger { ripple: r.id.clone(), trigger: ti, event: None });
    }
    let calls = app.rstore.live.calls(id);
    if !calls.is_empty() {
        p.head("Calls", vec![muted(count(calls.len(), "ripple", "ripples"))]);
        for c in calls {
            let kind = app.rstore.live.get(&c).map_or(String::new(), |x| x.kind.label().to_string());
            p.link(vec![bold(format!("{c}  ")), muted(kind)], Vec::new(), BusPage::Ripple(c));
        }
    }
    let by = app.rstore.live.called_by(id);
    if !by.is_empty() {
        p.head("Called by", vec![muted(count(by.len(), "ripple", "ripples"))]);
        for (c, case) in by {
            p.link(vec![bold(format!("{c}  ")), muted(case.map_or(String::new(), |k| format!("when {k}")))], Vec::new(), BusPage::Ripple(c));
        }
    }
    let runs: Vec<Run> = app.rstore.runs().into_iter().filter(|x| x.ripple == id).cloned().collect();
    p.head("Executions", vec![muted(count(runs.len(), "recorded", "recorded"))]);
    for run in runs {
        run_row(p, &run);
    }
    format!("{} · {}", r.id, r.kind.label())
}

/// A run as a link row: its state, when, where, what it did.
pub fn run_row_spans(app: &App, run: &Run) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let (g, st) = state_glyph(&run.state);
    let at: String = run.started.chars().take(8).collect();
    let left = vec![Span::styled(format!("{g} "), st), ink2(format!("{}  ", look::cell(&when(&run.day_word(), &at), 10))), bold(format!("{}  ", look::cell(&run.id, 7))), ink(run.line.clone())];
    let right = vec![muted(app.cluster_name(&run.cluster))];
    (left, right)
}

fn run_row(p: &mut P, run: &Run) {
    let (left, right) = run_row_spans(p.app, run);
    p.link(left, right, BusPage::Run(run.id.clone()));
}

fn step_glyph(s: &StepRec) -> (&'static str, Style) {
    state_glyph(&s.state)
}

fn step_right(s: &StepRec) -> String {
    let mut right = Vec::new();
    if s.ms > 0 {
        right.push(if s.ms >= 1000 { format!("{:.1} s", s.ms as f64 / 1000.0) } else { format!("{} ms", s.ms) });
    }
    if let Some(c) = s.cost {
        right.push(money(c));
    }
    right.join("  ")
}

fn run_page(p: &mut P, id: &str) -> String {
    let app = p.app;
    let Some(run) = app.rstore.run(id).cloned() else { return gone(p, id) };
    p.link(vec![muted("ripple   "), bold(run.ripple.clone())], vec![muted(app.rstore.live.get(&run.ripple).map_or(String::new(), |r| r.kind.label().to_string()))], BusPage::Ripple(run.ripple.clone()));
    let mut rows = vec![("cluster".to_string(), vec![ink(app.cluster_name(&run.cluster))]), ("started".to_string(), vec![ink(when(&run.day_word(), &run.started))])];
    if !run.cite.is_empty() {
        rows.push(("message".to_string(), vec![ink(run.cite.clone())]));
    }
    let (g, st) = state_glyph(&run.state);
    rows.push(("state".to_string(), vec![Span::styled(format!("{g} {}", run.state), st)]));
    if !run.elapsed.is_empty() {
        rows.push(("took".to_string(), vec![ink(run.elapsed.clone())]));
    }
    if let Some(c) = run.cost {
        rows.push(("cost".to_string(), vec![ink(money(c))]));
    }
    if !run.account.is_empty() {
        rows.push(("account".to_string(), vec![ink(run.account.clone())]));
    }
    rows.push(("did".to_string(), vec![ink(run.line.clone())]));
    rows.push(("policy".to_string(), vec![muted(app.rstore.run_hash(&run))]));
    p.table(rows);
    p.head("Steps", vec![muted(count(run.tree.len(), "step", "steps"))]);
    for (i, s) in run.tree.iter().enumerate() {
        tree_rows(p, &run, s, vec![i], 0);
    }
    format!("{} · {}", run.id, run.ripple)
}

fn tree_rows(p: &mut P, run: &Run, s: &StepRec, path: Vec<usize>, depth: usize) {
    let (g, gst) = step_glyph(s);
    let indent = "  ".repeat(depth);
    let label = match s.foreach {
        Some(n) => format!("{} ×{n}", s.step),
        None => s.step.clone(),
    };
    let lw = 22usize.saturating_sub(indent.len());
    p.link(vec![Span::raw(indent.clone()), Span::styled(format!("{g} "), gst), bold(format!("{}  ", look::cell(&label, lw))), muted(format!("{}  ", look::cell(kind_word(&s.kind), 9))), ink(s.line.clone())], vec![muted(step_right(s))], BusPage::Step(run.id.clone(), path.clone()));
    for (k, item) in s.items.iter().enumerate() {
        for (j, x) in item.steps.iter().enumerate() {
            if j == 0 {
                p.line(Line::from(vec![Span::raw(format!("    {indent}")), muted(format!("#{} ", k + 1)), ink2(format!("\"{}\"", crop(&item.text, 60)))]));
            }
            tree_rows(p, run, x, vec![path[0], k, j], depth + 2);
        }
    }
}

/// The step at `path` of `tree`: `[i]` a top step, `[i, k, j]` step `j` of
/// iteration `k` of foreach `i`.
pub fn step_at<'a>(tree: &'a [StepRec], path: &[usize]) -> Option<&'a StepRec> {
    match path {
        [i] => tree.get(*i),
        [i, k, j, ..] => tree.get(*i)?.items.get(*k)?.steps.get(*j),
        _ => None,
    }
}

fn step_page(p: &mut P, id: &str, path: &[usize]) -> String {
    let app = p.app;
    let Some(run) = app.rstore.run(id).cloned() else { return gone(p, id) };
    let Some(s) = step_at(&run.tree, path).cloned() else { return gone(p, &format!("a step of {id}")) };
    p.link(vec![muted("run      "), bold(run.id.clone())], vec![muted(run.ripple.clone())], BusPage::Run(run.id.clone()));
    let (g, st) = step_glyph(&s);
    let ms = if s.ms >= 1000 { format!("{:.1} s", s.ms as f64 / 1000.0) } else { format!("{} ms", s.ms) };
    let mut rows = vec![
        ("step".to_string(), vec![ink(s.step.clone())]),
        ("kind".to_string(), vec![ink(kind_word(&s.kind).to_string())]),
        ("state".to_string(), vec![Span::styled(format!("{g} {}", s.state), st)]),
        ("did".to_string(), vec![ink(s.line.clone())]),
        ("latency".to_string(), vec![ink(ms)]),
    ];
    if !s.model.is_empty() {
        rows.push(("model".to_string(), vec![ink(s.model.clone())]));
    }
    if let Some(c) = s.cost {
        rows.push(("cost".to_string(), vec![ink(money(c))]));
    }
    p.table(rows);
    if !s.p.is_empty() {
        let yes = s.p.values().filter(|v| **v >= 0.5).count();
        p.head("Distributions", vec![muted(format!("{} · {yes} true", count(s.p.len(), "answer", "answers")))]);
        p.table(s.p.iter().map(|(k, v)| (k.clone(), vec![ink(format!("true {v:.2} · false {:.2}", 1.0 - v))])).collect());
    }
    if !s.effects.is_empty() {
        p.head("Writes", vec![muted(count(s.effects.len(), "node", "nodes"))]);
        p.table(s.effects.iter().map(|e| (e.node.clone(), vec![ink(format!("rev {} → {}", e.rev_before.map_or("—".into(), |r| r.to_string()), e.rev_after.map_or("—".into(), |r| r.to_string())))])).collect());
    }
    for (title, text) in [("Input", &s.input), ("Output", &s.output)] {
        if text.is_empty() {
            continue;
        }
        p.head(title, vec![muted(count(text.lines().count(), "line", "lines"))]);
        p.para(text);
    }
    format!("{} · {}", run.id, s.step)
}

// ── the ledger's executions ────────────────────────────────────────────────

/// A ripple's TYPE as he says it: `① type 1`, `③ type 3`.
fn type_words(k: Kind) -> String {
    let g = crate::cauldron::ledger::type_glyph(k);
    match k {
        Kind::TypeOne => format!("{g} type 1"),
        Kind::TypeTwo => format!("{g} type 2"),
        Kind::TypeThree => format!("{g} type 3"),
    }
}

/// A link to the execution at `path` of dump `seq`: its state, type, name,
/// what it did, and how long.
fn exec_link(p: &mut P, seq: u64, path: Vec<usize>, e: &crate::cauldron::ledger::Exec, lead: &str) {
    let (g, st) = state_glyph(&e.state);
    let left = vec![muted(lead.to_string()), Span::styled(format!("{g} "), st), Span::styled(format!("{} ", crate::cauldron::ledger::type_glyph(e.kind)), palette::code()), bold(format!("{}  ", e.ripple)), ink2(e.line.clone())];
    p.link(left, vec![muted(crate::cauldron::ledger::took(e.ms))], BusPage::Exec { seq, path });
}

/// EXPLORING a ripple on the Ledger (msg 457): "if I'm looking at a type 1
/// ... you're just going to have an input and an output ... looking at a type
/// 2 is going to look just like an agent. Looking at a type 3 is going to
/// look kind of like a step function". Every page says what it is, when, how
/// long, and links what ran it and what it ran.
fn exec_page(p: &mut P, seq: u64, path: &[usize]) -> String {
    use crate::cauldron::ledger;
    let app = p.app;
    let dumps = ledger::dumps(&app.ledger, &app.sends.sends);
    let Some(d) = dumps.iter().find(|d| d.seq == seq) else { return gone(p, &format!("dump {seq}")) };
    let Some(e) = d.exec_at(path).cloned() else { return gone(p, &format!("a ripple of dump {seq}")) };
    // What ran it, when something did.
    if path.len() > 1 {
        let up = &path[..path.len() - 1];
        if let Some(parent) = d.exec_at(up) {
            exec_link(p, seq, up.to_vec(), parent, "ran by  ");
        }
    }
    let (g, st) = state_glyph(&e.state);
    let mut rows = vec![
        ("dump".to_string(), vec![ink(format!("{seq}  ")), ink2(d.headline())]),
        ("type".to_string(), vec![Span::styled(type_words(e.kind), palette::code())]),
        ("state".to_string(), vec![Span::styled(format!("{g} {}", e.state), st)]),
        ("started".to_string(), vec![ink(ledger::when(ledger::started(d.at, &e)))]),
        ("took".to_string(), vec![ink(format!("{}{}", ledger::took(e.ms), if e.state == "running" { " so far" } else { "" }))]),
    ];
    if !e.model.is_empty() {
        rows.push(("model".to_string(), vec![ink(e.model.clone())]));
    }
    if !e.line.is_empty() {
        rows.push(("did".to_string(), vec![ink(e.line.clone())]));
    }
    p.table(rows);
    match e.kind {
        // A type 1: its input, then its output.
        Kind::TypeOne => {
            for (title, text) in [("Input", &e.input), ("Output", &e.output)] {
                p.head(title, vec![muted(count(text.lines().count(), "line", "lines"))]);
                if text.is_empty() {
                    p.line(Line::from(vec![Span::raw("  "), muted("none")]));
                } else {
                    p.para(text);
                }
            }
        }
        // A type 2: its transcript, as an agent's reads — who spoke, then
        // what, each turn its own block; a tool's call named.
        Kind::TypeTwo => {
            p.head("Transcript", vec![muted(count(e.transcript.len(), "turn", "turns"))]);
            for t in &e.transcript {
                p.blank();
                let who = match t.role.as_str() {
                    "user" => Span::styled("› you", palette::amber().add_modifier(Modifier::BOLD)),
                    "assistant" => Span::styled(format!("● {}", e.ripple), palette::code().add_modifier(Modifier::BOLD)),
                    "tool" => Span::styled(format!("⚙ {}", if t.tool.is_empty() { "tool" } else { &t.tool }), palette::ink2()),
                    _ => Span::styled("◦ system", palette::muted()),
                };
                p.line(Line::from(vec![Span::raw("  "), who]));
                let style = match t.role.as_str() {
                    "assistant" => palette::ink(),
                    "system" | "tool" => palette::muted(),
                    _ => palette::ink2(),
                };
                for para in t.text.lines() {
                    for row in wrap_plain(para, p.w.saturating_sub(6).max(10)) {
                        p.line(Line::from(vec![Span::raw("    "), Span::styled(row, style)]));
                    }
                }
            }
            if e.state == "running" {
                p.blank();
                p.line(Line::from(vec![Span::raw("  "), Span::styled("▶ still running", palette::code())]));
            }
        }
        // A type 3: its steps in order, as a step function — from start to
        // end down one rail, each step its state, its word, what it did, how
        // long; the ripples a step ran hang off it, each a link.
        Kind::TypeThree => {
            p.head("Steps", vec![muted(count(e.steps.len(), "step", "steps"))]);
            p.line(Line::from(vec![Span::raw("  "), muted("◉ start")]));
            let nw = e.steps.iter().map(|s| display_width(&s.name)).max().unwrap_or(4).min(20);
            for s in &e.steps {
                p.line(Line::from(vec![Span::raw("  "), muted("│")]));
                let (g, st) = if s.state == "pending" { ("◇", palette::muted()) } else { state_glyph(&s.state) };
                let ms = if s.ms > 0 { ledger::took(s.ms) } else { String::new() };
                let left = vec![Span::raw("  "), Span::styled(format!("{g} "), st), bold(format!("{}  ", look::cell(&s.name, nw))), muted(format!("{}  ", look::cell(&s.word, 7))), ink2(s.line.clone())];
                p.line(look::lr(left, vec![muted(ms)], p.w));
                let n = s.calls.len();
                for (k, i) in s.calls.iter().enumerate() {
                    let Some(c) = e.children.get(*i) else { continue };
                    let mut cp = path.to_vec();
                    cp.push(*i);
                    exec_link(p, seq, cp, c, if k + 1 == n { "│ ╰─ " } else { "│ ├─ " });
                }
            }
            p.line(Line::from(vec![Span::raw("  "), muted("│")]));
            let end = match e.state.as_str() {
                "running" => Span::styled("▶ running", palette::code()),
                "failed" => Span::styled("✕ stopped", palette::red()),
                _ => muted("◉ end"),
            };
            p.line(Line::from(vec![Span::raw("  "), end]));
        }
    }
    // What it ran that no step names (a type 2's own calls, say).
    let named: Vec<usize> = e.steps.iter().flat_map(|s| s.calls.iter().copied()).collect();
    let loose: Vec<usize> = (0..e.children.len()).filter(|i| !named.contains(i)).collect();
    if !loose.is_empty() {
        p.head("Ran", vec![muted(count(loose.len(), "ripple", "ripples"))]);
        for i in loose {
            let mut cp = path.to_vec();
            cp.push(i);
            let c = e.children[i].clone();
            exec_link(p, seq, cp, &c, "");
        }
    }
    let mut names = vec![format!("dump {seq}")];
    for k in 1..=path.len() {
        if let Some(x) = d.exec_at(&path[..k]) {
            names.push(x.ripple.clone());
        }
    }
    names.join(" › ")
}

// ── keys ─────────────────────────────────────────────────────────────────

impl App {
    /// The bus modal's top page, laid out `width` columns wide (its links
    /// are the same at any width).
    pub fn bus_laid(&self, width: usize) -> Option<Laid> {
        let Some(Modal::Bus { stack }) = &self.modal else { return None };
        let (top, _, _) = stack.last()?;
        Some(page(self, top, width))
    }

    /// Keys in the bus modal (rule 8: it takes every key).
    pub fn bus_modal_key(&mut self, key: KeyEvent) {
        let n = self.bus_laid(BUS_WIDTH).map_or(0, |l| l.links.len());
        let Some(Modal::Bus { stack }) = self.modal.as_mut() else { return };
        let Some((top, sel, scroll)) = stack.last_mut() else {
            self.modal = None;
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.modal = None,
            KeyCode::Left | KeyCode::Backspace => {
                stack.pop();
                if stack.is_empty() {
                    self.modal = None;
                }
            }
            KeyCode::Up => *sel = sel.saturating_sub(1),
            KeyCode::Down => *sel = (*sel + 1).min(n.saturating_sub(1)),
            KeyCode::Home => {
                *sel = 0;
                *scroll = 0;
            }
            KeyCode::End => *sel = n.saturating_sub(1),
            KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
            KeyCode::PageDown | KeyCode::Char(' ') => *scroll += 10,
            KeyCode::Right | KeyCode::Enter => {
                let (at, page) = (*sel, top.clone());
                self.bus_follow(&page, at);
            }
            _ => {}
        }
    }

    /// Open link `i` of the bus page on top: its page goes onto the stack.
    pub fn bus_follow(&mut self, _on: &BusPage, i: usize) {
        let Some(to) = self.bus_laid(BUS_WIDTH).and_then(|l| l.links.into_iter().nth(i)).map(|(_, p)| p) else { return };
        if let Some(Modal::Bus { stack }) = self.modal.as_mut() {
            stack.push((to, 0, 0));
        }
    }

    /// A click on link `i` of the bus page on screen: it opens it.
    pub fn bus_click(&mut self, i: usize) {
        let Some(Modal::Bus { stack }) = self.modal.as_mut() else { return };
        let Some((top, sel, _)) = stack.last_mut() else { return };
        // A link follows at ONE click (0214 A5: an execution row needed two).
        *sel = i;
        let page = top.clone();
        self.bus_follow(&page, i);
    }

    /// The wheel over the bus modal.
    pub fn bus_scroll(&mut self, d: isize) {
        if let Some(Modal::Bus { stack }) = self.modal.as_mut() {
            if let Some((_, _, scroll)) = stack.last_mut() {
                *scroll = (*scroll as isize + 3 * d).max(0) as usize;
            }
        }
    }
}

/// The width the bus modal lays its pages out at.
pub const BUS_WIDTH: usize = 92;

/// The events of class `class`, newest first.
pub fn of_class(events: &[Event], class: &str) -> Vec<Event> {
    events.iter().filter(|e| e.class == class).cloned().collect()
}
