//! V3 (msg 186; task 0183): metadata on EVERY band. His ask was the
//! cluster Status ripples-count style — `Ripples  ▶ 1 running · 13 ripples`
//! — on every band, every tab: a band header whose right-hand slot is empty
//! is the defect. This walks every band of every tab — every
//! cluster's four tabs (aggregate off and on, both Tasks sub-tabs), every
//! Status sub-tab with every band open, every page of the Bus modal reachable
//! from the Bus tab (events, triggers, ripples, executions; msg 235), every
//! Config tab with every band open, and the Threads tab (its list, every
//! thread, every subagent) — and fails on
//! a band line that carries nothing but its title. Hermetic: a scratch root,
//! CAULDRON_TYPE_ONE=off.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use tui::cauldron::app::{App, CTab, KTab};
use tui::cauldron::bus::BusPage;
use tui::cauldron::menu::CfgTab;
use tui::cauldron::status::{self, SAct, StatusTab};
use tui::cauldron::store::seed_dir;
use tui::cauldron::render_to_text;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "bandmeta183-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_in(root: &Path, name: &str) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") }
        .for_name(name)
        .unwrap()
}

fn open(env: &str) -> (App, PathBuf) {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let root = scratch_root();
    (App::open(env_in(&root, env), seed_dir(), true).expect("seed loads"), root)
}

fn screen(a: &mut App) -> String {
    render_to_text(a, 140, 400).unwrap().join("\n")
}

/// What is inside the frame's border, right of the sidebar when there is one.
fn page(row: &str) -> String {
    let inner = row.trim_start_matches('│').trim_end_matches('│');
    let cols: Vec<&str> = row.split('│').collect();
    if cols.len() > 3 { cols[2..cols.len() - 1].join("│") } else { inner.to_string() }
}

/// Every band line of a frame whose metadata slot is empty: a row that opens
/// with a band chevron and holds nothing past its title (a Config band's ⓘ
/// is the row's tip, not its metadata).
fn bare_bands(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for l in s.lines().skip(1) {
        let p = page(l);
        let t = p.trim();
        let Some(rest) = t.strip_prefix("▾ ").or_else(|| t.strip_prefix("▸ ")) else { continue };
        let rest = rest.trim_end_matches('ⓘ').trim();
        let chunks = rest.split("  ").filter(|c| !c.trim().is_empty()).count();
        if chunks <= 1 {
            out.push(rest.to_string());
        }
    }
    out
}

/// Open every band of a Config tab.
fn open_all(a: &mut App) {
    for _ in 0..12 {
        let closed: Vec<_> = a.entries().into_iter().filter(|f| f.open == Some(false)).filter_map(|f| f.entry.band()).collect();
        if closed.is_empty() {
            break;
        }
        for b in closed {
            a.snav.set_open(b, true);
        }
    }
}

/// Open every band of the Status tab on screen (a few rounds: an opened band
/// shows more).
fn open_status(a: &mut App) {
    for _ in 0..6 {
        let shut: Vec<String> = status::view(a, 140).items.into_iter().filter(|i| i.act == SAct::Band && !status::is_open(a, &i.key)).map(|i| i.key).collect();
        if shut.is_empty() {
            break;
        }
        for k in shut {
            if a.stnav.contains(&k) { a.stnav.remove(&k); } else { a.stnav.insert(k); }
        }
    }
}

/// Every page of the Bus modal reachable from the Bus tab's rows: `f` sees
/// each one's lines.
fn every_bus_page(a: &mut App, f: &mut impl FnMut(&str, &str)) {
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Bus);
    open_status(a);
    let mut todo: VecDeque<BusPage> = status::view(a, 140).items.into_iter().filter_map(|i| match i.act { SAct::Bus(p) => Some(p), _ => None }).collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while let Some(p) = todo.pop_front() {
        if !seen.insert(format!("{p:?}")) || seen.len() > 400 {
            continue;
        }
        a.open_bus(p.clone());
        let laid = a.bus_laid(92).unwrap();
        let text: String = std::iter::once("top".to_string()).chain(laid.lines.iter().map(|l| format!("│ {l} │"))).collect::<Vec<_>>().join("\n");
        f(&format!("Bus {p:?}"), &text);
        todo.extend(laid.links.into_iter().map(|(_, q)| q));
    }
    a.modal = None;
}

