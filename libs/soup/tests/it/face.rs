//! The installed seed, read by THE FACE'S OWN LOADERS (`apps/tui`), the read
//! path every cauldron's data takes: no copy of the face's schema is trusted.
//!
//! `seed/ledger.json` too, since REL-3.S2: the face reads an environment's
//! own ledger seed first and knows `type_three` and a fold's `thread`.

use soup_seed::demo::{install, ENV};
use tui::cauldron::{artifacts, ledger, store, tickets};

struct Tmp(std::path::PathBuf);

impl Tmp {
    fn new(tag: &str) -> Tmp {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let p = std::env::temp_dir().join(format!("soup-face-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(p.join("seeds")).unwrap();
        Tmp(p)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_face_loads_the_curated_clusters_and_rolls_up_the_aggregate() {
    let t = Tmp::new("clusters");
    let soup_dir = t.0.join("tui").join("soup");
    install(&soup_dir).unwrap();
    // An EMPTY repo seed dir: whatever the face shows comes from the own seed.
    let soup = store::load_soup(&t.0.join("seeds"), ENV, &soup_dir).expect("the face loads the curated soup");
    let f = &soup.forest;
    assert_eq!(f.clusters.len(), 9, "{:?}", f.clusters.iter().map(|c| &c.key).collect::<Vec<_>>());
    let release = f.find("X-7").expect("X-7");
    let own: Vec<u32> = f.questions(release, false).iter().map(|(_, q)| q.serial).collect();
    let all: Vec<u32> = f.questions(release, true).iter().map(|(_, q)| q.serial).collect();
    assert_eq!(own, vec![7]);
    assert!(all.contains(&6) && all.contains(&7), "aggregate X-7 shows its children's questions: {all:?}");
    let actions: usize = f.subtree(release).iter().map(|&g| f.clusters[g].human_actions.len()).sum();
    assert!(actions >= 2, "aggregate X-7 holds its children's actions: {actions}");
}

#[test]
fn the_face_reads_every_artifact_as_present() {
    let t = Tmp::new("arts");
    let soup_dir = t.0.join("tui").join("soup");
    install(&soup_dir).unwrap();
    let seed = artifacts::load(&t.0.join("seeds"), ENV, &soup_dir).expect("the face loads the artifacts");
    assert!(!seed.artifacts.is_empty());
    for a in &seed.artifacts {
        assert_eq!(artifacts::art_state(a, false), artifacts::ArtState::Present, "{} at {}", a.id, a.location);
    }
}

#[test]
fn the_face_parses_the_secret_action_as_four_steps_with_masked_fields() {
    let ds = soup_seed::demo::Dataset::embedded().unwrap();
    let body = ds.clusters["clusters"][4]["children"][1]["human_actions"][0]["body"].as_str().unwrap();
    let t = tickets::parse(body);
    assert_eq!(t.steps.len(), 4, "{:?}", t.steps.iter().map(|s| &s.title).collect::<Vec<_>>());
    let fields: Vec<_> = t.steps.iter().flat_map(|s| s.fields()).collect();
    assert_eq!(fields.len(), 2);
    assert!(fields.iter().all(|f| f.secret), "every field of the secret action is masked");
}

#[test]
fn the_face_reads_the_curated_ledger_from_the_own_seed() {
    let t = Tmp::new("ledger");
    let soup_dir = t.0.join("tui").join("soup");
    // Before the install: nothing (the empty repo seed dir serves nothing).
    assert!(ledger::Seed::load(Some(&t.0.join("seeds")), ENV, &soup_dir).unwrap().dumps.is_empty());
    install(&soup_dir).unwrap();
    let seed = ledger::Seed::load(Some(&t.0.join("seeds")), ENV, &soup_dir).expect("the face parses the curated ledger");
    assert_eq!(seed.dumps.len(), 6);
    assert!(seed.dumps.iter().any(|d| !d.thread.is_empty()), "a thread fold");
}
