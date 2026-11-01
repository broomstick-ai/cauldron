//! 8e4fca10 msg 403 (task 0263): the Mesh's service controls — restart,
//! stop and rollback on every service, each asked first and recorded in the
//! supervision log the Mesh shows (who, what, why=requested), acting on the
//! face's data and never on a process. Hermetic: a scratch root,
//! CAULDRON_TYPE_ONE=off.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tui::cauldron::app::{App, CTab, Focus, KTab, Modal};
use tui::cauldron::render_to_text;
use tui::cauldron::status::StatusTab;
use tui::cauldron::store::seed_dir;
use tui::cauldron::supervise::{self, Op};

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "msg403-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_in(root: &Path) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") }
}

fn open_at(root: &Path, persist: bool) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(root), seed_dir(), persist).expect("seed loads")
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn click(a: &mut App, x: u16, y: u16) {
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        a.handle(Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }));
    }
}

/// The Mesh, with service `name`'s band open.
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

/// Enter on `restart`, `stop` or `rollback` asks first; Esc asks nothing more
/// and nothing changes; Enter on the confirm does it, records it, and the
/// service shows it.
#[test]
fn each_control_asks_first_then_acts_and_is_recorded() {
    let root = scratch_root();
    let mut a = open_at(&root, true);
    mesh_with(&mut a, "threads");
    assert_eq!(service(&a, "threads").status, "running");

    // Esc: asked, refused, nothing recorded.
    a.rows[0] = row_of(&a, "ctl:example-mac/threads/stop");
    a.handle(key(KeyCode::Enter));
    assert!(matches!(a.modal, Some(Modal::Confirm { ref title, .. }) if title == "Stop threads?"), "{:?}", a.modal.is_some());
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none());
    assert_eq!(service(&a, "threads").status, "running");
    assert!(supervise::load(&tui::cauldron::store::soup_dir(&a.env).unwrap()).unwrap().is_empty());

    // Stop.
    a.rows[0] = row_of(&a, "ctl:example-mac/threads/stop");
    a.handle(key(KeyCode::Enter));
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none(), "the confirm closes");
    assert_eq!(service(&a, "threads").status, "stopped");
    assert_eq!(service(&a, "threads").pid, None);

    // Stopped, `stop` says so and asks nothing.
    a.rows[0] = row_of(&a, "ctl:example-mac/threads/stop");
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none());
    assert_eq!(a.notice.as_deref(), Some("threads is already stopped"));

    // Restart.
    a.rows[0] = row_of(&a, "ctl:example-mac/threads/restart");
    a.handle(key(KeyCode::Enter));
    a.handle(key(KeyCode::Enter));
    assert_eq!(service(&a, "threads").status, "running");
    // Up since the record, counted when it is drawn (0258 J3).
    assert!(service(&a, "threads").started.is_some());
    assert!(supervise::uptime(service(&a, "threads")).ends_with('s'), "{}", supervise::uptime(service(&a, "threads")));
    assert!(service(&a, "threads").pid.is_some());

    // Rollback: to the newest proven version before the one it runs.
    a.rows[0] = row_of(&a, "ctl:example-mac/threads/rollback");
    a.handle(key(KeyCode::Enter));
    a.handle(key(KeyCode::Enter));
    assert_eq!(service(&a, "threads").version, "5c1e07aa");

    let recs = supervise::load(&tui::cauldron::store::soup_dir(&a.env).unwrap()).unwrap();
    assert_eq!(recs.iter().map(|r| r.op).collect::<Vec<_>>(), [Op::Stop, Op::Restart, Op::Rollback]);
    for r in &recs {
        assert_eq!((r.who.as_str(), r.why.as_str(), r.device.as_str(), r.service.as_str()), ("operator", "requested", "example-mac", "threads"));
    }
    assert_eq!((recs[2].from.as_str(), recs[2].to.as_str()), ("8299dcc1", "5c1e07aa"));

    // The log the Mesh shows: newest first, who and why on each.
    let s = render_to_text(&mut a, 140, 200).unwrap().join("\n");
    let log: Vec<&str> = s.lines().skip_while(|l| !l.contains("Supervision")).collect();
    assert!(log[0].contains("3 records"), "{s}");
    assert!(log[1].contains("rollback") && log[1].contains("threads on example-mac") && log[1].contains("8299dcc1 → 5c1e07aa · operator · requested"), "{s}");
    assert!(log[2].contains("restart") && log[3].contains("stop"), "{s}");

    // A new face over the same soup reads the log and shows what was done.
    let b = open_at(&root, true);
    assert_eq!(service(&b, "threads").version, "5c1e07aa");
    assert_eq!(b.dash.supervisions.len(), 3);
}

