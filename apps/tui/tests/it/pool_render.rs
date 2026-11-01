//! R7.S2 (DEC-37): the pool board's frame, from the daemon's own shape
//! frames, at a fixed clock. The golden is the whole screen; the state tests
//! pin the one thing a board must never get wrong — which state each card is
//! drawn in.

#[path = "pool_fixture/mod.rs"]

mod pool_fixture;

use pool_fixture::*;
use tui::pool::app::{App, LinkState};
use tui::pool::model::{Slot, Standing};
use tui::pool::render_to_text;

fn app() -> App {
    let mut a = App::new("ws://127.0.0.1:0/ws", now());
    for f in all_frames() {
        assert!(a.on_frame(&f), "frame not folded: {f}");
    }
    a.link = LinkState::Live;
    a
}

fn screen(a: &App, w: u16, h: u16) -> Vec<String> {
    render_to_text(a, w, h).unwrap()
}

fn line_of<'a>(s: &'a [String], label: &str) -> &'a str {
    s.iter()
        .find(|l| {
            // A card's title line: `[›] <n> <label> …`.
            let w: Vec<&str> = l.split_whitespace().skip_while(|t| *t == "›").take(2).collect();
            w.len() == 2 && w[0].parse::<usize>().is_ok() && w[1] == label
        })
        .unwrap_or_else(|| panic!("no card line for {label}:\n{}", s.join("\n")))
}

#[test]
fn the_pool_board_at_120x30() {
    let s = screen(&app(), 120, 30);
    let expected: &[&str] = &[
        " pool  ▶ 1 running (1 subagent) · ● 2 msgs queued · ◆ 1 parked (0 subagent, 1 no parent)   threads ● live · ws://127.0.0",
        " machine login is alpha",
        " ✕ beta login was WIPED — sign it in again",
        " ⚠ delta has no login yet",
        "",
        "› 1 alpha          login ok · LIVE LOGIN · ▶ 1 running · 50% share · pressure 1.40× · login 20d",
        "      session ▕██████████▎░░░░░░░░░░░░░░▏   41%  resets in 3h 12m",
        "      weekly  ▕██████████████████░░░░░░░▏   72%  resets in 3d 08h",
        "      fable   —",
        "      homed   ⋯ Mishmash: v5 region carve — waiting on 1",
        "                ▶ carve region R7 active +2q",
        "  2 beta           WIPED · ▶ 0 running · 50% share · pressure 1.40× · login 20d",
        "      session ▕███████████████████████▊░▏   95%  resets in 3h 12m",
        "      weekly  ▕██████████████████░░░░░░░▏   72%  resets in 3d 08h",
        "      fable   —",
        "      homed   ◆ parked · old test thread · no budget config: owner-gmail · since Sep 25 · no parent",
        "  3 delta          NO LOGIN · ▶ 0 running · 50% share · pressure 1.40× · login 20d",
        "      session —",
        "      weekly  —",
        "      fable   —",
        "  4 gamma          DISABLED — out of the pool, 3h 00m ago · vacation   press d to reactivate",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        " j/k move · 1-9 pick · d disable/reactivate · s set machine login · S share · q quit",
    ];
    if s != expected {
        panic!("golden mismatch; actual:\n{}", s.iter().map(|l| format!("        {l:?},")).collect::<Vec<_>>().join("\n"));
    }
}

/// Wiped is WIPED, live is live, disabled is DISABLED, no-login is NO LOGIN —
/// read off the drawn card lines, not the model.
#[test]
fn each_card_is_drawn_in_its_own_state() {
    let s = screen(&app(), 120, 30);
    let alpha = line_of(&s, "alpha");
    let beta = line_of(&s, "beta");
    let gamma = line_of(&s, "gamma");
    let delta = line_of(&s, "delta");
    assert!(alpha.contains("login ok") && alpha.contains("LIVE LOGIN"), "{alpha}");
    assert!(beta.contains("WIPED"), "wiped must be drawn WIPED: {beta}");
    assert!(!beta.contains("login ok") && !beta.contains("LIVE LOGIN"), "wiped drawn as live: {beta}");
    assert!(gamma.contains("DISABLED") && gamma.contains("vacation"), "{gamma}");
    assert!(!gamma.contains("login ok"), "a disabled card is one line, no login badge: {gamma}");
    assert!(delta.contains("NO LOGIN"), "{delta}");
    assert!(!alpha.contains("WIPED") && !alpha.contains("DISABLED"), "{alpha}");
    // The board warning for the wiped, ENABLED account; none for the disabled one.
    assert!(s.iter().any(|l| l.contains("✕ beta login was WIPED")), "{}", s.join("\n"));
}

