//! V1.9 (R-V1 item 9: "Rename 'splash' to type 3, and remove 'absent'"):
//! no screen says `splash` — a type 3 ripple is `type 3` on screen and
//! `type_three` on disk, ripple/v2's word — and the data the face ships and
//! its fixtures read in that word. The `absent` half (consumer §8, TC-48) is
//! proven where the status word is made, `store::tests::v19_g3_*`; the stored
//! word's serde half is `cauldron::ripples::tests::v19_g2_*`.
//!
//! Real frames from real seed files under a scratch cauldron root; the bans
//! are asserted on RENDERED text and PARSED data, never on source.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use serde_json::Value;
use tui::cauldron::app::{App, CTab, Focus};
use tui::cauldron::bus::BusPage;
use tui::cauldron::ripples::{Kind, SeedFile, DEFAULT_POLICY};
use tui::cauldron::status::StatusTab;
use tui::cauldron::store::seed_dir;
use tui::cauldron::render_to_text;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "v19-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn app() -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let root = scratch_root();
    let env = Environment { root: root.clone(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") };
    App::open(env, seed_dir(), true).expect("opens")
}

/// 120 columns, tall enough that no row a ban is about scrolls off.
fn frame(a: &mut App) -> String {
    render_to_text(a, 120, 300).unwrap().join("\n")
}

fn no_splash(what: &str, s: &str) {
    assert!(!s.to_lowercase().contains("splash"), "{what} says splash:\n{s}");
}

// ── G1: no "splash" on any screen ─────────────────────────────────────────

/// Every Status sub-tab, the Ledger with a type 3 expanded, the Bus's
/// ripple catalog, and the bus modal's pages for a type 3 — its Exec page
/// from the Ledger, its Ripple page, its runs and their type-3 steps. No
/// line says `splash`; the type-3 rows say `type 3` (the opposition: a type
/// 3 IS on each frame, so the ban is reachable).
#[test]
fn g1_no_screen_says_splash_and_a_type_3_says_type_3() {
    let mut a = app();
    a.set_ctab(CTab::Status);
    a.focus = Focus::Content;
    // The Bus's catalog band is shut by default; open it.
    a.stnav.insert("ripples".into());
    for t in StatusTab::ALL {
        a.set_sttab(t);
        let s = frame(&mut a);
        no_splash(t.label(), &s);
        if t == StatusTab::Bus {
            // The catalog row of `dump`, the seed's top-level type 3.
            let row = s.lines().find(|l| l.contains(" dump ") && l.contains("today")).unwrap_or_else(|| panic!("no dump row:\n{s}"));
            assert!(row.contains("type 3"), "{row}");
        }
        if t == StatusTab::Ledger {
            // Dump 11 is the newest and open; its first ripple, `dump`, is a
            // type 3 and shows its phases.
            assert!(s.lines().any(|l| l.contains("③ dump")), "no type 3 on the Ledger:\n{s}");
        }
    }

    // The bus modal's Exec page for dump 11's type 3.
    a.open_bus(BusPage::Exec { seq: 11, path: vec![0] });
    let s = frame(&mut a);
    no_splash("the Exec page", &s);
    assert!(s.contains("③ type 3") && s.contains("• Steps"), "{s}");
    a.modal = None;

    // Its Ripple page: the kind row.
    a.open_bus(BusPage::Ripple("dump".into()));
    let s = frame(&mut a);
    no_splash("the Ripple page", &s);
    let kind = s.lines().find(|l| l.contains("kind")).unwrap_or_else(|| panic!("no kind row:\n{s}"));
    assert!(kind.contains("type 3"), "{kind}");
    a.modal = None;

    // Every recorded run of a type 3, and every type-3 step inside one.
    let three: Vec<String> = a.rstore.live.file.ripple.iter().filter(|r| r.kind == Kind::TypeThree).map(|r| r.id.clone()).collect();
    let runs: Vec<(String, Vec<usize>)> = a
        .rstore
        .runs()
        .iter()
        .filter(|r| three.contains(&r.ripple))
        .flat_map(|r| {
            let mut v = vec![(r.id.clone(), Vec::new())];
            v.extend(r.tree.iter().enumerate().filter(|(_, s)| s.kind == "type_three").map(|(i, _)| (r.id.clone(), vec![i])));
            v
        })
        .collect();
    assert!(runs.iter().any(|(_, p)| p.is_empty()), "no recorded run of a type 3: {three:?}");
    let steps = runs.iter().filter(|(_, p)| !p.is_empty()).count();
    assert!(steps > 0, "no type-3 step in a type 3's run");
    for (id, path) in runs {
        let page = if path.is_empty() { BusPage::Run(id.clone()) } else { BusPage::Step(id.clone(), path.clone()) };
        a.open_bus(page);
        let s = frame(&mut a);
        no_splash(&format!("run {id} {path:?}"), &s);
        if path.is_empty() && s.contains("• Steps") {
            // A step that is a type 3 says so in its kind column.
            let r = a.rstore.run(&id).unwrap().clone();
            if let Some(st) = r.tree.iter().find(|s| s.kind == "type_three") {
                let row = s.lines().find(|l| l.contains(&st.step)).unwrap_or_else(|| panic!("no row {}:\n{s}", st.step));
                assert!(row.contains("type 3"), "{row}");
            }
        } else if !path.is_empty() {
            let kind = s.lines().find(|l| l.contains("kind")).unwrap_or_else(|| panic!("no kind row:\n{s}"));
            assert!(kind.contains("type 3"), "{kind}");
        }
        a.modal = None;
    }
}

// ── G4: the data loads, with no retired kind word left ────────────────────

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Every `kind` value in a JSON document, counted — parsed, not grepped.
fn kinds(v: &Value, out: &mut BTreeMap<String, usize>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(k)) = m.get("kind") {
                *out.entry(k.clone()).or_default() += 1;
            }
            m.values().for_each(|x| kinds(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| kinds(x, out)),
        _ => {}
    }
}