/// A click on a control asks first, as Enter does; the confirm's button does it.
#[test]
fn a_click_on_a_control_asks_first() {
    let root = scratch_root();
    let mut a = open_at(&root, true);
    mesh_with(&mut a, "tts");
    let s = render_to_text(&mut a, 140, 200).unwrap();
    let (y, l) = s.iter().enumerate().skip_while(|(_, l)| !l.contains("tts ")).find(|(_, l)| l.trim_start().contains("│      restart") || l.contains("      restart ")).expect("the restart row");
    let x = l.find("restart").map(|b| l[..b].chars().count()).unwrap() as u16;
    click(&mut a, x, y as u16);
    assert!(matches!(a.modal, Some(Modal::Confirm { ref title, .. }) if title == "Restart tts?"));
    let s = render_to_text(&mut a, 140, 200).unwrap();
    let flat = s.iter().map(|l| l.trim_matches(|c: char| c == '│' || c == ' ')).collect::<Vec<_>>().join(" ");
    assert!(flat.contains("Restart tts?") && flat.contains("supervision log"), "{}", s.join("\n"));
    a.handle(key(KeyCode::Enter));
    assert_eq!(a.dash.supervisions.len(), 1);
    assert_eq!(a.dash.supervisions[0].op, Op::Restart);
}

/// What cannot be done is said on the row and asks nothing: a rollback with
/// no earlier proven version; the threads daemon has controls too.
#[test]
fn what_cannot_be_done_is_said_and_the_daemon_has_controls() {
    let root = scratch_root();
    let mut a = open_at(&root, true);
    mesh_with(&mut a, "tts");
    let s = render_to_text(&mut a, 140, 200).unwrap().join("\n");
    assert!(s.contains("no version of tts before a35dad41 is recorded as proven good"), "{s}");
    a.rows[0] = row_of(&a, "ctl:example-mac/tts/rollback");
    a.handle(key(KeyCode::Enter));
    assert!(a.modal.is_none());
    assert!(a.dash.supervisions.is_empty());
    // The daemon: stop, then restart.
    a.stnav.insert("daemon".into());
    let live = a.dash.threads.link.clone();
    a.rows[0] = row_of(&a, "ctl:example-mac/threads daemon/stop");
    a.handle(key(KeyCode::Enter));
    if live == "live" {
        a.handle(key(KeyCode::Enter));
        assert_eq!(a.dash.threads.link, "offline");
    }
    a.rows[0] = row_of(&a, "ctl:example-mac/threads daemon/restart");
    a.handle(key(KeyCode::Enter));
    a.handle(key(KeyCode::Enter));
    assert_eq!(a.dash.threads.link, "live");
    assert_eq!(a.dash.supervisions.last().unwrap().service, supervise::DAEMON);
}

/// --dump changes nothing: the confirm says so and stays open.
#[test]
fn a_dump_records_nothing() {
    let root = scratch_root();
    let mut a = open_at(&root, false);
    mesh_with(&mut a, "threads");
    a.rows[0] = row_of(&a, "ctl:example-mac/threads/restart");
    a.handle(key(KeyCode::Enter));
    a.handle(key(KeyCode::Enter));
    assert!(matches!(a.modal, Some(Modal::Confirm { error: Some(ref e), .. }) if e.contains("--dump")));
    assert!(!supervise::path(&tui::cauldron::store::soup_dir(&a.env).unwrap()).exists());
}

/// A fresh face over a scratch root.
pub(crate) fn fresh() -> App {
    open_at(&scratch_root(), true)
}

// ── item 3: noun counts on every band header ──────────────────────────────

