//! Drawing one frame of the pool board. Pure over [`App`]: no clock (the app
//! carries `now`), no socket, no terminal.

use chrono::{DateTime, Duration, Utc};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use tui_render::palette;

use super::app::{App, LinkState, Prompt};
use super::model::{Board, Card, Gauge, Homed, Standing, Tone, AXES};
use shapewire::subtree::Rollup;

fn style(t: Tone) -> Style {
    match t {
        Tone::Good => palette::green(),
        Tone::Warn => palette::amber(),
        Tone::Bad => palette::red().add_modifier(Modifier::BOLD),
        Tone::Muted => palette::muted(),
        Tone::Plain => palette::ink(),
    }
}

/// `42m`, `15h 42m`, `3d 07h`; a negative span clamps to `0m` (`fmt.py:53`).
pub fn countdown(d: Duration) -> String {
    let m = d.num_minutes().max(0);
    let (days, hours, mins) = (m / 1440, (m % 1440) / 60, m % 60);
    if days > 0 {
        format!("{days}d {hours:02}h")
    } else if hours > 0 {
        format!("{hours}h {mins:02}m")
    } else {
        format!("{mins}m")
    }
}

pub fn ago(now: DateTime<Utc>, t: DateTime<Utc>) -> String {
    countdown(now - t)
}

/// `▕████▌░░▏` with eighth-block resolution, `width` cells inside the rails.
pub fn bar(pct: f64, width: usize) -> String {
    const EIGHTHS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let p = pct.clamp(0.0, 100.0) / 100.0;
    let total = (p * width as f64 * 8.0).round() as usize;
    let (full, rem) = (total / 8, total % 8);
    let mut s = String::from("▕");
    s.extend(std::iter::repeat('█').take(full.min(width)));
    if full < width {
        s.push(if rem == 0 { '░' } else { EIGHTHS[rem] });
        s.extend(std::iter::repeat('░').take(width - full - 1));
    }
    s.push('▏');
    s
}

/// Blue (ink here) below 70%, amber 70–89%, red at 90% and over (`fmt.py:127`).
fn bar_tone(pct: f64) -> Tone {
    if pct >= 90.0 {
        Tone::Bad
    } else if pct >= 70.0 {
        Tone::Warn
    } else {
        Tone::Plain
    }
}

fn header(app: &App, b: &Board) -> Line<'static> {
    let mut s = vec![Span::styled(" pool ", palette::header())];
    if b.totals.roster_known {
        let t = &b.totals;
        s.push(Span::styled(
            format!(" ▶ {} running ({} subagent)", t.running, t.running_sub),
            palette::green(),
        ));
        s.push(Span::styled(format!(" · ● {} msgs queued", t.queued_msgs), palette::ink()));
        if t.parked > 0 {
            // **WHOSE PARKED THREADS THESE ARE.** Two Sept-25 test threads
            // with no parent read as "◆ 2 parked" beside a live subagent run,
            // and nothing said they were not its children.
            s.push(Span::styled(
                format!(" · ◆ {} parked ({} subagent, {} no parent)", t.parked, t.parked_sub, t.parked_root),
                palette::amber(),
            ));
        }
    } else {
        s.push(Span::styled(" running ? · msgs queued ?", palette::muted()));
    }
    let (link, tone) = match &app.link {
        LinkState::Live => ("threads ● live".to_string(), Tone::Good),
        LinkState::Connecting => ("threads connecting…".to_string(), Tone::Muted),
        LinkState::Offline(why) => (format!("threads OFFLINE — {why}"), Tone::Bad),
    };
    s.push(Span::styled("   ", palette::ink()));
    s.push(Span::styled(link, style(tone)));
    s.push(Span::styled(format!(" · {}", app.endpoint), palette::muted()));
    Line::from(s)
}

fn warnings(b: &Board) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    if let Some(why) = &b.dead {
        out.push(Line::from(Span::styled(
            format!(" ✕ the accounts fold DIED — {why}; everything below is last known"),
            style(Tone::Bad),
        )));
    }
    for c in b.cards.iter().filter(|c| !c.disabled()) {
        if let Some((w, t)) = c.slot.warning() {
            let mark = if t == Tone::Bad { "✕" } else { "⚠" };
            out.push(Line::from(Span::styled(format!(" {mark} {} {w}", c.title()), style(t))));
        }
        if let Some(why) = &c.not_answering {
            out.push(Line::from(Span::styled(
                format!(" ✕ {} is NOT ANSWERING — {why}", c.title()),
                style(Tone::Bad),
            )));
        }
    }
    out
}

