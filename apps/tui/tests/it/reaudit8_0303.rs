//! Task 0303 (8e4fca10 msg 235, the 0258 re-audit): J2 a double click never
//! takes out a reference he wrote — what its first press replaced is put back
//! at its anchor before the quote, and a quote's own reference stays. Real
//! seed files, real events, scratch roots.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, Hit, Item, KTab};
use tui::cauldron::render_to_text;
use tui::cauldron::store::seed_dir;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "t0303-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn open_at(root: &Path) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let env = Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") };
    App::open(env, seed_dir(), true).expect("opens")
}

fn typed(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
    }
}

fn press_hit(a: &mut App, hit: Hit) {
    let _ = render_to_text(a, 140, 50).unwrap();
    let r = a.hits.iter().rev().find(|(_, h)| *h == hit).map(|(r, _)| *r).unwrap_or_else(|| panic!("no {hit:?} on screen"));
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
    }
}

/// The second press of a double click on item `i`: its frame drawn first,
/// so a loaded machine's slow frame never turns it into two single clicks.
fn second_press(a: &mut App, i: usize) {
    let _ = render_to_text(a, 140, 50).unwrap();
    a.last_click = Some((std::time::Instant::now(), Hit::Item(i)));
    let r = a.hits.iter().rev().find(|(_, h)| *h == Hit::Item(i)).map(|(r, _)| *r).unwrap();
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: r.x, row: r.y, modifiers: KeyModifiers::NONE }));
    }
}

/// A double click: two presses with no slower click before them.
fn double(a: &mut App, i: usize) {
    a.last_click = None;
    press_hit(a, Hit::Item(i));
    second_press(a, i);
}

fn two_questions(a: &mut App) -> (usize, usize) {
    let x1 = a.forest.find("X-1").unwrap();
    a.select(x1);
    a.set_ktab(KTab::Questions);
    let qs: Vec<usize> = a.items().iter().enumerate().filter(|(_, i)| matches!(i, Item::Question { .. })).map(|(i, _)| i).collect();
    (qs[0], qs[1])
}

fn tag_of(a: &App, i: usize) -> String {
    a.tag_for(a.items()[i]).unwrap().render()
}

// ── J2 ─────────────────────────────────────────────────────────────────────

/// 0258's own case: `note [DIR-3]`, the caret at its end, a double click.
#[test]
fn j2_a_double_click_puts_back_the_reference_he_wrote() {
    let mut a = open_at(&scratch_root());
    let (q1, _) = two_questions(&mut a);
    let t1 = tag_of(&a, q1);
    a.type_text("note [DIR-3]");
    double(&mut a, q1);
    assert_eq!(a.composer.text(), format!("note [DIR-3]\n\n> {t1}"));
    // With a space after it, the same.
    let mut b = open_at(&scratch_root());
    let (q1, _) = two_questions(&mut b);
    b.type_text("note [DIR-3] ");
    double(&mut b, q1);
    assert_eq!(b.composer.text(), format!("note [DIR-3]\n\n> {t1}"));
}

/// The caret right after a quote's tag: the quote stays whole and the new one
/// goes under it — no orphan `>`.
#[test]
fn j2_a_double_click_after_a_quote_leaves_the_quote_whole() {
    let mut a = open_at(&scratch_root());
    let (q1, q2) = two_questions(&mut a);
    let (t1, t2) = (tag_of(&a, q1), tag_of(&a, q2));
    a.type_text(&format!("base\n\n> {t1}"));
    double(&mut a, q1);
    assert_eq!(a.composer.text(), format!("base\n\n> {t1}\n\n> {t1}"));
    // Quote after quote, by double clicks alone.
    double(&mut a, q2);
    double(&mut a, q1);
    let t = a.composer.text();
    assert_eq!(t, format!("base\n\n> {t1}\n\n> {t1}\n\n> {t2}\n\n> {t1}"));
    assert!(!t.lines().any(|l| l.trim() == ">"), "no orphan `>`: {t:?}");
}