#[test]
fn the_projection_types_each_state() {
    let b = app().board();
    let by = |l: &str| b.cards.iter().find(|c| c.label == l).unwrap().clone();
    assert_eq!(by("alpha").slot, Slot::Live);
    assert_eq!(by("beta").slot, Slot::Wiped);
    assert_eq!(by("delta").slot, Slot::NoLogin);
    assert!(matches!(by("gamma").standing, Standing::Disabled { .. }));
    assert_eq!(by("alpha").standing, Standing::Enabled);
    // homed threads come from threads/non-idle, by account
    assert_eq!(by("alpha").homed.len(), 1);
    assert_eq!(by("alpha").running(), 1);
    assert_eq!(by("beta").homed[0].state, "parked");
    assert_eq!(b.totals.parked, 1);
    assert_eq!(b.totals.queued_msgs, 2);
}

/// A live `row` event moves a card; a `gone` event removes it.
#[test]
fn live_frames_move_the_board() {
    let mut a = app();
    let r = row("beta", "live", false, false, Some(5.0));
    assert!(a.on_frame(&live_row("accounts", "beta", r)));
    let s = screen(&a, 120, 30);
    assert!(!line_of(&s, "beta").contains("WIPED"), "{}", s.join("\n"));
    let gone = serde_json::json!({"type": "shape", "shape": "accounts", "key": "delta", "op": "live",
        "epoch": {"state": "known"}, "event": {"kind": "gone"}});
    assert!(a.on_frame(&gone));
    assert!(a.board().cards.iter().all(|c| c.label != "delta"));
}

/// TUI-SUBAGENTS §3.6: a running subagent hangs under a line naming its
/// parent, a parked root says WHY, since when and that it has no parent, the
/// totals split subagents from roots — and no hash id is drawn anywhere, even
/// before the names arrive.
#[test]
fn subagents_hang_under_their_parent_and_a_parked_root_says_why() {
    let s = screen(&app(), 140, 30).join("\n");
    assert!(s.contains("▶ 1 running (1 subagent)"), "{s}");
    assert!(s.contains("◆ 1 parked (0 subagent, 1 no parent)"), "{s}");
    assert!(s.contains("⋯ Mishmash: v5 region carve — waiting on 1"), "{s}");
    assert!(s.contains("                ▶ carve region R7 active"), "the child is not indented under its parent:\n{s}");
    assert!(
        s.contains("◆ parked · old test thread · no budget config: owner-gmail · since Sep 25 · no parent"),
        "{s}"
    );

    // Only the roster: no names, no reasons yet. Still no hash on screen.
    let mut bare = App::new("ws://127.0.0.1:0/ws", now());
    for f in all_frames().into_iter().filter(|f| f["shape"] == "accounts" || f["shape"] == "threads/non-idle") {
        bare.on_frame(&f);
    }
    let s = screen(&bare, 140, 30).join("\n");
    assert!(!s.contains("th-aaaa") && !s.contains("th-bbbb") && !s.contains("th-parent"), "a hash id was drawn:\n{s}");
    assert!(s.contains("⋯ a thread — waiting on 1"), "{s}");
    assert!(s.contains("◆ parked · a thread · reason not read yet"), "{s}");
    // …and the board asks for exactly what it needs: names for all three,
    // and the `threads` row of all three — its `rolled` status for every
    // roster thread and every parent (TUI-P1d), the parked one's reason.
    let mut w = bare.take_wanted();
    w.sort();
    assert_eq!(
        w,
        vec![
            ("threads", "th-aaaaaaaa1".to_string()),
            ("threads", "th-bbbbbbbb2".to_string()),
            ("threads", "th-parent00".to_string()),
            ("threads/summary", "th-aaaaaaaa1".to_string()),
            ("threads/summary", "th-bbbbbbbb2".to_string()),
            ("threads/summary", "th-parent00".to_string()),
        ]
    );
    assert!(bare.take_wanted().is_empty(), "a key was asked for twice");
}