fn gauge_line(g: &Gauge, label: &str, now: DateTime<Utc>, width: usize) -> Line<'static> {
    let mut s = vec![Span::styled(format!("      {label:<8}"), palette::muted())];
    match g.pct {
        None => s.push(Span::styled("—", palette::muted())),
        Some(p) => {
            s.push(Span::styled(bar(p, width), style(bar_tone(p))));
            let approx = if g.confidence.is_some_and(|c| c < 0.9) { "~" } else { " " };
            s.push(Span::styled(format!(" {approx}{:>3.0}%", p), style(bar_tone(p))));
            // A meter says when it resets, full or not. AT LIMIT is never read
            // off a gauge (V1-CLEAN2, V1-CLEAN1-I B2): it is the daemon's
            // `limit` cell, drawn on the card's own line (`Card.limit_until`).
            if let Some(r) = g.resets_at {
                s.push(Span::styled(format!("  resets in {}", countdown(r - now)), palette::muted()));
            }
        }
    }
    Line::from(s)
}

fn card_lines(c: &Card, n: usize, selected: bool, now: DateTime<Utc>, wide: bool) -> Vec<Line<'static>> {
    let marker = if selected { "›" } else { " " };
    let name_style = if selected { palette::ink().add_modifier(Modifier::BOLD) } else { palette::ink() };
    let mut title = vec![
        Span::styled(format!("{marker} {n} "), palette::muted()),
        // At least one space after the name, however long it is.
        Span::styled(format!("{:<14} ", c.title()), name_style),
    ];
    if let Standing::Disabled { at, note } = &c.standing {
        title.push(Span::styled("DISABLED", palette::amber().add_modifier(Modifier::BOLD)));
        let mut rest = String::from(" — out of the pool");
        if let Some(t) = at {
            rest.push_str(&format!(", {} ago", ago(now, *t)));
        }
        if let Some(n) = note {
            rest.push_str(&format!(" · {n}"));
        }
        title.push(Span::styled(rest, palette::muted()));
        title.push(Span::styled("   press d to reactivate", palette::muted()));
        return vec![Line::from(title)];
    }

    let (badge, tone) = c.slot.badge();
    title.push(Span::styled(badge, style(tone)));
    if c.is_live_login {
        title.push(Span::styled(" · ", palette::muted()));
        title.push(Span::styled("LIVE LOGIN", palette::ink().add_modifier(Modifier::BOLD)));
    }
    if c.registered == Some(false) {
        title.push(Span::styled(" · NOT REGISTERED", palette::muted()));
    }
    title.push(Span::styled(format!(" · ▶ {} running", c.running()), palette::green()));
    if c.retrying() > 0 {
        title.push(Span::styled(format!(" · ⟳ {} retrying", c.retrying()), palette::amber()));
    }
    if let Some(s) = c.share {
        title.push(Span::styled(format!(" · {:.0}% share", s * 100.0), palette::muted()));
    }
    match (c.pressure, &c.unranked_reason) {
        (Some(p), _) => title.push(Span::styled(format!(" · pressure {p:.2}×"), palette::muted())),
        // ∞ arrives as `pressure: null` + `infinite: true` (R9.PR1); it used
        // to fall through to `_` and draw nothing for the account placement
        // ranks first.
        (None, _) if c.infinite => title.push(Span::styled(" · pressure ∞".to_string(), palette::muted())),
        (None, Some(r)) => title.push(Span::styled(format!(" · unranked ({r})"), palette::muted())),
        _ => {}
    }
    if let Some(exp) = c.login_expires {
        let days = (exp - now).num_days();
        title.push(Span::styled(format!(" · login {days}d"), palette::muted()));
    }
    let mut out = vec![Line::from(title)];
    if let Some(u) = c.limit_until {
        let prose = c.limit_prose.clone().map(|p| format!(" · {p}")).unwrap_or_default();
        out.push(Line::from(Span::styled(
            format!("      AT LIMIT — back in {}{prose}", countdown(u - now)),
            style(Tone::Bad),
        )));
    }
    let width = if wide { 25 } else { 12 };
    for (g, (_, label)) in c.gauges.iter().zip(AXES) {
        out.push(gauge_line(g, label, now, width));
    }
    out.extend(homed_lines(&c.homed, now));
    out
}

