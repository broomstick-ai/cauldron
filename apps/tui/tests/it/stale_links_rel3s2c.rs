//! REL-3.S2c: **a cluster linked to a repo that no longer exists still
//! opens.**
//!
//! A cluster's repo claims (a cluster made in this environment records the
//! repos its seeding claimed in `clusters.json`), a claim's focus and a
//! task's or milestone's edge can name a repo or blueprint node that only the
//! old repo seed provided. Before this step `Repos::build_with` refused, and
//! the face did not open. Now each such link is KEPT in its file, skipped in
//! the view, and counted in the same one-line notice REL-3.S2b added.
//!
//! Every guard pairs the vanished arm with an arm over the same files in
//! which the link resolves (the fixture seed registers the repo), so neither
//! a constant notice nor a constant silence nor a constant skip passes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use tui::cauldron::app::App;
use tui::cauldron::blueprint::{BlueprintFile, Repos};
use tui::cauldron::model::{DataFile, Soup};
use tui::cauldron::store;

const ONE: &str = "1 cluster link to a repo no longer exists";

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "rel3s2c-{}-{}-{}",
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

/// A prod soup with NO seed and one cluster made here whose seeding claimed
/// `orchard`, a repo only the fixture seed registers. Returns the file.
fn made_here_claiming(root: &Path, repos: &[&str]) -> PathBuf {
    let d = store::soup_dir(&env_in(root, "prod")).unwrap();
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("clusters.json");
    let text = serde_json::to_string_pretty(&serde_json::json!({"clusters": [
        {"id": "X-90", "name": "Made here", "created_at": "2026-10-30T10:00:00Z", "repos": repos}
    ]}))
    .unwrap();
    std::fs::write(&p, text).unwrap();
    p
}

fn open(root: &Path, seeds: Option<PathBuf>, persist: bool) -> anyhow::Result<App> {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let env = env_in(root, "prod");
    let seeds = seeds.unwrap_or_else(|| store::no_seeds(&env));
    App::open(env, seeds, persist)
}

fn said(a: &App) -> String {
    a.notice.clone().unwrap_or_default()
}

/// The guard for REL-3.S2b-1: a cluster claiming a repo no seed registers
/// opens, says so once, keeps the claim on disk, and shows no such repo.
#[test]
fn a_cluster_claiming_a_vanished_repo_opens_with_the_notice_and_keeps_the_claim() {
    let root = scratch_root();
    let f = made_here_claiming(&root, &["orchard"]);
    let before = std::fs::read(&f).unwrap();
    let mut a = open(&root, None, true).expect("REL-3.S2c: a claim of a vanished repo never stops the face opening");
    let g = a.forest.find("X-90").expect("the cluster is there");
    assert!(said(&a).contains(ONE), "one quiet notice, with the count: {:?}", a.notice);
    assert!(a.error.is_none(), "a notice, not an error: {:?}", a.error);
    assert!(a.repos.claims[g].is_empty(), "skipped in the view: {:?}", a.repos.claims[g]);
    assert!(a.forest.clusters[g].repos.iter().any(|c| c.name == "orchard"), "the cluster still holds the claim in memory");
    a.sync();
    assert!(a.error.is_none(), "nor on a re-read: {:?}", a.error);
    assert_eq!(std::fs::read(&f).unwrap(), before, "the claim is kept on disk, byte for byte");

    // Opposed arm, SAME file: with the seed that registers orchard the claim
    // resolves, nothing is said, and the cluster shows the repo.
    let b = open(&root, Some(store::seed_dir()), false).expect("opens");
    let g = b.forest.find("X-90").expect("the cluster is there");
    assert!(!said(&b).contains("cluster link"), "nothing dangles here: {:?}", b.notice);
    let orchard = b.repos.find_repo("orchard").expect("the seed registers orchard");
    assert_eq!(b.repos.claims[g].iter().map(|c| c.repo).collect::<Vec<_>>(), vec![orchard], "shown where it exists");
    assert!(b.repos.stale_links.is_empty(), "{:?}", b.repos.stale_links);
}