const RETIRED: [&str; 3] = ["splash", "shell", "aggregate"];

/// The data files a type 3 lives in: the policy compiled into the face, the
/// fixture policy, the fixture ledger, and REL-3's `cauldron-build` ledger.
fn data() -> Vec<(String, String, &'static str)> {
    let mut v = vec![("DEFAULT_POLICY".to_string(), DEFAULT_POLICY.to_string(), "policy")];
    for (rel, what) in [("tests/fixtures/seed/ripples.json", "policy"), ("tests/fixtures/seed/soup/prod.ledger.json", "ledger"), ("../../libs/soup/demo/cauldron-build/ledger.json", "ledger")] {
        let p = repo(rel);
        v.push((p.display().to_string(), std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())), what));
    }
    v
}

fn parse(text: &str, what: &str) -> Result<(), String> {
    match what {
        "policy" => serde_json::from_str::<SeedFile>(text).map(|_| ()).map_err(|e| e.to_string()),
        _ => serde_json::from_str::<tui::cauldron::ledger::Seed>(text).map(|_| ()).map_err(|e| e.to_string()),
    }
}

#[test]
fn g4_the_data_parses_and_no_kind_is_splash_shell_or_aggregate() {
    for (name, text, what) in data() {
        // Opposition: the unmodified file parses.
        parse(&text, what).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut n = BTreeMap::new();
        kinds(&serde_json::from_str(&text).unwrap(), &mut n);
        assert!(n.get("type_three").copied().unwrap_or(0) > 0, "{name} holds no type 3, so the ban below is not reachable: {n:?}");
        for r in RETIRED {
            assert_eq!(n.get(r), None, "{name} still says kind `{r}`: {n:?}");
        }
    }
}

/// G4's red, kept: one `splash` kind planted in a COPY of each file fails
/// the parse, and the failure names the word.
#[test]
fn g4_a_planted_splash_kind_fails_the_parse_naming_it() {
    for (name, text, what) in data() {
        let planted = text.replacen("\"type_three\"", "\"splash\"", 1);
        assert_ne!(planted, text, "{name}: nothing to plant over");
        let e = parse(&planted, what).expect_err(&name);
        assert!(e.contains("splash"), "{name}: the refusal does not name the word: {e}");
    }
}