/// msg 23 still holds: a click, then a double click on another, quotes the
/// second and takes the first click's reference out — only that one.
#[test]
fn j2_a_click_then_a_double_click_still_takes_the_clicks_own_reference() {
    let mut a = open_at(&scratch_root());
    let (q1, q2) = two_questions(&mut a);
    let t2 = tag_of(&a, q2);
    a.type_text("see [DIR-3]  ");
    press_hit(&mut a, Hit::Item(q1));
    double(&mut a, q2);
    assert_eq!(a.composer.text(), format!("see [DIR-3]\n\n> {t2}"));
    // A click that replaced a typed reference, then a double: the typed one
    // comes back, and the click's is gone.
    let mut b = open_at(&scratch_root());
    let (q1, q2) = two_questions(&mut b);
    b.type_text("x [DIR-3]");
    press_hit(&mut b, Hit::Item(q1));
    double(&mut b, q2);
    assert_eq!(b.composer.text(), format!("x [DIR-3]\n\n> {t2}"));
}

/// A second face types while this one double-clicks after a reference he
/// wrote: nothing of either goes, and the reference stays at its anchor.
#[test]
fn j2_with_a_second_face_typing_nothing_goes() {
    let root = scratch_root();
    let mut f = open_at(&root);
    let mut g = open_at(&root);
    let (q1, _) = two_questions(&mut f);
    let t1 = tag_of(&f, q1);
    f.type_text("lead\n\nnote [DIR-3]");
    g.sync();
    g.focus_input();
    g.composer = tui_render::compose::Composer::from_text(&g.composer.text());
    // g's caret at the end of `lead`, where it types.
    tui::cauldron::dump::set_caret(&mut g.composer, "lead".len());
    for (r, w) in ["one", "two", "three"].iter().enumerate() {
        f.last_click = None;
        match r {
            0 => {
                typed(&mut g, &format!(" {w}"));
                press_hit(&mut f, Hit::Item(q1));
                second_press(&mut f, q1);
            }
            1 => {
                press_hit(&mut f, Hit::Item(q1));
                typed(&mut g, &format!(" {w}"));
                second_press(&mut f, q1);
            }
            _ => {
                press_hit(&mut f, Hit::Item(q1));
                second_press(&mut f, q1);
                typed(&mut g, &format!(" {w}"));
            }
        }
        f.sync();
        g.sync();
        assert!(g.composer.text().contains(w), "g typed {w} into its bucket: {:?} ({:?})", g.composer.text(), g.focus);
    }
    for _ in 0..2 {
        f.sync();
        g.sync();
    }
    let (x, y) = (f.composer.text(), g.composer.text());
    assert_eq!(x, y, "the faces converge");
    assert!(x.starts_with("lead one two three\n\nnote [DIR-3]"), "{x:?}");
    assert_eq!(x.matches("[DIR-3]").count(), 1, "{x:?}");
    assert_eq!(x.matches(&format!("> {t1}")).count(), 3, "{x:?}");
    assert!(!x.lines().any(|l| l.trim() == ">"), "no orphan `>`: {x:?}");
}

// ── J1, J3, J6, J9: the Mesh's service controls ───────────────────────────

use ratatui::crossterm::event::KeyCode as K;
use tui::cauldron::app::{CTab, Focus, Modal};
use tui::cauldron::status::StatusTab;
use tui::cauldron::supervise;

fn mesh_with(a: &mut App, name: &str) {
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Mesh);
    a.focus = Focus::Content;
    a.stnav.insert(format!("svc:example-mac/{name}"));
}

fn row_of(a: &App, key: &str) -> usize {
    tui::cauldron::status::view(a, 120).items.iter().position(|i| i.key == key).unwrap_or_else(|| panic!("no row `{key}`"))
}

fn service<'a>(a: &'a App, name: &str) -> &'a tui::cauldron::dash::Service {
    a.dash.mesh.iter().flat_map(|n| n.services.iter()).find(|s| s.name == name).expect("the service")
}

fn enter(a: &mut App) {
    a.handle(Event::Key(KeyEvent::new(K::Enter, KeyModifiers::NONE)));
}

fn confirm_body(a: &App) -> Vec<String> {
    match &a.modal {
        Some(Modal::Confirm { body, .. }) => body.clone(),
        m => panic!("no confirm: {}", m.is_some()),
    }
}