fn every_screen(a: &mut App, f: &mut impl FnMut(&str, &mut App)) {
    let keys: Vec<String> = a.forest.clusters.iter().map(|x| x.key.clone()).collect();
    for g in &keys {
        let at = a.forest.find(g).unwrap();
        a.select(at);
        for agg in [false, true] {
            a.aggregate = agg;
            for t in KTab::ALL {
                a.set_ctab(CTab::Cauldron);
                a.set_ktab(t);
                f(&format!("{g} {t:?} aggregate={agg}"), a);
            }
        }
        a.aggregate = false;
    }
    for st in StatusTab::ALL {
        a.set_ctab(CTab::Status);
        a.set_sttab(st);
        open_status(a);
        f(&format!("Status {st:?}"), a);
    }
    for t in CfgTab::ALL {
        a.set_ctab(CTab::Settings);
        a.set_cfg_tab(t);
        open_all(a);
        f(&format!("Config {t:?}"), a);
    }
    // The Threads tab (msg 235): its list, every thread, every subagent.
    a.set_ctab(CTab::Threads);
    crate::live_fixture::feed(a);
    f("Threads list", a);
    for i in crate::live_fixture::LISTED {
        crate::live_fixture::open(a, i);
        f(&format!("Threads #{i}"), a);
        let agents: Vec<String> = a.threads.agent_rows.iter().map(|x| x.id.clone()).collect();
        for id in agents {
            a.threads.set_subject(Some(id.clone()));
            tui::cauldron::threads::settle(a);
            crate::live_fixture::transcript(a);
            f(&format!("Threads #{i} agent {id}"), a);
        }
        a.threads.set_subject(None);
        tui::cauldron::threads::settle(a);
    }
    a.set_ctab(CTab::Cauldron);
}

/// V3: no band, on any tab or Bus page, has an empty metadata slot.
#[test]
fn v3_every_band_carries_metadata() {
    for env in ["prod", "design"] {
        let (mut a, _r) = open(env);
        let mut bands = 0;
        let mut bare: BTreeSet<String> = BTreeSet::new();
        every_screen(&mut a, &mut |what, a| {
            let s = screen(a);
            bands += s.lines().filter(|l| ["▾ ", "▸ ", "• "].iter().any(|c| page(l).trim_start().starts_with(c))).count();
            for b in bare_bands(&s) {
                bare.insert(format!("{env} · {what}: `{b}`"));
            }
        });
        let mut pages = 0;
        every_bus_page(&mut a, &mut |what, s| {
            pages += 1;
            for b in bare_bands(s) {
                bare.insert(format!("{env} · {what}: `{b}`"));
            }
        });
        // The design cauldron has two clusters and no System pages (msg 235).
        // An account is a table's row now, not a band (msg 235).
        // REL-6 took the Threads tab's Projects band and its per-project
        // bands away (and its rows are the store's, not each environment's
        // fake file), so the design floor is 100.
        let floor = if env == "prod" { 400 } else { 100 };
        assert!(bands > floor, "{env}: only {bands} band lines seen");
        assert!(env == "design" || pages > 20, "{env}: only {pages} bus pages seen");
        assert!(bare.is_empty(), "{env}: {} bands with no metadata:\n{}", bare.len(), bare.into_iter().collect::<Vec<_>>().join("\n"));
    }
}

/// The check itself: a band line with only its title is caught, with or
/// without its ⓘ; a band with metadata is not.
#[test]
fn the_check_catches_a_bare_band() {
    assert_eq!(bare_bands("top\n│ ▾ Roots                                   ⓘ │"), vec!["Roots"]);
    assert_eq!(bare_bands("top\n│ ▸ User input                              │"), vec!["User input"]);
    assert!(bare_bands("top\n│ ▾ Roots                  3 roots          ⓘ │").is_empty());
    assert!(bare_bands("top\n│ ▾ User input    ▶ 1 running · 13 ripples    │").is_empty());
}