/// The count is the count, and the notice extends the REL-3.S2b line rather
/// than replacing it.
#[test]
fn the_notice_counts_only_the_links_that_name_nothing_beside_the_other_stale_state() {
    let root = scratch_root();
    made_here_claiming(&root, &["orchard", "ghost-one", "ghost-two"]);
    let a = open(&root, Some(store::seed_dir()), false).expect("opens");
    assert!(said(&a).contains("2 cluster links to repos no longer exist"), "{:?}", a.notice);
    let g = a.forest.find("X-90").unwrap();
    assert_eq!(a.repos.claims[g].len(), 1, "the valid claim beside them still shows");
    // The same notice carries the tickets part too, joined, not overwritten.
    let root = scratch_root();
    made_here_claiming(&root, &["orchard"]);
    let d = store::soup_dir(&env_in(&root, "prod")).unwrap();
    std::fs::write(d.join("tickets.json"), r#"{"done":["X-9/HA-9/x"],"submitted":[]}"#).unwrap();
    let a = open(&root, None, false).expect("opens");
    assert_eq!(said(&a), format!("1 ticket refers to an item that no longer exists · {ONE}"), "one line, both counts");
}

/// A claim's focus naming a node its repo does not hold: skipped, counted;
/// the focus it does hold still shows. Opposed by the same claim with only
/// the node that exists.
#[test]
fn a_focus_on_a_vanished_node_is_skipped_and_counted() {
    let bp = || -> BlueprintFile {
        serde_json::from_str(
            r#"{"repos":[{"name":"r","path":"/r","branch":"main","head":"0","blueprint":{"schema_version":1,
                "nodes":[{"id":"MOD-a-00000000","type":"module","state":"draft","handle":"h"}]}}]}"#,
        )
        .unwrap()
    };
    let soup = |focus: &str| Soup::from_file(serde_json::from_str::<DataFile>(&format!(r#"{{"clusters":[{{"id":"X-1","name":"A","description":"","repos":[{{"name":"r","focus":[{focus}]}}]}}]}}"#)).unwrap()).unwrap();
    let s = soup(r#""MOD-a-00000000","MOD-gone-11111111""#);
    let r = Repos::build(bp(), &s.forest).expect("REL-3.S2c: a focus on a vanished node is skipped, not refused");
    assert_eq!(r.stale_links, vec!["cluster `X-1` focuses `MOD-gone-11111111`, which is not in repo `r`".to_string()]);
    assert_eq!(r.claims[0].len(), 1, "the claim itself stands");
    assert_eq!(r.claims[0][0].focus, vec![0], "with the focus that exists");
    let s = soup(r#""MOD-a-00000000""#);
    let r = Repos::build(bp(), &s.forest).expect("opens");
    assert!(r.stale_links.is_empty(), "{:?}", r.stale_links);
    assert_eq!(r.claims[0][0].focus, vec![0]);
}

/// End to end, the BUILT face on a scratch HOME and CAULDRON_HOME whose prod
/// holds a cluster claiming a vanished repo: `rc 0` and the notice.
#[test]
fn the_built_face_opens_a_cluster_claiming_a_vanished_repo_rc_0_with_the_notice() {
    let home = scratch_root();
    let root = home.join(".cauldron");
    let f = made_here_claiming(&root, &["orchard"]);
    // MERGE-9b: through the isolation helper (its defaults live in a
    // sibling scratch dir); HOME and CAULDRON_HOME are still this test's own.
    let o = crate::face::Face::new(&home.with_extension("face"))
        .args(["--cauldron", "--dump", "--env", "prod"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("CAULDRON_HOME", &root)
        .env("CAULDRON_TYPE_ONE", "off")
        .env("CAULDRON_OPENER", "none")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "rc {:?}\n{}", o.status.code(), String::from_utf8_lossy(&o.stderr));
    assert!(out.contains(ONE), "the first frame says it:\n{out}");
    assert!(std::fs::read_to_string(&f).unwrap().contains("orchard"), "kept on disk");
}
