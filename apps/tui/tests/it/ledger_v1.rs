//! V1.4.S1 — Ledger v1, the first cut (R-V1 item 4): *"a time-linear list
//! of dumps, styled as an event flow like a Claude Code thread. It is NOT
//! the expandable bands used elsewhere. Each dump expands into its phases
//! and its ripple tree. Lines show which ripple spawned which. Thread folds
//! appear as rows."* The guards G1–G8 of its validation plan (G9 is a
//! unit test beside `ledger::dumps`, where its counter lives). Fixtures are
//! seeds written to a scratch dir and read through `Seed::load`, the one
//! loader; the rest read the crate's test fixture for prod
//! (`tests/fixtures/seed/soup/prod.ledger.json`, named by `seed_dir()` —
//! a real face never names it).
//!
//! V1.4.S2 (G10–G12): the DATA. `cauldron-build` shows the soup crate's
//! curated ledger (its own seed, `soup::demo::install`); prod, and every
//! other environment, shows only real data — its bucket log's dumps with
//! nothing run; and the ledger has one loader, run once per open.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;
use tui::cauldron::app::{App, CTab, Focus, Modal};
use tui::cauldron::bus::BusPage;
use tui::cauldron::ledger::{self, Seed};
use tui::cauldron::status::{self, SAct, StatusTab};
use tui::cauldron::store::seed_dir;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "v14s1-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_in(root: &Path, name: &str) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: name.into(), name_source: Source::Default, data_root: root.join("data").join(name) }
}

/// The prod app on the crate's fixture seeds (`seed_dir()`, whose
/// `soup/prod.ledger.json` is this file's ledger), on Status › Ledger.
fn app() -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut a = App::open(env_in(&scratch_root(), "prod"), seed_dir(), true).expect("opens");
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Ledger);
    a.focus = Focus::Content;
    a
}

/// A seed file `json` in a scratch seeds dir, read by `Seed::load`. `dir` is
/// a fresh, empty scratch dir of its own, so `own_seed(dir, "ledger.json")`
/// never fires and the fixture under `seeds/soup/` is what loads.
fn load(json: &serde_json::Value) -> anyhow::Result<Seed> {
    let seeds = scratch_root();
    std::fs::create_dir_all(seeds.join("soup")).unwrap();
    std::fs::write(seeds.join("soup").join("fx.ledger.json"), serde_json::to_string_pretty(json).unwrap()).unwrap();
    let dir = scratch_root();
    Seed::load(Some(&seeds), "fx", &dir)
}

/// The prod app with its ledger replaced by fixture `json` and no log, every
/// dump open.
fn fixture(json: serde_json::Value) -> App {
    let mut a = app();
    a.ledger = load(&json).expect("the fixture loads");
    a.sends.sends.clear();
    open_all(&mut a);
    a
}

fn open_all(a: &mut App) {
    let ds = ledger::dumps(&a.ledger, &a.sends.sends);
    let newest = ledger::newest(&ds);
    for d in &ds {
        if Some(d.seq) != newest {
            a.stnav.insert(format!("ld:{}", d.seq));
        }
    }
}

