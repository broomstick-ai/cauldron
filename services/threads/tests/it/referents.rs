//! # Every `Ref` this producer declares resolves to a shape it serves, and the
//! one it leaves unmapped is the resource door
//!
//! `shapewire::hub::Shape::REFERS` is checked against the manifest at
//! `register`; whether the shape NAMED exists is a graph question (`accounts`
//! rows name threads and `threads` rows name accounts — a cycle no registration
//! order can see both ends of), so the chassis answers it with
//! `Hub::dangling_refs` and this producer asserts the answer here, once every
//! shape is registered. This is the positive sentinel for the declarations in
//! `src/shapes/*.rs`: a referent naming a shape this daemon does not serve is
//! red here, in the producer's own suite.
//!
//! Forced red: declare `("thread", "thread")` on `recent` — the message names
//! `threads/recent-updates`, `thread` and `thread`.

use std::collections::BTreeMap;

use threads::build_manager;
use threads::config::Config;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_declared_referent_names_a_shape_this_producer_serves() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "first".to_string(),
        tmp.path().join("first").to_string_lossy().to_string(),
    );
    threads::clock::testing::ticking();
    let manager = build_manager(Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();
    manager.start_watchers();

    let list = manager.hub.list();
    let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"threads") && names.contains(&"accounts"),
        "the seven shapes must be registered for this to test anything: {names:?}"
    );
    assert_eq!(
        manager.hub.core().dangling_refs(),
        Vec::<(&'static str, String, String)>::new(),
        "every declared referent names a shape this producer serves"
    );

    // The declarations, read back off `shapes.list` as a consumer would — the
    // positive sentinel that the walk above saw something: the mutual edge
    // between the pool and the roster is on BOTH shapes.
    let refs = |n: &str| list.iter().find(|s| s.name == n).unwrap().refs.clone();
    assert_eq!(refs("threads").get("thread").map(String::as_str), Some("threads"));
    assert_eq!(refs("threads").get("account").map(String::as_str), Some("accounts"));
    assert_eq!(refs("accounts").get("thread").map(String::as_str), Some("threads"));
    assert_eq!(refs("accounts").get("account").map(String::as_str), Some("accounts"));
    assert_eq!(refs("threads/recent-updates").get("thread").map(String::as_str), Some("threads"));
    assert_eq!(refs("narration").get("account").map(String::as_str), Some("accounts"));
    // And the one deliberately unmapped: a resource id is the resource door's,
    // not a shape's key — absence here is that statement, not an omission.
    assert_eq!(refs("threads").get("resource"), None);
    // Shapes with no `Ref` at all declare nothing and publish nothing.
    assert!(refs("heartbeat").is_empty() && refs("threads/log").is_empty());

    // The paging contract every shape now publishes, and the one producer-set
    // default: `threads/summary` serves its whole (hydrate-capped) answer to a
    // silent consumer, so neither face's keyless read moved.
    let page = |n: &str| list.iter().find(|s| s.name == n).unwrap().page.unwrap();
    assert_eq!(page("threads/summary").default, None, "summary: no pagination by default");
    assert_eq!(page("threads").default, Some(100), "the chassis default elsewhere");
}