/// Two rollbacks in a row: the first goes back to the older proven build,
/// the second says there is none — never forward to the newer one.
#[test]
fn j1_rollback_only_ever_goes_back() {
    let root = scratch_root();
    let mut a = open_at(&root);
    mesh_with(&mut a, "threads");
    assert_eq!(service(&a, "threads").version, "8299dcc1");
    a.rows[0] = row_of(&a, "ctl:example-mac/threads/rollback");
    enter(&mut a);
    assert!(confirm_body(&a)[0].contains("goes back from 8299dcc1 to 5c1e07aa"), "{:?}", confirm_body(&a));
    enter(&mut a);
    assert_eq!(service(&a, "threads").version, "5c1e07aa");
    // The second: nothing older is proven, so it is said and nothing is asked.
    let s = render_to_text(&mut a, 140, 200).unwrap().join("\n");
    assert!(s.contains("no version of threads before 5c1e07aa is recorded as proven good"), "{s}");
    assert!(!s.contains("to 8299dcc1, from 5c1e07aa"), "{s}");
    a.rows[0] = row_of(&a, "ctl:example-mac/threads/rollback");
    enter(&mut a);
    assert!(a.modal.is_none(), "nothing to confirm");
    assert_eq!(a.notice.as_deref(), Some("no version of threads before 5c1e07aa is recorded as proven good"));
    assert_eq!(service(&a, "threads").version, "5c1e07aa");
    assert_eq!(supervise::load(&tui::cauldron::store::soup_dir(&a.env).unwrap()).unwrap().len(), 1);
    // A face opened later agrees.
    let b = open_at(&root);
    assert_eq!(service(&b, "threads").version, "5c1e07aa");
    // A restart keeps the version: still nothing before it.
    mesh_with(&mut a, "threads");
    a.rows[0] = row_of(&a, "ctl:example-mac/threads/restart");
    enter(&mut a);
    enter(&mut a);
    assert_eq!(service(&a, "threads").version, "5c1e07aa");
    assert!(supervise::plan(&a.dash.mesh, &a.dash.threads, "example-mac", "threads", supervise::Op::Rollback).is_err());
}

/// Uptime after a restart counts from the record's `at`, and an unreadable
/// time reads `—`, never a frozen `0s`.
#[test]
fn j3_uptime_counts_from_the_record() {
    let root = scratch_root();
    let dir = {
        let a = open_at(&root);
        tui::cauldron::store::soup_dir(&a.env).unwrap()
    };
    // UPTIME-DST: the record a face writes (`stamp`), 3h 5m 7s before the
    // face's `now`; never a local wall-clock string subtracted from another.
    let at = supervise::stamp(supervise::now() - chrono::Duration::seconds(UP));
    let r = supervise::Record { at, who: "operator".into(), device: "example-mac".into(), service: "tts".into(), op: supervise::Op::Restart, why: "requested".into(), from: String::new(), to: String::new() };
    supervise::append(&dir, &r).unwrap();
    let mut a = open_at(&root);
    assert_eq!(supervise::uptime(service(&a, "tts")), "3h 5m");
    mesh_with(&mut a, "tts");
    let s = render_to_text(&mut a, 140, 200).unwrap().join("\n");
    assert!(s.lines().any(|l| l.contains("tts") && l.contains("running") && l.contains("3h 5m")), "{s}");
    assert!(s.lines().any(|l| l.contains("uptime") && l.contains("3h 5m")), "{s}");
    assert!(!s.lines().any(|l| l.contains("uptime") && l.contains(" 0s")), "{s}");
    // A record whose time cannot be read: `—`.
    let root = scratch_root();
    let dir = tui::cauldron::store::soup_dir(&open_at(&root).env).unwrap();
    supervise::append(&dir, &supervise::Record { at: "not a time".into(), ..r }).unwrap();
    assert_eq!(supervise::uptime(service(&open_at(&root), "tts")), "—");
    assert_eq!(supervise::span(12), "12s");
    assert_eq!(supervise::span(4 * 60 + 3), "4m");
    assert_eq!(supervise::span(6 * 86_400 + 2 * 3600 + 9), "6d 2h");
}

/// 3h 5m 7s: the uptime every UPTIME-DST case reads as `3h 5m`.
const UP: i64 = 3 * 3600 + 5 * 60 + 7;