/// The metadata of every band header in `s` (what stands right of the name), by
/// where it was first seen.
fn band_metas(s: &str, out: &mut std::collections::BTreeMap<String, String>, what: &str) {
    for row in s.lines() {
      for k in [1usize, 2] {
        let page = row.split('│').nth(k).unwrap_or("");
        let t = page.trim_start_matches(['▌', ' ', '╭', '─']);
        if t.starts_with('▾') || t.starts_with('▸') || t.starts_with('•') {
            // the meta: after the last run of 2+ spaces
            let tt = t.trim_end();
            if let Some(i) = tt.rfind("  ") {
                let m = tt[i..].trim().trim_end_matches(['─', '╮', ' ']).to_string();
                out.entry(m).or_insert_with(|| format!("{what}: {}", tt.chars().take(60).collect::<String>()));
            }
        }
      }
    }
}

/// msg 403, Q10 (a): every band header's metadata counts with a NOUN —
/// `35 artifacts`, `16 ripples`, `2 directives` — on every tab, every
/// cluster, aggregated or not, every band shut and open: no segment is a bare
/// number or a glyph and a bare number.
#[test]
fn every_band_header_counts_with_a_noun() {
    let mut a = fresh();
    let mut out = std::collections::BTreeMap::new();
    for agg in [false, true] {
        a.set_aggregate(agg);
        for g in 0..a.forest.clusters.len() {
            for t in KTab::ALL {
                for open in [false, true] {
                    a.select(g);
                    a.set_ktab(t);
                    crate::cauldron_render::all_bands_open(&mut a, open);
                    let s = render_to_text(&mut a, 200, 300).unwrap().join("\n");
                    band_metas(&s, &mut out, &format!("{} {t:?}", a.forest.get(g).key));
                }
            }
        }
    }
    a.set_ctab(CTab::Status);
    for st in StatusTab::ALL {
        a.set_sttab(st);
        for _ in 0..4 {
            let v = tui::cauldron::status::view(&a, 200);
            for it in &v.items {
                if it.act == tui::cauldron::status::SAct::Band && !tui::cauldron::status::is_open(&a, &it.key) {
                    if !a.stnav.remove(&it.key) { a.stnav.insert(it.key.clone()); }
                }
            }
        }
        let s = render_to_text(&mut a, 150, 430).unwrap().join("\n");
        band_metas(&s, &mut out, &format!("Status {st:?}"));
    }
    for t in tui::cauldron::menu::CfgTab::ALL {
        a.set_ctab(CTab::Settings);
        a.set_cfg_tab(t);
        let s = render_to_text(&mut a, 150, 430).unwrap().join("\n");
        band_metas(&s, &mut out, &format!("Config {t:?}"));
    }
    a.set_ctab(CTab::Threads);
    crate::live_fixture::feed(&mut a);
    let s = render_to_text(&mut a, 200, 300).unwrap().join("\n");
    band_metas(&s, &mut out, "Threads");
    assert!(out.len() > 150, "the sweep saw {} band headers", out.len());
    let bare: Vec<String> = out
        .iter()
        .filter(|(m, _)| m.split('·').map(str::trim).filter(|seg| !seg.starts_with('#')).any(|seg| {
            let t = seg.trim_start_matches(|c: char| !c.is_alphanumeric()).trim();
            !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) || seg.split_whitespace().collect::<Vec<_>>().windows(3).any(|w| w[1].chars().all(|c| c.is_ascii_digit()) && !w[0].chars().any(char::is_alphanumeric) && !w[2].chars().any(char::is_alphanumeric))
        }))
        .map(|(m, w)| format!("`{m}` on {w}"))
        .collect();
    assert!(bare.is_empty(), "bare counts:\n{}", bare.join("\n"));
}

// ── item 4: a region band shows its handle in normal case ─────────────────