/// **∞ PRESSURE IS DRAWN `∞`** (TUI-P1b2; R9.PR1 inspect finding 2). An
/// account in the final 60 s before its weekly reset arrives as
/// `pressure: null, infinite: true, ranked: true` and placement ranks it
/// FIRST; the card used to draw no pressure at all for it. Opposed: a finite
/// row keeps its number and says no `∞`, and a genuinely unranked row
/// (`infinite: false` with a reason) says `unranked`, not `∞`.
#[test]
fn an_infinite_pressure_account_reads_infinity_and_an_unranked_one_does_not() {
    let mut a = app();
    let mut inf = row("beta", "live", false, false, Some(5.0));
    inf["pressure"] = serde_json::json!({"ranked": true, "pressure": null, "infinite": true,
        "unranked_reason": null, "share": 0.5, "disabled": false});
    assert!(a.on_frame(&live_row("accounts", "beta", inf)));
    let mut unr = row("delta", "live", false, false, Some(5.0));
    unr["pressure"] = serde_json::json!({"ranked": false, "pressure": null, "infinite": false,
        "unranked_reason": "no weekly reading", "share": 0.5, "disabled": false});
    assert!(a.on_frame(&live_row("accounts", "delta", unr)));
    let s = screen(&a, 120, 30);
    let (alpha, beta, delta) = (line_of(&s, "alpha"), line_of(&s, "beta"), line_of(&s, "delta"));
    assert!(beta.contains("pressure ∞"), "an ∞ account must read `pressure ∞`: {beta}");
    assert!(!beta.contains("unranked"), "∞ is ranked first, not unranked: {beta}");
    assert!(alpha.contains("pressure 1.40×") && !alpha.contains('∞'), "a finite one keeps its number: {alpha}");
    assert!(delta.contains("unranked (no weekly reading)") && !delta.contains('∞'), "{delta}");
}

/// The lines of `label`'s card: its title line to the line before the next
/// card's title (or the first blank line).
fn card_block(s: &[String], label: &str) -> Vec<String> {
    let title = line_of(s, label);
    let at = s.iter().position(|l| l == title).unwrap();
    let mut out = vec![s[at].clone()];
    for l in &s[at + 1..] {
        let w: Vec<&str> = l.split_whitespace().skip_while(|t| *t == "›").take(1).collect();
        if l.trim().is_empty() || w.first().is_some_and(|t| t.parse::<usize>().is_ok()) {
            break;
        }
        out.push(l.clone());
    }
    out
}

/// **V1-CLEAN2 (V1-CLEAN1-I B2): AT LIMIT is the daemon's `limit` cell, never
/// a full gauge** — as F5 made it on Status › Tokens. `full` reads 100 % on
/// its session meter with NO `limit` held: no line of its card says AT LIMIT.
/// `held` reads 10 % with `limit.until` set: its card says AT LIMIT, with the
/// daemon's countdown. A board deriving the state from `p >= 100` fails both.
#[test]
fn at_limit_is_the_daemons_limit_cell_never_a_full_gauge() {
    use serde_json::json;
    let mut a = App::new("ws://127.0.0.1:0/ws", now());
    let full = row("full", "live", false, false, Some(100.0));
    let mut held = row("held", "live", false, false, Some(10.0));
    held["limit"] = json!({"until": (now() + chrono::Duration::minutes(65)).to_rfc3339()});
    for f in [export("accounts", "full", full), export("accounts", "held", held), export("threads/non-idle", "fleet", json!({"pool": {}, "threads": []}))] {
        assert!(a.on_frame(&f), "frame not folded: {f}");
    }
    a.link = LinkState::Live;
    let s = screen(&a, 120, 30);
    let full = card_block(&s, "full");
    assert!(full.iter().any(|l| l.contains("session") && l.contains("100%")), "sentinel: full's meter reads 100 %:\n{}", full.join("\n"));
    assert!(!full.iter().any(|l| l.contains("AT LIMIT")), "a full meter drawn as a limit:\n{}", full.join("\n"));
    let held = card_block(&s, "held");
    assert!(held.iter().any(|l| l.contains("session") && l.contains("10%")), "sentinel: held's meter reads 10 %:\n{}", held.join("\n"));
    assert!(held.iter().any(|l| l.contains("AT LIMIT — back in 1h 05m")), "the daemon's limit not drawn:\n{}", held.join("\n"));
}