/// **UPTIME-DST.** A service restarted 3h 5m before a moment that spans a DST
/// change reads `3h 5m`: its uptime is a difference of UTC instants, never of
/// local wall-clock readings. Each case runs the face in a CHILD of this test
/// binary with `TZ` and `CAULDRON_TEST_NOW` set (the JOBS-DST injection: an
/// isolated process reads its now from the variable), so the zone is real
/// and nothing in this process's environment is touched.
///
/// The instants (unix seconds) were computed once with Python `zoneinfo`:
///
/// | now (UTC)            | Los Angeles now | start, 3h 5m 7s before |
/// |----------------------|-----------------|------------------------|
/// | 1772968200 03-08 11:10 | 04:10 PDT     | 00:04:53 PST (spring-forward between) |
/// | 1793533620 11-01 11:47 | 03:47 PST     | 01:41:53 PDT, first 01:xx (fall-back between) |
/// | 1793537400 11-01 12:50 | 04:50 PST     | 01:44:53 PST, the REPEATED 01:xx |
///
/// The old naive-local subtraction read `4h 5m`, `2h 5m` and `3h 5m`. Every
/// case runs in America/Los_Angeles, in America/Phoenix (no DST) and in UTC,
/// with the record in the format a face now writes (RFC 3339) and, where it
/// is unambiguous, in the legacy local `YYYY-MM-DD HH:MM:SS` an older face
/// wrote. A legacy time in the repeated hour cannot name one instant; it is
/// read as the first occurrence (asserted below as documented, not as right).
#[test]
fn uptime_dst_reads_3h_5m_across_both_2026_transitions_in_every_zone() {
    const LA: &str = "America/Los_Angeles";
    const PHX: &str = "America/Phoenix";
    // (zone, now, legacy?, expected)
    let mut cases: Vec<(&str, i64, bool, &str)> = Vec::new();
    for now in [1_772_968_200, 1_793_533_620, 1_793_537_400] {
        for tz in [LA, PHX, "UTC"] {
            cases.push((tz, now, false, "3h 5m"));
            let repeated = tz == LA && now == 1_793_537_400;
            cases.push((tz, now, true, if repeated { "4h 5m" } else { "3h 5m" }));
        }
    }
    let exe = std::env::current_exe().unwrap();
    let mut failed = Vec::new();
    for (tz, now, legacy, want) in &cases {
        let out = std::process::Command::new(&exe)
            .args(["reaudit8_0303::uptime_dst_child", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
            .env("TZ", tz)
            .env("CAULDRON_TEST_NOW", now.to_string())
            .env("UPTIME_DST_LEGACY", if *legacy { "1" } else { "0" })
            .env("UPTIME_DST_WANT", want)
            .output()
            .unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        // Sentinel: the child ran the case, it did not filter to nothing.
        let ran = text.contains("UPTIME-DST-CHILD-RAN");
        if !out.status.success() || !ran {
            failed.push(format!("TZ={tz} now={now} legacy={legacy} want={want} ran={ran}:\n{text}"));
        }
    }
    assert!(failed.is_empty(), "{} of {} cases failed:\n{}", failed.len(), cases.len(), failed.join("\n---\n"));
}

/// The child of [`uptime_dst_reads_3h_5m_across_both_2026_transitions_in_every_zone`]:
/// one case, with `TZ` and `CAULDRON_TEST_NOW` set by the parent. Ignored,
/// and a no-op without the parent's variables.
#[test]
#[ignore = "run by uptime_dst_reads_3h_5m_across_both_2026_transitions_in_every_zone"]
fn uptime_dst_child() {
    let Ok(want) = std::env::var("UPTIME_DST_WANT") else { return };
    let legacy = std::env::var("UPTIME_DST_LEGACY").as_deref() == Ok("1");
    let now = supervise::now();
    // Sentinel: the pinned now was read, not the machine clock.
    assert_eq!(now.timestamp().to_string(), std::env::var("CAULDRON_TEST_NOW").unwrap());
    let start = now - chrono::Duration::seconds(UP);
    let at = if legacy { start.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string() } else { supervise::stamp(start) };
    assert!(supervise::instant(&at).is_some(), "{at}");
    let root = scratch_root();
    let dir = tui::cauldron::store::soup_dir(&open_at(&root).env).unwrap();
    let r = supervise::Record { at: at.clone(), who: "operator".into(), device: "example-mac".into(), service: "tts".into(), op: supervise::Op::Restart, why: "requested".into(), from: String::new(), to: String::new() };
    supervise::append(&dir, &r).unwrap();
    let mut a = open_at(&root);
    assert_eq!(supervise::uptime(service(&a, "tts")), want, "at={at}");
    assert_eq!(supervise::since_at(&at, now), want, "at={at}");
    mesh_with(&mut a, "tts");
    let s = render_to_text(&mut a, 140, 200).unwrap().join("\n");
    assert!(s.lines().any(|l| l.contains("uptime") && l.contains(&want)), "{s}");
    // The log shows the start as this zone's wall clock.
    let local = start.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string();
    if !legacy || want == "3h 5m" {
        assert!(s.lines().any(|l| l.contains(&local) && l.contains("restart")), "{local}\n{s}");
    }
    println!("UPTIME-DST-CHILD-RAN at={at} uptime={want}");
}

/// The Supervision band is shut while it is empty, and opens by itself once
/// it holds a record.
#[test]
fn j6_an_empty_supervision_log_is_shut() {
    let mut a = open_at(&scratch_root());
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Mesh);
    assert!(!tui::cauldron::status::is_open(&a, "supervision"));
    let s = render_to_text(&mut a, 140, 200).unwrap();
    let i = s.iter().position(|l| l.contains("Supervision")).expect("the band");
    assert!(s[i].contains("▸") && s[i].contains("0 records"), "{}", s[i]);
    mesh_with(&mut a, "tts");
    a.rows[0] = row_of(&a, "ctl:example-mac/tts/stop");
    enter(&mut a);
    enter(&mut a);
    assert!(tui::cauldron::status::is_open(&a, "supervision"));
}

/// Restart on a stopped service says it starts it, on the row and in the
/// confirm; the confirm carries no caption about the build's data (J5).
#[test]
fn j9_j5_restart_of_a_stopped_service_and_the_plain_confirm() {
    let mut a = open_at(&scratch_root());
    mesh_with(&mut a, "tts");
    a.rows[0] = row_of(&a, "ctl:example-mac/tts/restart");
    enter(&mut a);
    assert_eq!(confirm_body(&a), ["tts on example-mac is stopped and started again.", "It is recorded in the supervision log as requested by you."]);
    a.handle(Event::Key(KeyEvent::new(K::Esc, KeyModifiers::NONE)));
    a.rows[0] = row_of(&a, "ctl:example-mac/tts/stop");
    enter(&mut a);
    enter(&mut a);
    let s = render_to_text(&mut a, 140, 200).unwrap().join("\n");
    let line = s.lines().find(|l| l.contains("restart") && l.contains("start it again")).unwrap_or_else(|| panic!("{s}"));
    assert!(!line.contains("stop and start"), "{line}");
    a.rows[0] = row_of(&a, "ctl:example-mac/tts/restart");
    enter(&mut a);
    assert_eq!(confirm_body(&a)[0], "tts on example-mac is started again.");
    assert!(!s.contains("never on a process"), "{s}");
}

// ── J4, J8, H8 ─────────────────────────────────────────────────────────────

#[test]
fn j4_the_config_default_is_named_aggregated_clusters() {
    let d = tui::cauldron::settings::def("ui.aggregate_default");
    assert_eq!(d.label, "Aggregated clusters");
    let mut a = open_at(&scratch_root());
    a.set_ctab(CTab::Settings);
    a.set_cfg_tab(tui::cauldron::menu::CfgTab::Tui);
    let s = render_to_text(&mut a, 140, 50).unwrap().join("\n");
    assert!(s.contains("Aggregated clusters") && !s.contains("include everything below"), "{s}");
}

/// At 50 columns the cauldrons dropdown is whole, never cropped into the tabs.
/// V1.1: the environment bar (row 1) holds five tabs, so `prod` is whole from
/// 53 columns (the spec's floor is 60); narrower its NAME crops, never a tab
/// and never the `▾`.
#[test]
fn j8_the_top_bar_at_50_columns() {
    let mut a = open_at(&scratch_root());
    // V1-CLEAN1 (V1.1-I §3): the `|| w < 46` arm that excused 40 columns is
    // gone — below the spec's 60-column floor only 50 is held, and held whole.
    let top = render_to_text(&mut a, 50, 20).unwrap()[1].clone();
    assert!(top.contains('▾') && top.ends_with("Permissions|Settings"), "50: {top:?}");
    for w in [56u16, 60, 80, 140] {
        let top = render_to_text(&mut a, w, 20).unwrap()[1].clone();
        assert!(top.contains(" prod  ▾"), "{w}: {top:?}");
        assert!(top.contains("Settings"), "{w}: {top:?}");
        assert!(!top.contains("prCauldron") && !top.contains("prSurface"), "{w}: {top:?}");
        // 8e4fca10 msg 457: no product name; the environment leads.
        assert!(top.starts_with("  prod  ▾") || top.starts_with(" prod  ▾"), "{w}: {top:?}");
        if w >= 80 {
            assert!(top.starts_with("  prod  ▾"), "{w}: {top:?}");
        }
        // The dropdown's hit is where its name is drawn.
        let env = a.hits.iter().find(|(_, h)| *h == Hit::Env).map(|(r, _)| *r).unwrap();
        let x = top[..top.find(" prod").unwrap()].chars().count() as u16;
        assert_eq!(env.x, x, "{w}: {top:?}");
    }
}

#[test]
fn h8_con1_says_which_tabs_carry_a_count() {
    let mut a = open_at(&scratch_root());
    let s = render_to_text(&mut a, 140, 50).unwrap();
    assert!(s[3].contains("Questions 3 | Actions 1 | Tasks |"), "{}", s[3]);
    let seed = std::fs::read_to_string(seed_dir().join("clusters.json")).unwrap();
    assert!(seed.contains("No numbers on the cauldron's tabs; only Questions and Actions carry a count."));
    assert!(!seed.contains("only Tasks carries a count"));
}

// ── J7 ─────────────────────────────────────────────────────────────────────

/// A click on a reference in the bucket goes to it; the next page click puts
/// its reference BESIDE the one he went by, never in its place. Once he has
/// typed, the rule of msg 23 holds again.
#[test]
fn j7_the_reference_he_went_by_is_not_replaced() {
    let mut a = open_at(&scratch_root());
    let (q1, q2) = two_questions(&mut a);
    let (t1, t2) = (tag_of(&a, q1), tag_of(&a, q2));
    a.type_text("see ");
    press_hit(&mut a, Hit::Item(q1));
    assert_eq!(a.composer.text(), format!("see {t1} "));
    // A click on it, in the bucket.
    let s = render_to_text(&mut a, 140, 50).unwrap();
    let y = s.iter().rposition(|l| l.contains(&format!("see {t1}"))).unwrap();
    let x = s[y][..s[y].rfind(&t1).unwrap()].chars().count() as u16 + 1;
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: x, row: y as u16, modifiers: KeyModifiers::NONE }));
    }
    assert!(a.notice.as_deref().is_some_and(|n| n.starts_with(&t1)), "went to it: {:?}", a.notice);
    assert_eq!(a.composer.text(), format!("see {t1} "), "the bucket is untouched");
    // The caret is left after the chip (mail #68).
    assert_eq!(tui::cauldron::dump::caret_offset(&a.composer), format!("see {t1}").len());
    // The page it went to: the next click goes beside it.
    let (q1, q2) = two_questions(&mut a);
    a.last_click = None;
    press_hit(&mut a, Hit::Item(q2));
    assert_eq!(a.composer.text(), format!("see {t1}  {t2} "), "kept, not replaced");
    // A click right after that one replaces it, as msg 23 says.
    press_hit(&mut a, Hit::Item(q1));
    assert_eq!(a.composer.text(), format!("see {t1}  {t1} "));
}