/// One homed thread's line: mark, name, state — and for a parked one its
/// reason, since when, and whose it is.
fn homed_line(h: &Homed, indent: &str, show_lineage: bool) -> Line<'static> {
    // The mark and the word are the thread's status as the DAEMON rolled it
    // (`rolled`, TUI-P1d) — a queued parent with a running subagent is drawn
    // running because the daemon says its subtree is active.
    let word = h.rollup.word(&h.state).to_string();
    let (mark, st) = rollup_mark(h.rollup, &h.state, h.executing);
    let mut s = vec![Span::styled(indent.to_string(), palette::muted())];
    if word == "parked" {
        let mut t = format!("{mark} parked · {}", h.label());
        t.push_str(&format!(" · {}", h.detail.as_deref().unwrap_or("reason not read yet")));
        if let Some(at) = h.since {
            t.push_str(&format!(" · since {}", at.format("%b %d")));
        }
        if show_lineage {
            match (&h.parent_id, &h.parent_name) {
                (None, _) => t.push_str(" · no parent"),
                (Some(_), Some(p)) => t.push_str(&format!(" · ↳ {p}")),
                (Some(_), None) => t.push_str(" · ↳ a thread"),
            }
        }
        s.push(Span::styled(t, st));
    } else {
        s.push(Span::styled(format!("{mark} {} {}", h.label(), word), st));
    }
    if h.queued > 0 {
        s.push(Span::styled(format!(" +{}q", h.queued), palette::muted()));
    }
    Line::from(s)
}

/// The mark and colour of a thread's shown status (TUI-P1d: read off the
/// daemon's `rolled`, see `model::shown`). A thread decided by its OWN state
/// is drawn exactly as before: `▶` only while executing, `◆` when parked. The
/// daemon saying its subtree is active is `▶`. There is no parked rollup — an
/// error does not roll up (R9.TS1) — so `SubtreeParked` is never produced and
/// draws as its own word.
fn rollup_mark(r: Rollup, own: &str, executing: bool) -> (&'static str, Style) {
    match r {
        Rollup::SubtreeActive => ("▶", palette::green()),
        _ if executing => ("▶", palette::green()),
        _ if own == "parked" => ("◆", palette::amber()),
        _ => ("·", palette::ink()),
    }
}

/// **THE HOMED THREADS, GROUPED UNDER THEIR PARENTS** (TUI-SUBAGENTS §3.6).
/// A root is drawn flat; children are drawn indented under one line naming
/// their parent and how many of its subagents are here. The parent itself is
/// usually NOT on the roster — a parent waiting on children is idle and holds
/// no slot — so its line is drawn from its name alone.
fn homed_lines(homed: &[Homed], _now: DateTime<Utc>) -> Vec<Line<'static>> {
    const FIRST: &str = "      homed   ";
    const NEXT: &str = "              ";
    const CHILD: &str = "                ";
    let mut out = Vec::new();
    let lead = |out: &mut Vec<Line<'static>>| if out.is_empty() { FIRST } else { NEXT };
    // Parents in first-seen order.
    let mut parents: Vec<&str> = Vec::new();
    for h in homed {
        if let Some(p) = h.parent_id.as_deref() {
            if !parents.contains(&p) {
                parents.push(p);
            }
        }
    }
    for p in &parents {
        let kids: Vec<&Homed> = homed.iter().filter(|h| h.parent_id.as_deref() == Some(*p)).collect();
        let name = kids[0].parent_name.clone().unwrap_or_else(|| "a thread".to_string());
        let l = lead(&mut out);
        // The parent line says what the daemon's `rolled` says for its WHOLE
        // subtree — not only the kids homed on this card (TUI-P1d).
        let (text, st) = match kids[0].parent_rollup {
            Some(r) if r.active() => (format!("▶ {name} — active · {} here", kids.len()), palette::green()),
            _ => (format!("⋯ {name} — waiting on {}", kids.len()), palette::ink()),
        };
        out.push(Line::from(vec![
            Span::styled(l.to_string(), palette::muted()),
            Span::styled(text, st),
        ]));
        for k in kids {
            out.push(homed_line(k, CHILD, false));
        }
    }
    for h in homed.iter().filter(|h| h.parent_id.is_none()) {
        let l = lead(&mut out);
        out.push(homed_line(h, l, true));
    }
    out
}

