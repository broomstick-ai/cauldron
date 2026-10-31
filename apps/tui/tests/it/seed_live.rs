//! **THE LIVE PROOF** of the seeding ripple's real type-one call (8e4fca10
//! msg 112): the fake soup's every candidate, his boundaries, ONE batched
//! call through the `type-one` CLI (ripple/v2) with the real model.
//!
//! Billed, so `#[ignore]`d; run by hand:
//!
//! ```text
//! CAULDRON_TYPE_ONE_BIN=/path/to/type-one SEED_LIVE_ADAPTER=jev \
//!   cargo test -p tui --test it seed_live:: -- --ignored --nocapture
//! ```
//!
//! `SEED_LIVE_ADAPTER` (`jev` | `haiku`, default `jev`) is set on the
//! ripple's MODEL row (`model.trusted`), exactly as the Ripples tab would.
//! The test FAILS if the stand-in answered: a fallback is not a proof.
//!
//! The second test is the FACE'S path, end to end: `+ new cluster` typed into
//! a real `App` on the fake soup, the seed policy's own MODEL row (no edit),
//! the binary found the way the face finds it (`CAULDRON_TYPE_ONE_BIN`, else
//! `type-one` on PATH), the call off the UI loop, landed on a quiet tick.

use std::path::Path;

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tui::cauldron::app::App;
use tui::cauldron::seeding::{self, LocalMatcher, SEED_RIPPLE};
use tui::cauldron::store::seed_dir;

#[test]
#[ignore = "billed: makes a real type-one call"]
fn the_seeding_asks_the_real_model_in_one_batched_call() {
    let root = std::env::temp_dir().join(format!("seed-live-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let env = Environment {
        root: root.to_path_buf(),
        root_source: Source::CauldronHome,
        name: "prod".into(),
        name_source: Source::Default,
        data_root: Path::new(&root).join("data").join("prod"),
    };
    let a = App::open(env, seed_dir(), false).expect("seed loads");
    let mut policy = a.rstore.live.clone();
    let side = match std::env::var("SEED_LIVE_ADAPTER").as_deref() {
        Ok("haiku") => "primary",
        _ => "secondary",
    };
    let i = policy.index(SEED_RIPPLE).unwrap();
    policy.file.ripple[i].model.trusted = Some(side.into());
    let items = a.seed_candidates();
    let boundaries = std::env::var("SEED_LIVE_BOUNDARIES").unwrap_or_else(|_| {
        "the overnight dreaming ripple: its consolidation of the corpus, and replays of recorded runs against policy variants".into()
    });
    let t0 = std::time::Instant::now();
    let s = seeding::seed(&policy, &a.seed_live(), &LocalMatcher, &boundaries, &items, "dream-lab", "r-live", chrono::Local::now());
    let wall = t0.elapsed().as_millis();
    let run = s.run.expect("a run");
    println!("BOUNDARIES: {boundaries}");
    println!("CANDIDATES: {}", items.len());
    println!("RUN LINE: {}", run.line);
    println!("STEP MODEL: {}", run.tree[0].model);
    println!("STEP OUTPUT: {}", run.tree[0].output);
    println!("WALL: {wall} ms · COST: {:?}", run.cost);
    for it in &run.tree[0].items {
        let st = &it.steps[0];
        println!("  {:>5}  {:<70}  {}", st.p.values().next().map_or("—".into(), |p| format!("{p:.3}")), it.text, st.line);
    }
    println!("CLAIMED regions {:?} repos {:?} links {:?}", s.regions, s.repos, s.links);
    assert!(!run.line.starts_with("stand-in"), "the stand-in answered, so this proves nothing: {}", run.line);
    assert_eq!(run.tree[0].items.len(), items.len());
}

#[test]
#[ignore = "billed: makes a real type-one call"]
fn a_cluster_made_in_the_face_opens_at_once_and_is_seeded_by_the_model() {
    let root = std::env::temp_dir().join(format!("seed-face-{}-{}", std::process::id(), uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&root).unwrap();
    let env = Environment {
        root: root.to_path_buf(),
        root_source: Source::CauldronHome,
        name: "prod".into(),
        name_source: Source::Default,
        data_root: Path::new(&root).join("data").join("prod"),
    };
    let mut a = App::open(env, seed_dir(), true).expect("seed loads");
    let key = |c: KeyCode| Event::Key(KeyEvent::new(c, KeyModifiers::NONE));
    a.open_new_cluster(None);
    for c in "Dream Lab".chars() {
        a.handle(key(KeyCode::Char(c)));
    }
    a.handle(key(KeyCode::Enter));
    let boundaries = "the overnight dreaming ripple: its consolidation of the corpus, and replays of recorded runs against policy variants";
    for c in boundaries.chars() {
        a.handle(key(KeyCode::Char(c)));
    }
    let t0 = std::time::Instant::now();
    a.handle(key(KeyCode::Enter));
    let opened = t0.elapsed();
    assert!(a.modal.is_none(), "{:?}", a.modal);
    let g = a.forest.clusters.iter().position(|x| x.name == "Dream Lab").expect("made");
    let bg = a.forest.get(g).background.iter().find(|b| b.kind == "seed").expect("a seed row").clone();
    println!("OPENED IN: {} ms (the call is off the loop)", opened.as_millis());
    println!("WHILE SEEDING: [{}] {}", bg.state, bg.line);
    println!("NOTICE: {:?}", a.notice);
    assert_eq!(bg.state, "running", "it says it is seeding: {bg:?}");
    assert!(a.wait_seeding(std::time::Duration::from_secs(120)), "the seeding landed");
    let g = a.forest.clusters.iter().position(|x| x.name == "Dream Lab").expect("made");
    let bg = a.forest.get(g).background.iter().find(|b| b.kind == "seed").expect("a seed row").clone();
    println!("LANDED AFTER: {} ms", t0.elapsed().as_millis());
    println!("RUN LINE: [{}] {}", bg.state, bg.line);
    println!("NOTICE: {:?}", a.notice);
    let run = a.rstore.runs().into_iter().find(|r| r.ripple == SEED_RIPPLE).expect("the run").clone();
    println!("STEP MODEL: {}", run.tree[0].model);
    println!("STEP OUTPUT: {}", run.tree[0].output);
    let claims: Vec<&str> = a.corpus.claims[g].iter().map(|&r| a.corpus.regions[r].id.as_str()).collect();
    println!("CLAIMED regions {claims:?}");
    assert_eq!(bg.state, "done");
    assert!(!run.line.starts_with("stand-in"), "the stand-in answered, so this proves nothing: {}", run.line);
    assert!(run.line.starts_with("jev · "), "the seed policy's row is trusted = secondary (jev): {}", run.line);
    let _ = std::fs::remove_dir_all(&root);
}