// ── the adversarial pass ───────────────────────────────────────────────────

/// Seeded runs of everything a click can meet — words typed, references typed
/// by hand, single clicks, double clicks, clicks on a reference in the bucket,
/// a second face typing — and after every step: each word typed is there
/// exactly once, a double click never takes out a reference typed by hand,
/// and no quote line is left without its reference.
#[test]
fn adversarial_no_click_path_loses_or_doubles_what_he_typed() {
    for seed in 1..=6u64 {
        let root = scratch_root();
        let mut f = open_at(&root);
        let mut g = open_at(&root);
        let (q1, q2) = two_questions(&mut f);
        let mut rng = seed;
        let mut next = |n: u64| {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng >> 33) % n
        };
        let mut words: Vec<String> = Vec::new();
        for step in 0..60 {
            let q = if next(2) == 0 { q1 } else { q2 };
            let dirs_before = f.composer.text().matches("[DIR-").count();
            match next(7) {
                0 | 1 => {
                    let w = format!("w{seed}x{step}z");
                    f.focus_input();
                    let end = f.composer.text().len();
                    tui::cauldron::dump::set_caret(&mut f.composer, end);
                    typed(&mut f, &format!(" {w}"));
                    words.push(w);
                }
                2 => {
                    f.focus_input();
                    typed(&mut f, &format!(" [DIR-{}]", 1 + next(5)));
                }
                3 => {
                    f.last_click = None;
                    press_hit(&mut f, Hit::Item(q));
                }
                4 => {
                    double(&mut f, q);
                    assert!(f.composer.text().matches("[DIR-").count() >= dirs_before, "seed {seed} step {step}: a double click took a typed reference: {:?}", f.composer.text());
                }
                5 => {
                    let w = format!("g{seed}x{step}z");
                    g.sync();
                    g.focus_input();
                    tui::cauldron::dump::set_caret(&mut g.composer, 0);
                    typed(&mut g, &format!("{w} "));
                    words.push(w);
                }
                _ => {
                    // A click on a reference in the bucket, when one is drawn.
                    let s = render_to_text(&mut f, 140, 50).unwrap();
                    if let Some((y, x)) = s.iter().enumerate().rev().take(8).find_map(|(y, l)| l.find("[Q-").map(|b| (y, l[..b].chars().count() as u16 + 1))) {
                        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
                            f.handle(Event::Mouse(MouseEvent { kind, column: x, row: y as u16, modifiers: KeyModifiers::NONE }));
                        }
                        let _ = two_questions(&mut f);
                    }
                }
            }
            f.sync();
            g.sync();
            let t = f.composer.text();
            for w in &words {
                assert_eq!(t.matches(w.as_str()).count(), 1, "seed {seed} step {step}: {w} in {t:?}");
            }
            assert!(!t.lines().any(|l| l.trim() == ">"), "seed {seed} step {step}: an orphan `>`: {t:?}");
        }
        for _ in 0..2 {
            f.sync();
            g.sync();
        }
        assert_eq!(f.composer.text(), g.composer.text(), "seed {seed}: the faces converge");
    }
}