pub const HINTS: &str = " j/k move · 1-9 pick · d disable/reactivate · s set machine login · S share · q quit";

pub fn draw(f: &mut Frame, app: &App) {
    let b = app.board();
    let area = f.area();
    let [top, body, foot] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(2)]).areas(area);
    f.render_widget(Paragraph::new(header(app, &b)), top);

    let mut lines = vec![];
    if let Some(m) = &b.machine_login {
        lines.push(Line::from(Span::styled(format!(" {m}"), palette::muted())));
    }
    lines.extend(warnings(&b));
    lines.push(Line::from(""));
    let sel = app.selected_index(&b);
    let mut sel_line = 0usize;
    if b.cards.is_empty() {
        let msg = if !b.exported {
            " ◌ waiting for the daemon's first export…"
        } else {
            " the daemon's accounts fold holds no accounts"
        };
        lines.push(Line::from(Span::styled(msg, palette::muted())));
    }
    let wide = area.width >= 100;
    for (i, c) in b.cards.iter().enumerate() {
        if Some(i) == sel {
            sel_line = lines.len();
        }
        lines.extend(card_lines(c, i + 1, Some(i) == sel, app.now, wide));
    }
    let h = body.height as usize;
    let scroll = if sel_line + 6 > h { sel_line + 6 - h } else { 0 };
    f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), body);

    let status = match &app.prompt {
        Prompt::Confirm(a) => Line::from(vec![
            Span::styled(format!(" {} ", a.question_as(&app.shown(a.label()))), palette::amber()),
            Span::styled("y/enter yes · any other key no", palette::muted()),
        ]),
        Prompt::Share { label, buf } => Line::from(Span::styled(
            format!(" {} share %: {buf}▏ (enter to confirm, empty clears, esc cancels)", app.shown(label)),
            palette::amber(),
        )),
        Prompt::None => match &app.note {
            Some((n, t)) => Line::from(Span::styled(format!(" {n}"), style(*t))),
            None => Line::from(""),
        },
    };
    f.render_widget(
        Paragraph::new(vec![status, Line::from(Span::styled(HINTS, palette::muted()))]),
        foot,
    );
}

#[cfg(test)]
mod tests {
    use super::super::model::{project, Fold, NON_IDLE, SUMMARY, THREADS};
    use super::*;
    use serde_json::json;

    /// A board whose one account `acct` homes `entries`: `(id, state,
    /// executing, ancestors nearest first)`. `rolled` is the daemon's
    /// `rolled.status` per thread id, held on the keyed `threads` fold exactly
    /// as the daemon serves it; a thread not named has no `rolled` read yet.
    fn board(entries: &[(&str, &str, bool, &[&str])], rolled: &[(&str, &str)]) -> Board {
        let mut accounts = Fold::new("accounts");
        accounts.rows.insert("acct".into(), json!({}));
        let mut non_idle = Fold::new(NON_IDLE);
        let threads: Vec<_> = entries
            .iter()
            .map(|(id, s, e, anc)| {
                json!({"id": id, "account": "acct", "state": s, "executing": e, "queued": 0,
                       "parent_id": anc.first(), "ancestors": anc})
            })
            .collect();
        non_idle.rows.insert("fleet".into(), json!({"pool": "fleet", "threads": threads}));
        let mut keyed = Fold::new(THREADS);
        for (id, status) in rolled {
            let display = if *status == "error" { "error: parked" } else { status };
            keyed.rows.insert(
                (*id).into(),
                json!({"rolled": {"status": status, "display": display, "active_descendants": 0}}),
            );
        }
        // Every thread is named `n-<id>`, so a parent line says whose it is.
        let mut summary = Fold::new(SUMMARY);
        for id in ["q", "c", "g", "r", "p"] {
            summary.rows.insert(id.into(), json!({"title": {"title": format!("n-{id}")}}));
        }
        project(&accounts, &non_idle, &keyed, &summary, Utc::now())
    }

