//! Task 0203 (8e4fca10 msg 235's rename, at the source): what the face wrote
//! by the OLD ids is never read as nothing. A human action's steps kept in
//! `tickets.json` are keyed by the ids the soup has now (`X-3/HA-4/step`);
//! an entry naming no step or field of any human action here — an id from
//! before msg 235 above all — is never read as nothing in silence. Since
//! REL-3.S2b it no longer refuses the file (prod's own file named seed items
//! REL-3.S2 took away, and the face would not open): the entry is KEPT on
//! disk, skipped, and SAID, on open and on a re-read. And a cauldron's
//! own seed written before msg 235 (`seed/surfaces.json`) is refused, not
//! read in place of the `clusters.json` its writer writes now.

use crate::own_seed_fixture;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use tui::cauldron::app::App;
use tui::cauldron::store::{seed_dir, soup_dir};

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "rename0203-{}-{}-{}",
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

fn open(root: &Path, env: &str) -> anyhow::Result<App> {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(root, env), seed_dir(), true)
}

/// Write prod's `tickets.json` in `root`.
fn tickets(root: &Path, json: serde_json::Value) {
    let dir = soup_dir(&env_in(root, "prod")).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tickets.json"), serde_json::to_string_pretty(&json).unwrap()).unwrap();
}

/// HA-4 on X-3 (routing's yes/no ask) is one implicit step, `step`.
fn routing_step_done(a: &App) -> bool {
    let g = a.forest.find("X-3").unwrap();
    let (_, t) = a.ticket(g, 4).unwrap();
    a.step_done(g, 4, &t, 0)
}

#[test]
fn a_step_kept_by_its_id_now_reads_as_done() {
    let root = scratch_root();
    tickets(&root, serde_json::json!({"done": ["X-3/HA-4/step"], "submitted": ["X-8/HA-5/SUPABASE_ANON_KEY"]}));
    let a = open(&root, "prod").expect("every entry names a step or field here");
    assert!(routing_step_done(&a));
    assert!(a.tickets.is_submitted("X-8", 5, "SUPABASE_ANON_KEY"));
}

#[test]
fn an_entry_by_an_id_from_before_msg_235_is_kept_and_said_not_refused() {
    let root = scratch_root();
    tickets(&root, serde_json::json!({"done": ["cauldron-ripples/HA-1/step"], "submitted": []}));
    let a = open(&root, "prod").expect("REL-3.S2b: kept and said, never a reason not to open");
    let n = a.notice.clone().unwrap_or_default();
    assert!(n.contains("1 ticket refers to an item that no longer exists"), "{n}");
    assert_eq!(a.tickets.file.done, ["cauldron-ripples/HA-1/step"], "kept, not dropped");
    assert!(!routing_step_done(&a), "and read as nothing it does not name");
}

#[test]
fn an_entry_naming_no_step_or_no_field_of_a_real_human_action_is_said() {
    for (done, submitted, bad) in [
        (vec!["X-3/HA-4/no-such-step"], vec![], "X-3/HA-4/no-such-step"),
        (vec![], vec!["X-8/HA-5/NO_SUCH_FIELD"], "X-8/HA-5/NO_SUCH_FIELD"),
        (vec!["X-3/HA-9/step"], vec![], "X-3/HA-9/step"),
        // A field name is not a step, nor a step a field.
        (vec![], vec!["X-3/HA-4/step"], "X-3/HA-4/step"),
    ] {
        let root = scratch_root();
        tickets(&root, serde_json::json!({"done": done, "submitted": submitted}));
        let a = open(&root, "prod").unwrap_or_else(|e| panic!("{bad}: opens, {e:#}"));
        assert_eq!(a.tickets.unknown(&a.forest), [bad], "{bad}");
        let n = a.notice.clone().unwrap_or_default();
        assert!(n.contains("1 ticket refers to an item that no longer exists"), "{bad}: {n}");
    }
}

#[test]
fn a_re_read_that_finds_an_old_id_is_said_and_never_an_error() {
    let root = scratch_root();
    tickets(&root, serde_json::json!({"done": ["X-3/HA-4/step"], "submitted": []}));
    let mut a = open(&root, "prod").unwrap();
    assert!(routing_step_done(&a));
    assert!(!a.notice.clone().unwrap_or_default().contains("no longer exist"), "{:?}", a.notice);
    a.notice = None;
    // Another face (an old build) writes the old id back.
    tickets(&root, serde_json::json!({"done": ["X-3/HA-4/step", "cauldron-ripples/HA-1/step"], "submitted": []}));
    a.sync();
    assert!(a.error.is_none(), "{:?}", a.error);
    let n = a.notice.clone().expect("said, not swallowed");
    assert!(n.contains("1 ticket refers to an item that no longer exists"), "{n}");
    assert!(routing_step_done(&a), "what names something still reads as done");
}

#[test]
fn an_own_seed_written_before_msg_235_is_refused_not_read() {
    let root = scratch_root();
    own_seed_fixture::write_legacy(&root, "design");
    let e = format!("{:#}", open(&root, "design").err().expect("refused"));
    assert!(e.contains("surfaces.json") && e.contains("clusters.json"), "{e}");
}