fn text(l: &Line) -> String {
    l.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn lines(a: &App, w: usize) -> Vec<String> {
    status::view(a, w).lines.iter().map(text).collect()
}

fn width(s: &str) -> usize {
    tui_render::compose::str_width(s)
}

fn press(a: &mut App, code: KeyCode) {
    a.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn select_key(a: &mut App, key: &str) {
    let v = status::view(a, 120);
    a.rows[0] = v.items.iter().position(|i| i.key == key).unwrap_or_else(|| panic!("no row {key}: {:?}", v.items.iter().map(|i| i.key.clone()).collect::<Vec<_>>()));
}

fn selected_key(a: &App) -> String {
    status::view(a, 120).items[a.rows[0]].key.clone()
}

fn ex(ripple: &str, kind: &str, children: Vec<serde_json::Value>) -> serde_json::Value {
    serde_json::json!({"ripple": ripple, "kind": kind, "state": "done", "ms": 1200, "line": format!("{ripple} line"), "children": children})
}

// ── G1: not a band ─────────────────────────────────────────────────────────

/// No ledger line carries a chevron, and a dump's line 1 starts with its
/// state dot then `Dump N`. Opposed: the Tokens sub-tab of the same app
/// still draws `▾`/`▸`, so the assertion reads lines where a chevron CAN be.
#[test]
fn g1_a_dump_is_an_event_block_never_a_band() {
    let mut a = app();
    open_all(&mut a);
    a.stnav.insert("lx:11:0.2".into()); // one ripple shut: its own suffix is checked too
    let ds = ledger::dumps(&a.ledger, &a.sends.sends);
    assert!(ds.len() >= 4);
    let ls = lines(&a, 120);
    for l in &ls[1..] {
        assert!(!l.contains('▾') && !l.contains('▸'), "a chevron on the Ledger: {l:?}");
    }
    let v = status::view(&a, 120);
    for d in &ds {
        let it = v.items.iter().find(|i| i.key == format!("ld:{}", d.seq)).unwrap();
        let head = text(&v.lines[it.range.start]);
        assert!(head.starts_with(&format!("● Dump {}", d.seq)), "{head:?}");
    }
    a.set_sttab(StatusTab::Tokens);
    assert!(lines(&a, 120).iter().skip(1).any(|l| l.contains('▾') || l.contains('▸')), "Tokens still draws bands");
}

// ── G2: phases ─────────────────────────────────────────────────────────────

fn phased() -> serde_json::Value {
    serde_json::json!({"dumps": [
        {"seq": 1, "at": "-1h", "text": "a phased dump", "ripples": [{
            "ripple": "dump", "kind": "shell", "state": "done", "ms": 9000,
            "steps": [
                {"name": "discover", "word": "call", "state": "done", "ms": 1000, "calls": [0]},
                {"name": "stage", "word": "call", "state": "done", "ms": 2000, "calls": [1, 2]},
                {"name": "land", "word": "native", "state": "done", "ms": 300}
            ],
            "children": [ex("scout", "type_two", vec![]), ex("stager_a", "type_one", vec![]), ex("stager_b", "type_one", vec![])]
        }]},
        {"seq": 2, "at": "-2h", "text": "an agent's dump", "ripples": [
            ex("agent", "type_two", vec![ex("helper_a", "type_one", vec![]), ex("helper_b", "type_one", vec![])])
        ]}
    ]})
}

/// A dump whose top ripple is a stepped type 3 shows its three phases in
/// step order, each with the ripples ITS `calls` name under it: child 0
/// under `discover`, 1 and 2 under `stage`, none under `land`. Opposed: a
/// dump whose top ripple is a type 2 has no phase and its children as the
/// plain tree.
#[test]
fn g2_phases_hold_the_ripples_their_steps_called() {
    let a = fixture(phased());
    let v = status::view(&a, 120);
    let pos = |k: &str| v.items.iter().position(|i| i.key == k).unwrap_or_else(|| panic!("{k}: {:?}", v.items.iter().map(|i| &i.key).collect::<Vec<_>>()));
    let parent = |k: &str| v.items[pos(k)].parent.clone().unwrap();
    let lp: Vec<&str> = v.items.iter().filter(|i| i.key.starts_with("lp:1:")).map(|i| i.key.as_str()).collect();
    assert_eq!(lp, ["lp:1:0:0", "lp:1:0:1", "lp:1:0:2"], "three phases, in step order");
    let ls = lines(&a, 120);
    for (k, name) in [("lp:1:0:0", "discover"), ("lp:1:0:1", "stage"), ("lp:1:0:2", "land")] {
        assert!(ls[v.items[pos(k)].range.start].contains(name), "{k}: {}", ls[v.items[pos(k)].range.start]);
    }
    assert_eq!(parent("lx:1:0.0"), "lp:1:0:0", "scout under discover");
    assert_eq!(parent("lx:1:0.1"), "lp:1:0:1", "stager_a under stage");
    assert_eq!(parent("lx:1:0.2"), "lp:1:0:1", "stager_b under stage");
    assert!(pos("lp:1:0:0") < pos("lx:1:0.0") && pos("lx:1:0.0") < pos("lp:1:0:1") && pos("lx:1:0.2") < pos("lp:1:0:2"), "each under its phase, in order");
    assert!(!v.items.iter().any(|i| i.parent.as_deref() == Some("lp:1:0:2")), "land called nothing");
    // Opposed: the type 2's dump.
    assert!(!v.items.iter().any(|i| i.key.starts_with("lp:2:")), "a type 2 has no phases");
    assert_eq!(parent("lx:2:0.0"), "lx:2:0");
    assert_eq!(parent("lx:2:0.1"), "lx:2:0");
}

// ── G3: spawn lines ────────────────────────────────────────────────────────

/// `dump` ③ → `apply` ③ → `author` ②, and `apply`'s sibling `tail`: the
/// author's connector is one rail deeper than apply's, under apply's glyph;
/// siblings read `├─` then `╰─`, so a constant connector fails.
#[test]
fn g3_rails_show_which_ripple_spawned_which() {
    let a = fixture(serde_json::json!({"dumps": [{"seq": 3, "at": "-1h", "text": "three deep", "ripples": [
        ex("dump", "shell", vec![ex("apply", "shell", vec![ex("author", "type_two", vec![])]), ex("tail", "type_two", vec![])])
    ]}]}));
    let ls = lines(&a, 120);
    let row = |t: &str| ls.iter().find(|l| l.contains(t)).cloned().unwrap_or_else(|| panic!("{t}:\n{}", ls.join("\n")));
    let at = |l: &str, t: &str| width(&l[..l.find(t).unwrap_or_else(|| panic!("{t} on {l:?}"))]);
    let (apply, author, tail) = (row("③ apply"), row("② author"), row("② tail"));
    assert!(apply.contains("├─✓ ③ apply"), "{apply:?}");
    assert!(tail.contains("╰─✓ ② tail"), "{tail:?}");
    assert!(author.contains("│ ╰─✓ ② author"), "apply's rail runs on past it to tail: {author:?}");
    assert_eq!(at(&author, "╰─"), at(&apply, "├─") + 2, "one rail deeper:\n{apply}\n{author}");
    assert_eq!(at(&apply, "├─"), at(&row("③ dump"), "✓"), "under the glyph of what spawned it");
}

// ── G4: folds ──────────────────────────────────────────────────────────────

/// A seed dump with `"thread"` is a FOLD: `⟲ Fold · <thread>` on its line 1;
/// one without is `Dump N`, both in one fixture. And the seed still refuses
/// an unknown field (`deny_unknown_fields` kept) — opposed by the same file
/// spelled right loading.
#[test]
fn g4_a_thread_fold_is_a_row_of_its_own() {
    let a = fixture(serde_json::json!({"dumps": [
        {"seq": 4, "at": "-1h", "text": "folded", "thread": "release notes", "ripples": [ex("fold", "shell", vec![])]},
        {"seq": 5, "at": "-2h", "text": "a dump", "ripples": [ex("dump", "shell", vec![])]}
    ]}));
    let v = status::view(&a, 120);
    let head = |seq: u64| text(&v.lines[v.items.iter().find(|i| i.key == format!("ld:{seq}")).unwrap().range.start]);
    assert!(head(4).starts_with("● ⟲ Fold · release notes"), "{:?}", head(4));
    assert!(!head(4).contains("Dump 4"), "{:?}", head(4));
    assert!(head(5).starts_with("● Dump 5"), "{:?}", head(5));
    assert!(head(4).contains("1 ripple") && head(4).contains("✓ done"), "a fold's metadata as a dump's: {:?}", head(4));
    let bad = serde_json::json!({"dumps": [{"seq": 4, "at": "-1h", "threadd": "release notes"}]});
    let err = load(&bad).expect_err("an unknown field is refused");
    assert!(format!("{err:#}").contains("threadd"), "{err:#}");
    let good = serde_json::json!({"dumps": [{"seq": 4, "at": "-1h", "thread": "release notes"}]});
    assert_eq!(load(&good).expect("spelled right, it loads").dumps[0].thread, "release notes");
}

// ── G5: order ──────────────────────────────────────────────────────────────

/// Three dumps an hour apart list in the order `NEWEST_FIRST` says; the
/// assertion reads the const, so the answer to Q-L1 is a one-line flip.
#[test]
fn g5_the_order_is_the_one_switch() {
    let a = fixture(serde_json::json!({"dumps": [
        {"seq": 7, "at": "-3h", "text": "oldest"},
        {"seq": 9, "at": "-1h", "text": "newest"},
        {"seq": 8, "at": "-2h", "text": "middle"}
    ]}));
    let order: Vec<String> = status::view(&a, 120).items.iter().filter(|i| i.key.starts_with("ld:")).map(|i| i.key.clone()).collect();
    let want = if ledger::NEWEST_FIRST { ["ld:9", "ld:8", "ld:7"] } else { ["ld:7", "ld:8", "ld:9"] };
    assert_eq!(order, want, "NEWEST_FIRST = {}", ledger::NEWEST_FIRST);
    // The newest is the one open by default, whichever end it is at.
    let b = {
        let mut b = app();
        b.ledger = a.ledger.clone();
        b.sends.sends.clear();
        b
    };
    assert!(status::is_open(&b, "ld:9") && !status::is_open(&b, "ld:7") && !status::is_open(&b, "ld:8"));
}

// ── G6: no prose ───────────────────────────────────────────────────────────

/// Every line that is not a row's own line is the dump's words, a `⎿ N
/// ripples` summary, a blank between blocks, or one of the two allowed
/// empty texts — at 120 and 60 columns. Opposed: the dumps' own words (two
/// lines of them, on Dump 11) pass, and are seen to.
#[test]
fn g6_no_sentence_but_the_dumps_own_words() {
    let mut a = app();
    a.dump("a thought with nothing run"); // the newest, open: ◇ nothing has run
    a.stnav.insert("ld:10".into()); // open; 11 stays shut: its ⎿ summary
    let ds = ledger::dumps(&a.ledger, &a.sends.sends);
    let words: Vec<String> = ds.iter().flat_map(|d| d.words()).collect();
    assert!(ds.iter().any(|d| d.words().len() == 2), "a dump with two lines of words is in the frame");
    for w in [120usize, 60] {
        let v = status::view(&a, w);
        let starts: std::collections::HashMap<usize, &str> = v.items.iter().map(|i| (i.range.start, i.key.as_str())).collect();
        let mut seen_words = 0;
        for (n, l) in v.lines.iter().map(text).enumerate().skip(1) {
            if let Some(k) = starts.get(&n) {
                if k.ends_with(":none") {
                    assert_eq!(l.trim(), "⎿ ◇ nothing has run for it yet", "{w}");
                }
                continue;
            }
            let t = l.trim();
            let summary = t.strip_prefix("⎿ ").is_some_and(|r| {
                let digits = r.chars().take_while(char::is_ascii_digit).count();
                digits > 0 && (r[digits..].starts_with(" ripple") || r[digits..].starts_with(" ripples"))
            });
            let own = !t.is_empty() && words.iter().any(|wd| wd.starts_with(t.trim_end_matches('…')));
            seen_words += own as usize;
            assert!(t.is_empty() || summary || own || t == "no dump yet", "{w} columns, a line that is none of the dump's: {l:?}");
        }
        assert!(seen_words >= 3, "{w}: the words are checked, not skipped ({seen_words})");
    }
}

// ── G7: widths ─────────────────────────────────────────────────────────────

/// At 120, 80, 60 and 40 columns no ledger line is wider than the view, and
/// a dump's line 1 keeps `Dump N` whole and its metadata standing; a fold's
/// thread is title, and crops first. The fixture's running dump with a
/// failure has the longest metadata the face draws.
#[test]
fn g7_every_line_fits_at_120_80_60_and_40() {
    let a = fixture(serde_json::json!({"dumps": [
        {"seq": 1234, "at": "-1d", "text": "a dump whose words are long enough that at forty columns they have to crop somewhere well before the end", "ripples": [
            {"ripple": "dump", "kind": "shell", "state": "running", "ms": 130000, "children": [ex("a", "type_two", vec![]), {"ripple": "b", "kind": "type_one", "state": "failed", "ms": 5}]}
        ]},
        {"seq": 5, "at": "-2d", "thread": "a thread with a long display name that cannot fit", "ripples": [ex("fold", "shell", vec![])]}
    ]}));
    for w in [120usize, 80, 60, 40] {
        let ls = lines(&a, w);
        for l in &ls {
            assert!(width(l) <= w, "{w}: {l:?} is {} wide", width(l));
        }
        let h = ls.iter().find(|l| l.starts_with("● Dump 1234")).unwrap_or_else(|| panic!("{w}: Dump 1234 whole:\n{}", ls.join("\n")));
        assert!(h.contains("▶ running") && h.contains("3 ripples"), "{w}: the metadata stands: {h:?}");
        let f = ls.iter().find(|l| l.starts_with("● ⟲ Fold")).unwrap();
        assert!(f.contains("1 ripple"), "{w}: a fold's metadata stands: {f:?}");
        if w <= 80 {
            assert!(!f.contains("cannot fit"), "{w}: the thread name crops: {f:?}");
        }
    }
}

// ── G8: keys ───────────────────────────────────────────────────────────────

/// → on a shut dump opens it (its ripples appear), → again steps to its
/// first row, ← on it shuts it; Enter on a ripple explores THAT path. Opposed:
/// → on a ripple that ran nothing never toggles anything (it explores, as
/// today), and → on a phase that called nothing stays put.
#[test]
fn g8_keys_open_step_shut_and_explore() {
    let mut a = app();
    let has = |a: &App, k: &str| status::view(a, 120).items.iter().any(|i| i.key == k);
    assert!(!has(&a, "lx:10:0"), "Dump 10 starts shut");
    select_key(&mut a, "ld:10");
    press(&mut a, KeyCode::Right);
    assert!(has(&a, "lx:10:0") && has(&a, "lx:10:0.2.1.1"), "→ opens it");
    assert_eq!(selected_key(&a), "ld:10");
    press(&mut a, KeyCode::Right);
    assert_eq!(selected_key(&a), "lx:10:0", "→ again steps to its first row");
    select_key(&mut a, "ld:10");
    press(&mut a, KeyCode::Left);
    assert!(!has(&a, "lx:10:0"), "← shuts it");
    press(&mut a, KeyCode::Right);
    select_key(&mut a, "lx:10:0.2.1.1");
    press(&mut a, KeyCode::Enter);
    match &a.modal {
        Some(Modal::Bus { stack }) => assert_eq!(stack.last().unwrap().0, BusPage::Exec { seq: 10, path: vec![0, 2, 1, 1] }),
        other => panic!("{other:?}"),
    }
    a.modal = None;
    // Opposed: a ripple that ran nothing — → toggles nothing.
    select_key(&mut a, "lx:10:0.0");
    let before = a.stnav.clone();
    let v = status::view(&a, 120);
    assert!(matches!(v.items[a.rows[0]].act, SAct::Exec { inside: 0, .. }));
    press(&mut a, KeyCode::Right);
    assert_eq!(a.stnav, before, "→ on a ripple that ran nothing opens and shuts nothing");
    a.modal = None;
    // Opposed: a phase that called nothing — → stays.
    select_key(&mut a, "lp:10:0:0");
    press(&mut a, KeyCode::Right);
    assert_eq!(selected_key(&a), "lp:10:0:0", "load_dump called nothing");
    select_key(&mut a, "lp:10:0:1");
    press(&mut a, KeyCode::Right);
    assert_eq!(selected_key(&a), "lx:10:0.0", "fragment called the fragment ripple");
}

// ── V1.4.S2: G10–G12, the data ─────────────────────────────────────────────

/// An environment named `name` under `root`, as the CLI resolves one.
fn real_env(root: &Path, name: &str) -> Environment {
    env_in(root, "prod").for_name(name).unwrap()
}

/// The Ledger's lines at 120 with every dump and every ripple open.
fn ledger_lines(a: &mut App) -> Vec<String> {
    a.set_ctab(CTab::Status);
    a.set_sttab(StatusTab::Ledger);
    a.focus = Focus::Content;
    open_all(a);
    lines(a, 120)
}

/// G10 — **prod is real-only.** A seeds dir holding a `ledger.json` where
/// globs2's prod fallback read it (and the bucket-log fixture beside it):
/// prod loads NO seeded dumps from it, and its Ledger shows only the bucket
/// log's dumps, every one `◇ nothing has run for it yet`. Red: restore the
/// `env == "prod" → seeds/ledger.json` arm. Opposed: the same file, named
/// for its environment (`soup/<env>.ledger.json`), loads ≥ 1 dump with ≥ 1
/// ripple, and `cauldron-build` loads ≥ 1 dump with ≥ 1 ripple (G11) — so
/// a constant "nothing" fails.
#[test]
fn g10_prod_reads_no_seed_and_shows_only_its_bucket_log() {
    let fx = std::fs::read_to_string(seed_dir().join("soup").join("prod.ledger.json")).unwrap();
    let seeds = scratch_root();
    std::fs::write(seeds.join("ledger.json"), &fx).unwrap();
    std::fs::copy(seed_dir().join("dumps.json"), seeds.join("dumps.json")).unwrap();
    let empty = scratch_root();
    assert!(Seed::load(Some(&seeds), "prod", &empty).unwrap().dumps.is_empty(), "prod takes nothing from <seeds>/ledger.json");

    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let mut a = App::open(env_in(&scratch_root(), "prod"), seeds.clone(), false).expect("opens");
    assert!(a.ledger.dumps.is_empty(), "no seeded dumps on prod");
    assert!(!a.sends.sends.is_empty(), "the bucket log has dumps (the predicate can see blocks)");
    let r = ledger_lines(&mut a);
    let s = r.join("\n");
    let blocks = r.iter().filter(|l| l.starts_with("● ")).count();
    assert_eq!(blocks, a.sends.sends.len(), "one block per bucket-log dump:\n{s}");
    assert_eq!(r.iter().filter(|l| l.contains("◇ nothing has run for it yet")).count(), blocks, "every one with nothing run:\n{s}");
    assert!(!r.iter().any(|l| l.contains('①') || l.contains('②') || l.contains('③')), "no ripple row at all:\n{s}");

    // Opposed: the same bytes, named for an environment, DO load.
    std::fs::create_dir_all(seeds.join("soup")).unwrap();
    std::fs::write(seeds.join("soup").join("fx.ledger.json"), &fx).unwrap();
    let named = Seed::load(Some(&seeds), "fx", &empty).unwrap();
    assert!(!named.dumps.is_empty() && named.dumps.iter().any(|d| !d.ripples.is_empty()), "the file itself is a ledger");
}

/// `cauldron-build`, created as the face creates it (`store::create_env`
/// runs `soup::demo::install`), opened with an EMPTY scratch seeds dir.
fn cauldron_build(root: &Path) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let here = env_in(root, "prod");
    here.ensure_dirs().unwrap();
    tui::cauldron::store::create_env(&here, soup::demo::ENV).expect("creates cauldron-build");
    let seeds = scratch_root();
    assert_eq!(std::fs::read_dir(&seeds).unwrap().count(), 0, "the seeds dir is empty");
    App::open(real_env(root, soup::demo::ENV), seeds, false).expect("opens")
}

/// G11 — **the demo is REL-3's data, not a test fixture.** With NOTHING in
/// the seeds dir, `cauldron-build`'s Ledger draws a fold row and phases only
/// the soup crate's curated dataset holds (`libs/soup/demo/cauldron-build/
/// ledger.json`: dump 104 `"thread": "Ledger rails"`; dump 103's `apply`
/// phase, `"line": "1 rolled back, 1 retried"`, and the failed `tranche` it
/// called). This test writes no ledger, so it cannot witness its own write;
/// the crate's prod fixture has no `thread`, no `tranche` and no such line.
/// Red: the loader pointed at an empty dataset.
#[test]
fn g11_cauldron_build_shows_the_soup_crates_curated_ledger() {
    let mut a = cauldron_build(&scratch_root());
    assert!(!a.ledger.dumps.is_empty() && a.ledger.dumps.iter().any(|d| !d.ripples.is_empty()), "≥ 1 dump with ≥ 1 ripple");
    let r = ledger_lines(&mut a);
    let s = r.join("\n");
    assert!(r.iter().any(|l| l.starts_with("● ⟲ Fold · Ledger rails")), "the fold row:\n{s}");
    let phase = r.iter().find(|l| l.contains("├─✓ apply  1 rolled back, 1 retried")).unwrap_or_else(|| panic!("dump 103's apply phase:\n{s}"));
    assert!(phase.trim_end().ends_with("11s"), "with its own duration: {phase}");
    assert!(r.iter().any(|l| l.contains("├─✕ ③ tranche  rolled back: two writers on M-4")), "the ripple that phase called:\n{s}");
    // The fixture cannot be what drew them.
    let fx = std::fs::read_to_string(seed_dir().join("soup").join("prod.ledger.json")).unwrap();
    assert!(!fx.contains("Ledger rails") && !fx.contains("1 rolled back, 1 retried") && !fx.contains("tranche") && !fx.contains("\"thread\""), "the fixture holds none of it");
}

/// G12 — **one loader.** The ledger is read once per `App::open`, and once
/// more per `switch_env` (which opens), and never by drawing: twenty
/// renders of the Ledger and a key move read nothing. Red: a
/// `Seed::load` call in `status::view`. Opposed: the counter moves on open
/// and on switch, so it can see a read.
#[test]
fn g12_the_ledger_is_read_once_per_open_and_never_by_drawing() {
    let root = scratch_root();
    let n0 = ledger::loads_on_this_thread();
    let mut a = cauldron_build(&root);
    assert_eq!(ledger::loads_on_this_thread() - n0, 1, "one read on open");
    for _ in 0..20 {
        ledger_lines(&mut a);
    }
    press(&mut a, KeyCode::Down);
    let _ = tui::cauldron::render_to_text(&mut a, 120, 40).unwrap();
    assert_eq!(ledger::loads_on_this_thread() - n0, 1, "drawing reads nothing");
    a.switch_env("prod").expect("switches");
    assert_eq!(ledger::loads_on_this_thread() - n0, 2, "a switch is an open: one more read");
    assert!(a.ledger.dumps.is_empty(), "and prod, opened on the same empty seeds, has no seeded dumps");
}

// ── V1.4.S2b: G13–G14, no fixture for a real environment; an installed face ──

/// G13 — **a real environment gets NO fixture, whatever any directory
/// holds.** A real face opens on `store::no_seeds(env)` (the CLI's default),
/// so the fixture fallback (`<seeds>/soup/<env>.ledger.json`) must not run
/// for it at all. The test PLANTS the crate's prod ledger fixture exactly
/// where that fallback would look under `no_seeds`, so the forbidden read is
/// reachable (doctrine 5): prod opened as a real face loads none of it, and
/// `cauldron-build`, opened the same way, still loads its own seed. Red:
/// `ledger::fixtures` returning `Some(seeds)` for every dir (the V1.4.S2
/// behaviour). Opposed: the SAME bytes in a NAMED fixture dir (`--seeds`)
/// do load on prod, and `fixtures` answers `Some` for a named dir — so a
/// constant `None` fails too.
#[test]
fn g13_a_real_environment_reads_no_fixture_even_one_planted_under_no_seeds() {
    let fx = std::fs::read_to_string(seed_dir().join("soup").join("prod.ledger.json")).unwrap();
    let root = scratch_root();
    let prod = real_env(&root, "prod");
    let real = tui::cauldron::store::no_seeds(&prod);
    std::fs::create_dir_all(real.join("soup")).unwrap();
    std::fs::write(real.join("soup").join("prod.ledger.json"), &fx).unwrap();
    std::fs::write(real.join("soup").join(format!("{}.ledger.json", soup::demo::ENV)), &fx).unwrap();
    assert_eq!(ledger::fixtures(&real, &prod), None, "no_seeds names no fixture");

    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let a = App::open(prod.clone(), real.clone(), false).expect("opens");
    assert!(a.ledger.dumps.is_empty(), "prod, opened as a real face, read the fixture planted under no_seeds: {} dumps", a.ledger.dumps.len());

    // cauldron-build, opened as a real face, reads its OWN seed (the soup
    // crate's), not the fixture planted for its name.
    let here = env_in(&root, "prod");
    here.ensure_dirs().unwrap();
    tui::cauldron::store::create_env(&here, soup::demo::ENV).expect("creates cauldron-build");
    let cb = real_env(&root, soup::demo::ENV);
    let b = App::open(cb.clone(), tui::cauldron::store::no_seeds(&cb), false).expect("opens");
    assert!(b.ledger.dumps.iter().any(|d| d.thread == "Ledger rails"), "cauldron-build shows the soup crate's fold");

    // Opposed: the same bytes, NAMED (a test's `--seeds`), load on prod.
    let named = scratch_root();
    std::fs::create_dir_all(named.join("soup")).unwrap();
    std::fs::write(named.join("soup").join("prod.ledger.json"), &fx).unwrap();
    assert_eq!(ledger::fixtures(&named, &prod), Some(named.as_path()), "a named dir is a fixture");
    let n = App::open(prod, named, false).expect("opens");
    assert!(!n.ledger.dumps.is_empty() && n.ledger.dumps.iter().any(|d| !d.ripples.is_empty()), "the named fixture loads ≥ 1 dump with ≥ 1 ripple");
}

/// A sandbox profile that denies every read under `dirs` (and allows the
/// rest), for `sandbox-exec -p`.
fn deny_reads(dirs: &[&Path]) -> String {
    let mut p = String::from("(version 1)(allow default)");
    for d in dirs {
        let d = d.canonicalize().unwrap_or_else(|e| panic!("{}: {e}", d.display()));
        p.push_str(&format!("(deny file-read* (subpath \"{}\"))", d.display()));
    }
    p
}

/// G14 — **an INSTALLED face draws the demo.** The built face is COPIED to
/// `<HOME>/.cauldron/bin/` (the install layout) and run from there with a
/// scratch HOME and CAULDRON_HOME, under `sandbox-exec` with every read of
/// this source tree and of the build slot DENIED — so a ledger, a seed or a
/// fixture resolved relative to `CARGO_MANIFEST_DIR` / the build dir cannot
/// be what draws it. `cauldron-build` (created through `store::create_env`,
/// the face's own `soup::demo::seed_env` path) draws the soup crate's fold
/// row; prod, with the crate's fixture planted under `no_seeds`, draws no
/// ripple (G13 through the real CLI). Opposed: the same copy with
/// `--seeds <dir>` holding that fixture draws its ripples. Sentinels: the
/// profile really denies the tree (`cat` of this crate's Cargo.toml fails
/// under it) and really allows the scratch (`cat` of a scratch file works).
#[cfg(target_os = "macos")]
#[test]
fn g14_an_installed_copy_of_the_face_draws_the_demo_with_the_source_tree_unreadable() {
    let scratch = scratch_root();
    let home = scratch.join("home");
    let croot = home.join(".cauldron");
    std::fs::create_dir_all(&croot).unwrap();
    let here = env_in(&croot, "prod");
    here.ensure_dirs().unwrap();
    tui::cauldron::store::create_env(&here, soup::demo::ENV).expect("creates cauldron-build");
    let prod = real_env(&croot, "prod");
    let fx = std::fs::read_to_string(seed_dir().join("soup").join("prod.ledger.json")).unwrap();
    let planted = tui::cauldron::store::no_seeds(&prod).join("soup");
    std::fs::create_dir_all(&planted).unwrap();
    std::fs::write(planted.join("prod.ledger.json"), &fx).unwrap();
    let named = scratch.join("named-seeds");
    std::fs::create_dir_all(named.join("soup")).unwrap();
    std::fs::write(named.join("soup").join("prod.ledger.json"), &fx).unwrap();

    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let tree = crate_dir.join("..").join("..");
    let exe = std::env::current_exe().unwrap();
    // The test binary runs from the build slot: `<slot>/<profile>/deps/it-…`.
    let slot = exe.ancestors().nth(3).unwrap().to_path_buf();
    let profile = deny_reads(&[&tree, &slot]);
    // Sentinels: the profile denies the tree and allows the scratch.
    let sb = |args: &[&OsStr]| std::process::Command::new("/usr/bin/sandbox-exec").arg("-p").arg(&profile).args(args).output().expect("sandbox-exec runs");
    let denied = sb(&[OsStr::new("/bin/cat"), crate_dir.join("Cargo.toml").as_os_str()]);
    assert!(!denied.status.success(), "the sandbox let the face read the source tree");
    let probe = scratch.join("probe.txt");
    std::fs::write(&probe, "x").unwrap();
    assert!(sb(&[OsStr::new("/bin/cat"), probe.as_os_str()]).status.success(), "the sandbox denies the scratch too");

    let pre = ["-p", profile.as_str()];
    let run = |env_name: &str, extra: &[&str]| {
        let o = crate::face::Face::installed(&scratch.join("face"), &croot.join("bin"), Some((OsStr::new("/usr/bin/sandbox-exec"), &pre)))
            .args(["--cauldron", "--dump", "--env", env_name, "--status-tab", "ledger", "--size", "120x60"])
            .args(extra)
            .env("HOME", &home)
            .env("CAULDRON_HOME", &croot)
            .env_remove("ENV")
            .env_remove("CAULDRON_ENV")
            .current_dir(&scratch)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        assert!(o.status.success(), "{env_name} {extra:?}: {:?}: {}\n{out}", o.status, String::from_utf8_lossy(&o.stderr));
        out
    };
    let demo = run(soup::demo::ENV, &[]);
    assert!(demo.contains("⟲ Fold · Ledger rails"), "the installed face draws the soup crate's fold row:\n{demo}");
    let ripple = |s: &str| s.contains('①') || s.contains('②') || s.contains('③');
    let real = run("prod", &[]);
    assert!(!ripple(&real) && !real.contains("⟲ Fold"), "prod drew the fixture planted under no_seeds:\n{real}");
    let shown = run("prod", &["--seeds", named.to_str().unwrap()]);
    assert!(ripple(&shown), "opposed: the same fixture, named, is drawn:\n{shown}");
}