    fn text(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn homed<'a>(b: &'a Board, id: &str) -> &'a Homed {
        b.cards[0].homed.iter().find(|h| h.id == id).expect("homed")
    }

    /// The homed line of `id` and every grouped line of the card, as text.
    fn drawn(b: &Board, id: &str) -> (String, Vec<String>) {
        let one = text(&homed_line(homed(b, id), "", true));
        let all = homed_lines(&b.cards[0].homed, Utc::now()).iter().map(text).collect();
        (one, all)
    }

    /// The roster `q` (queued) ← `c` (idle, off the roster) ← `g` (running),
    /// plus a lone queued root `r` and a parked child `p` of `r`.
    const ROSTER: &[(&str, &str, bool, &[&str])] = &[
        ("q", "queued", false, &[]),
        ("g", "active", true, &["c", "q"]),
        ("r", "queued", false, &[]),
        ("p", "parked", false, &["r"]),
    ];

    /// **TUI-P1d (TC-12): A POOL CARD'S STATUS IS THE DAEMON'S `rolled`, EVEN
    /// WHEN THE ROSTER WOULD ROLL DIFFERENTLY.** Every answer here is the
    /// opposite of what a face-side rollup over this same roster gives:
    /// * `q` has a running grandchild on the roster, and the daemon says `idle`
    ///   — it is drawn `queued` (its own word), not `active`;
    /// * `g`'s parent `c` (not on the roster) has a running child, and the
    ///   daemon says `idle` — its line is `waiting`, not `active`;
    /// * `r` has nothing running below it on the roster, and the daemon says
    ///   `active` (a descendant this board's roster does not show it) — it is
    ///   drawn `▶ … active`;
    /// * `r`'s child `p` is parked and errors do not roll up — `r` is never
    ///   drawn parked.
    ///
    /// FORCED RED by restoring the face rollup (`rollup_in` over the roster in
    /// `model::project`'s `roll`). Opposed by the agreeing case below, which a
    /// constant "own word" fails.
    #[test]
    fn tui_p1d_a_pool_card_reads_rolled_when_the_roster_would_roll_differently() {
        let b = board(ROSTER, &[("q", "idle"), ("c", "idle"), ("r", "active"), ("g", "active"), ("p", "error")]);
        assert_eq!(homed(&b, "q").rollup, Rollup::Own, "q: the roster outvoted the daemon's idle");
        assert_eq!(homed(&b, "r").rollup, Rollup::SubtreeActive, "r: the daemon's active was not read");
        let (q, all) = drawn(&b, "q");
        assert_eq!(q, "· n-q queued", "q drawn {q:?}");
        let (r, _) = drawn(&b, "r");
        assert_eq!(r, "▶ n-r active", "r drawn {r:?}");
        assert!(all.iter().any(|l| l.ends_with("⋯ n-c — waiting on 1")), "c's line: {all:?}");
        assert!(all.iter().any(|l| l.ends_with("▶ n-r — active · 1 here")), "r's line: {all:?}");
        assert!(!all.iter().any(|l| l.contains("parked ·") && l.contains("n-r")), "r rolled parked: {all:?}");
        // No `rolled` read yet: its own word, never a recount of the roster.
        let b = board(ROSTER, &[]);
        assert_eq!(homed(&b, "q").rollup, Rollup::Own, "no rolled held, and q was rolled up anyway");
    }

    /// **THE AGREEING CASE** — the daemon says what the roster suggests: `q`
    /// and `c` are `active`, and both are drawn active. A face that ignored
    /// `rolled` and drew every thread's own word fails here.
    #[test]
    fn tui_p1d_a_pool_card_reads_rolled_when_the_roster_agrees() {
        let b = board(ROSTER, &[("q", "active"), ("c", "active"), ("r", "idle"), ("g", "active")]);
        assert_eq!(homed(&b, "q").rollup, Rollup::SubtreeActive);
        assert_eq!(homed(&b, "g").rollup, Rollup::SelfActive);
        let (q, all) = drawn(&b, "q");
        assert_eq!(q, "▶ n-q active", "q drawn {q:?}");
        let (r, _) = drawn(&b, "r");
        assert_eq!(r, "· n-r queued", "r drawn {r:?}");
        assert!(all.iter().any(|l| l.ends_with("▶ n-c — active · 1 here")), "c's line: {all:?}");
        assert!(all.iter().any(|l| l.ends_with("⋯ n-r — waiting on 1")), "r's line: {all:?}");
    }
}