/// msg 403, Q11 (a) "handle it is": every region band — on a cluster's
/// Nodes, under `Aggregated clusters`, on Status › Soup's unclaimed regions
/// — shows the handle in normal case, never its capitalised lead; a band
/// that is wide enough shows it whole.
#[test]
fn region_bands_show_the_handle_in_normal_case() {
    let mut a = fresh();
    let mut seen = 0;
    for agg in [false, true] {
        a.set_aggregate(agg);
        for g in 0..a.forest.clusters.len() {
            a.select(g);
            a.set_ktab(KTab::Nodes);
            let s = render_to_text(&mut a, 220, 200).unwrap();
            for l in s.iter().filter(|l| l.contains("▸ R-") || l.contains("▾ R-") || l.contains("▸ SUB-") || l.contains("▾ SUB-")) {
                seen += 1;
                for loud in ["PLATFORM DEPLOY", "RIPPLES -", "THE CLUSTER TREE", "MESH DEVICES", "SUBJECTS", "REGIONS PROTOCOL", "DREAMING", "LANDING -", "CURATION CRAFT", "ENVIRONMENTS"] {
                    assert!(!l.contains(loud), "{loud} on {l}");
                }
            }
        }
    }
    assert!(seen > 10, "{seen} region bands");
    a.set_aggregate(false);
    a.select(a.forest.find("X-9").unwrap());
    a.set_ktab(KTab::Nodes);
    let s = render_to_text(&mut a, 220, 200).unwrap().join("\n");
    assert!(s.contains("R-21   Platform deploy - the signed service"), "{s}");
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Soup);
    let s = render_to_text(&mut a, 160, 120).unwrap().join("\n");
    assert!(s.contains("R-9     Mesh devices - machines") && !s.contains("MESH DEVICES"), "{s}");
}

// ── the adversarial pass ───────────────────────────────────────────────────

/// Nothing he typed is lost: the bucket's text stands through a service act
/// asked and confirmed; a paste while the confirm is open is not dropped
/// without a word; a second face over the same soup shows the act at its
/// next sync, and both faces' words stand in the one bucket.
#[test]
fn a_service_act_loses_nothing_he_typed_and_reaches_the_other_face() {
    let root = scratch_root();
    let mut a = open_at(&root, true);
    let mut b = open_at(&root, true);
    a.type_text("half a thought ");
    b.type_text("the other face's words");
    mesh_with(&mut a, "cauldron daemon");
    a.rows[0] = row_of(&a, "ctl:example-mac/cauldron daemon/stop");
    a.handle(key(KeyCode::Enter));
    assert!(matches!(a.modal, Some(Modal::Confirm { .. })));
    a.handle(Event::Paste("dictated while it asked".into()));
    assert!(a.notice.as_deref().is_some_and(|n| n.contains("not pasted")), "{:?}", a.notice);
    assert!(matches!(a.modal, Some(Modal::Confirm { .. })), "the paste does not answer the confirm");
    a.handle(key(KeyCode::Enter));
    assert_eq!(service(&a, "cauldron daemon").status, "stopped");
    assert_eq!(a.composer.text(), "half a thought ");
    // The other face.
    assert_eq!(service(&b, "cauldron daemon").status, "running");
    b.sync();
    assert_eq!(service(&b, "cauldron daemon").status, "stopped", "the other face reads the log");
    // The cauldron has ONE bucket, merged between faces: both faces' words
    // stand in it, whole.
    let t = b.composer.text();
    assert!(t.contains("the other face's words") && t.contains("half a thought"), "{t}");
    let c = open_at(&root, true);
    let t = c.composer.text();
    assert!(t.contains("the other face's words") && t.contains("half a thought"), "a new face reads both: {t}");
}

/// Every control of every service on the Mesh asks first: Enter on each
/// opens the confirm or says why not, and Esc leaves the data as it was.
#[test]
fn every_control_of_every_service_asks_first() {
    let root = scratch_root();
    let mut a = open_at(&root, true);
    mesh_with(&mut a, "threads");
    for s in a.dash.mesh[0].services.iter().map(|s| s.name.clone()).collect::<Vec<_>>() {
        a.stnav.insert(format!("svc:example-mac/{s}"));
    }
    a.stnav.insert("daemon".into());
    let keys: Vec<String> = tui::cauldron::status::view(&a, 140).items.iter().filter(|i| i.key.starts_with("ctl:")).map(|i| i.key.clone()).collect();
    assert_eq!(keys.len(), 3 * (a.dash.mesh[0].services.len() + 1), "{keys:?}");
    for k in keys {
        a.rows[0] = row_of(&a, &k);
        a.handle(key(KeyCode::Enter));
        let asked = matches!(a.modal, Some(Modal::Confirm { .. }));
        assert!(asked || a.notice.is_some(), "{k}: neither asked nor said why not");
        if asked {
            a.handle(key(KeyCode::Esc));
        }
        assert!(a.modal.is_none());
    }
    assert!(a.dash.supervisions.is_empty(), "nothing was confirmed");
}