// ── H6 ─────────────────────────────────────────────────────────────────────

/// A click whose caret sits inside a word goes to the nearest word boundary,
/// never into the word.
#[test]
fn h6_a_click_inside_a_word_goes_to_its_nearest_boundary() {
    for (text, caret, want) in [
        ("hello world", "hello wor".len(), "hello world {t} "),
        ("hello world", "hello w".len(), "hello {t} world"),
        ("seven", "sev".len(), "seven {t} "),
        ("seven", "s".len(), "{t} seven"),
        ("a café b", "a ca".len(), "a café {t} b"),
    ] {
        let mut a = open_at(&scratch_root());
        let (q1, _) = two_questions(&mut a);
        let t = tag_of(&a, q1);
        a.type_text(text);
        tui::cauldron::dump::set_caret(&mut a.composer, caret);
        a.focus = Focus::Content;
        a.last_click = None;
        press_hit(&mut a, Hit::Item(q1));
        assert_eq!(a.composer.text(), want.replace("{t}", &t), "{text:?} at {caret}");
    }
}

/// 0258's own race: another face types a word where this face's caret sits;
/// this face's click lands beside the word, whole.
#[test]
fn h6_another_faces_word_is_never_split() {
    let root = scratch_root();
    let mut f = open_at(&root);
    let mut g = open_at(&root);
    let (q1, _) = two_questions(&mut f);
    let t1 = tag_of(&f, q1);
    f.type_text("start ");
    g.sync();
    g.focus_input();
    g.composer = tui_render::compose::Composer::from_text(&g.composer.text());
    let end = g.composer.text().len();
    tui::cauldron::dump::set_caret(&mut g.composer, end);
    for w in ["two", "seven", "eleven"] {
        typed(&mut g, &format!("{w} "));
        f.last_click = None;
        press_hit(&mut f, Hit::Item(q1));
        f.sync();
        g.sync();
        let t = f.composer.text();
        assert!(t.split_whitespace().any(|x| x == w), "{w} whole in {t:?}");
    }
    for _ in 0..2 {
        f.sync();
        g.sync();
    }
    let t = f.composer.text();
    assert_eq!(t, g.composer.text());
    for w in ["start", "two", "seven", "eleven"] {
        assert_eq!(t.split_whitespace().filter(|x| *x == w).count(), 1, "{w} in {t:?}");
    }
    assert!(t.contains(&t1));
}
