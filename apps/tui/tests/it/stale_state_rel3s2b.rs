//! REL-3.S2b: **an environment whose state points at vanished seed items
//! still opens.**
//!
//! After REL-3.S2 a real environment no longer reads the repo seed, but the
//! state it kept was written against that seed: prod's `tickets.json` named
//! `X-3/HA-4/step` and `X-1/HA-1/step-1-read-the-four-files`, its `refs.json`
//! anchored `[T-1]` on `X-1`, its bucket and old per-cluster draft quoted
//! them, and the shared `repos.json` can unregister a seed repo. At 8fd2e064
//! the face refused to open (`rc 1`, "tickets.json names 2 step(s) ... of no
//! human action in this soup"). Now every such entry is KEPT on disk byte for
//! byte, skipped in the view, and said in ONE quiet notice.
//!
//! Every guard pairs the dangling arm with an arm over the same predicate in
//! which the very same files DO name something (the fixture seed), so neither
//! a constant notice nor a constant silence passes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use tui::cauldron::app::App;
use tui::cauldron::store;

const TWO: &str = "2 tickets refer to items that no longer exist";

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "rel3s2b-{}-{}-{}",
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

fn soup(root: &Path) -> PathBuf {
    let d = store::soup_dir(&env_in(root, "prod")).unwrap();
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn tickets(root: &Path, done: &[&str]) {
    write(&soup(root).join("tickets.json"), &serde_json::to_string_pretty(&serde_json::json!({"done": done, "submitted": []})).unwrap());
}

/// A prod soup shaped like the operator's: NO `clusters.json` and no `seed/`,
/// and UI state written while the repo seed was still read.
fn prod_shaped(root: &Path) {
    let d = soup(root);
    tickets(root, &["X-3/HA-4/step", "X-1/HA-1/step-1-read-the-four-files"]);
    let draft = |text: &str| serde_json::json!({"nonce": "n-1", "writer": "1@demo", "updated_at": "2026-10-30T10:00:00Z", "text": text, "merges": true}).to_string();
    write(&d.join("bucket.json"), &draft("see [T-1] before the release"));
    write(&d.join("drafts/X-1.json"), &draft("an old per-cluster draft quoting [Q-2]"));
    write(
        &d.join("refs.json"),
        &serde_json::json!({"version": 1, "text": "see [T-1] before the release", "refs": [
            {"id": "r1", "tag": "[T-1]", "at": 4, "cluster": "X-1", "ctab": "surface", "tab": "tasks", "aggregate": false}
        ]})
        .to_string(),
    );
    write(&d.join("layout.json"), r#"{"sidebar_permille": 300}"#);
    write(
        &d.join("supervisions.jsonl"),
        &format!("{}\n", serde_json::json!({"at": "2026-10-30 10:00:00", "who": "operator", "device": "demo-device", "service": "ripples", "op": "restart", "why": "requested"})),
    );
    write(
        &d.join("dumps.jsonl"),
        &format!("{}\n", serde_json::json!({"seq": 1, "at": "2026-10-30T10:00:00Z", "env": "prod", "from_cluster": "X-1", "text": "on [T-1]", "tags": ["[T-1]"], "targets": ["X-1"]})),
    );
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

/// The guard the inspection (REL-3.S2-I F1) asked for: a prod-shaped soup
/// holding stale entries opens, says so once, and keeps every entry on disk.
#[test]
fn a_prod_shaped_env_with_tickets_naming_vanished_items_opens_with_one_notice() {
    let root = scratch_root();
    prod_shaped(&root);
    let t = soup(&root).join("tickets.json");
    let before = std::fs::read(&t).unwrap();
    let mut a = open(&root, None, true).expect("REL-3.S2b: a stale tickets.json never stops the face opening");
    assert!(a.forest.clusters.iter().all(|c| c.human_actions.is_empty()), "prod shows its own (empty) data");
    assert!(said(&a).contains(TWO), "one quiet notice, with the count: {:?}", a.notice);
    assert!(a.error.is_none(), "a notice, not an error: {:?}", a.error);
    a.sync();
    assert!(a.error.is_none(), "nor on a re-read: {:?}", a.error);
    assert_eq!(std::fs::read(&t).unwrap(), before, "the dangling entries are kept on disk, byte for byte");
    assert!(a.tickets.file.done.len() == 2, "and in memory: {:?}", a.tickets.file.done);

    // Opposed arm, SAME files: with the seed that holds X-3/HA-4 and X-1/HA-1
    // every entry names something, so nothing is said and the step is done.
    let b = open(&root, Some(store::seed_dir()), false).expect("opens");
    assert!(!said(&b).contains("no longer exist"), "nothing dangles here: {:?}", b.notice);
    let g = b.forest.find("X-3").unwrap();
    let (_, tk) = b.ticket(g, 4).unwrap();
    assert!(b.step_done(g, 4, &tk, 0), "the entry still reads as done where its item exists");
}

/// The count is the count: one stale entry among a valid one says one.
#[test]
fn the_notice_counts_only_the_entries_that_name_nothing() {
    let root = scratch_root();
    tickets(&root, &["X-3/HA-4/step", "X-3/HA-4/no-such-step"]);
    let a = open(&root, Some(store::seed_dir()), false).expect("opens");
    assert!(said(&a).contains("1 ticket refers to an item that no longer exists"), "{:?}", a.notice);
    let g = a.forest.find("X-3").unwrap();
    let (_, tk) = a.ticket(g, 4).unwrap();
    assert!(a.step_done(g, 4, &tk, 0), "the valid entry beside it still reads as done");
    // And an empty file on the same seedless env says nothing.
    let root = scratch_root();
    tickets(&root, &[]);
    let a = open(&root, None, false).expect("opens");
    assert!(!said(&a).contains("no longer exist"), "{:?}", a.notice);
}

/// Another face writing a stale entry while this one runs: no error, the
/// file adopted, the line said.
#[test]
fn a_re_read_that_finds_a_stale_entry_says_it_and_adopts_the_file() {
    let root = scratch_root();
    tickets(&root, &[]);
    let mut a = open(&root, None, true).expect("opens");
    assert!(!said(&a).contains("no longer exist"), "{:?}", a.notice);
    a.notice = None;
    // mtime granularity: make sure the watch sees a different signature.
    std::thread::sleep(std::time::Duration::from_millis(20));
    tickets(&root, &["X-3/HA-4/step", "X-1/HA-1/step-1-read-the-four-files", "X-9/HA-2/x"]);
    a.sync();
    assert!(a.error.is_none(), "{:?}", a.error);
    assert!(said(&a).contains("3 tickets refer to items that no longer exist"), "{:?}", a.notice);
    assert_eq!(a.tickets.file.done.len(), 3, "the file as written is adopted");
}

/// The shared repo registry unregistering a repo no seed registers any more
/// (a seed repo he unregistered while prod still read the seed) is the same
/// failure; it too is kept, skipped and said.
#[test]
fn a_registry_unregistering_a_vanished_seed_repo_opens_with_the_notice() {
    let root = scratch_root();
    let env = env_in(&root, "prod");
    let reg = store::global_dir(&env).join("repos.json");
    let text = r#"{"unregistered":["orchard"]}"#;
    write(&reg, text);
    let a = open(&root, None, true).expect("REL-3.S2b: a stale unregistered repo never stops the face opening");
    assert!(said(&a).contains("1 unregistered repo no longer exists"), "{:?}", a.notice);
    assert_eq!(std::fs::read_to_string(&reg).unwrap(), text, "kept on disk");
    // Opposed arm: with the seed that registers orchard, nothing dangles
    // and orchard is not shown.
    let b = open(&root, Some(store::seed_dir()), false).expect("opens");
    assert!(!said(&b).contains("no longer exist"), "{:?}", b.notice);
    assert!(b.repos.find_repo("orchard").is_none(), "still unregistered where it exists");
    assert!(b.repos.find_repo("harness").is_some(), "the other seed repos stand");
}

/// End to end, the BUILT face on a scratch HOME and CAULDRON_HOME holding a
/// prod-shaped copy: `rc 0` and the notice on the first frame.
#[test]
fn the_built_face_opens_a_prod_shaped_copy_rc_0_with_the_notice() {
    let home = scratch_root();
    let root = home.join(".cauldron");
    prod_shaped(&root);
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
    assert!(out.contains(TWO), "the first frame says it:\n{out}");
    assert!(std::fs::read_to_string(store::soup_dir(&env_in(&root, "prod")).unwrap().join("tickets.json")).unwrap().contains("X-3/HA-4/step"));
}
