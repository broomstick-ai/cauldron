//! The document, the refusals, the ordering, and the read behaviour.
//!
//! `0071` §14 obligations 4, 5, 6, 7, 8 and the §12 witness live here.

mod support;

use std::fs;
use std::path::PathBuf;

use projects::*;
use support::*;

// ──────────────────────────────────────────────────────────────────────────
//  §14.6 — the refusals, each with the file's bytes as the witness
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn register_refuses_a_name_already_taken_and_writes_nothing() {
    let home = scratch_home();
    let h = home.path();
    fs::create_dir_all(h.join("code/harness")).unwrap();
    fs::create_dir_all(h.join("other/harness")).unwrap();

    Registry::update(h, |r| r.register(&h.join("code/harness"), None).map(|_| ())).unwrap();
    let before = registry_bytes(h);

    let err = Registry::update(h, |r| {
        r.register(&h.join("other/harness"), None).map(|_| ())
    })
    .unwrap_err();

    assert_eq!(err.sentence(), "`harness` is taken — name it something else.");
    // POSITIVE WITNESS: the document is byte-identical, not merely "no error".
    assert_eq!(registry_bytes(h), before, "a refusal must write nothing");
}

#[test]
fn register_refuses_the_same_checkout_under_a_second_spelling() {
    let home = scratch_home();
    let h = home.path();
    let real = h.join("code/harness");
    fs::create_dir_all(&real).unwrap();
    // A second SPELLING of the same checkout: a symlink to it.
    std::os::unix::fs::symlink(&real, h.join("harness-link")).unwrap();

    Registry::update(h, |r| r.register(&real, None).map(|_| ())).unwrap();
    let before = registry_bytes(h);

    let err = Registry::update(h, |r| {
        r.register(&h.join("harness-link"), Some("second")).map(|_| ())
    })
    .unwrap_err();

    assert!(
        err.sentence().ends_with("is already registered as `harness`."),
        "got: {}",
        err.sentence()
    );
    assert_eq!(registry_bytes(h), before);
}

#[test]
fn register_refuses_every_spelling_of_the_bucket() {
    let home = scratch_home();
    let h = home.path();
    for spelling in ["miscellaneous", "_miscellaneous", "Miscellaneous", "MISCELLANEOUS"] {
        let dir = h.join("code").join(format!("d-{spelling}"));
        fs::create_dir_all(&dir).unwrap();
        let err = Registry::update(h, |r| r.register(&dir, Some(spelling)).map(|_| ())).unwrap_err();
        assert!(
            err.sentence().contains("is reserved"),
            "{spelling} was not refused: {}",
            err.sentence()
        );
    }
    // Nothing was ever created, because every call refused.
    assert!(Registry::load(h).unwrap().is_none());
}

#[test]
fn register_refuses_a_missing_directory_and_a_file() {
    let home = scratch_home();
    let h = home.path();
    let mut reg = Registry::empty();

    let missing = h.join("not-there");
    let err = reg.register(&missing, None).unwrap_err();
    assert_eq!(err.sentence(), format!("There is nothing at `{}`.", missing.display()));

    let file = h.join("a-file");
    fs::write(&file, b"x").unwrap();
    let err = reg.register(&file, None).unwrap_err();
    assert!(err.sentence().contains("is not a directory"), "{}", err);

    assert_eq!(reg.len(), 0);
}

#[test]
fn register_refuses_a_name_that_could_escape_its_directory() {
    let home = scratch_home();
    let h = home.path();
    let dir = h.join("code/p");
    fs::create_dir_all(&dir).unwrap();
    let mut reg = Registry::empty();

    for bad in ["../escape", "a/b", "..", ".hidden", "  "] {
        let err = reg.register(&dir, Some(bad)).unwrap_err();
        assert!(
            err.sentence().contains("not a name I can use"),
            "{bad} was accepted: {err}"
        );
    }
    assert_eq!(reg.len(), 0);
}

#[test]
fn remove_refuses_the_bucket_and_an_unknown_name() {
    let mut reg = Registry::in_memory(vec![("a", PathBuf::from("/a"))]);
    assert!(reg.remove(BUCKET).unwrap_err().sentence().contains("is the bucket"));
    assert_eq!(
        reg.remove("nope").unwrap_err().sentence(),
        "I don't have a project called `nope`."
    );
    assert_eq!(reg.len(), 1);
}

// ──────────────────────────────────────────────────────────────────────────
//  §14.8 — de-register touches the entry and NOTHING else
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn de_register_leaves_the_directory_its_mind_and_its_threads_alone() {
    let home = scratch_home();
    let h = home.path();
    let proj = h.join("code/keeper");
    fs::create_dir_all(proj.join(".mind")).unwrap();
    fs::write(proj.join(".mind/threads.json"), br#"{"threads":[]}"#).unwrap();
    fs::write(proj.join("README.md"), b"his work").unwrap();

    Registry::update(h, |r| r.register(&proj, None).map(|_| ())).unwrap();
    let before = tree(&proj);
    assert!(!before.is_empty());

    let gone = Registry::update(h, |r| r.remove("keeper")).unwrap();
    assert_eq!(gone.name, "keeper");

    // WITNESS: the whole subtree is byte-for-byte what it was.
    assert_eq!(tree(&proj), before, "de-register must not touch the directory");
    assert!(Registry::load(h).unwrap().unwrap().get("keeper").is_none());
}

// ──────────────────────────────────────────────────────────────────────────
//  §14.7 — the move, and the defect it refuses to reproduce
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn move_before_places_by_neighbour_name_and_round_trips_through_the_file() {
    let home = scratch_home();
    let h = home.path();
    for n in ["a", "b", "c", "d"] {
        let d = h.join("code").join(n);
        fs::create_dir_all(&d).unwrap();
        Registry::update(h, |r| r.register(&d, None).map(|_| ())).unwrap();
    }
    assert_eq!(
        Registry::load(h).unwrap().unwrap().names().collect::<Vec<_>>(),
        vec!["a", "b", "c", "d"]
    );

    Registry::update(h, |r| r.move_before("d", Some("b"))).unwrap();
    assert_eq!(
        Registry::load(h).unwrap().unwrap().names().collect::<Vec<_>>(),
        vec!["a", "d", "b", "c"]
    );

    // `None` means last.
    Registry::update(h, |r| r.move_before("a", None)).unwrap();
    assert_eq!(
        Registry::load(h).unwrap().unwrap().names().collect::<Vec<_>>(),
        vec!["d", "b", "c", "a"]
    );
}

/// **THE DEFECT WE WERE TOLD ABOUT AND MUST NOT REPRODUCE.**
///
/// `move_pin` (`apps/aui/src/v2/mutate.rs:711`) resolves a stale `before` with
/// `unwrap_or(next.len())` — the row silently goes to the END and the move
/// looks like it worked. `MoveProject` refuses instead, and the ORDER IS
/// UNCHANGED, which is the half an "it returned an error" assertion would miss.
#[test]
fn move_before_refuses_a_stale_neighbour_instead_of_sending_the_row_to_the_end() {
    let mut reg = Registry::in_memory(vec![
        ("a", PathBuf::from("/a")),
        ("b", PathBuf::from("/b")),
        ("c", PathBuf::from("/c")),
    ]);

    let err = reg.move_before("a", Some("vanished")).unwrap_err();
    assert_eq!(
        err.sentence(),
        "`vanished` is not in the list any more — the list changed under you; try again."
    );
    // POSITIVE WITNESS: `a` is still FIRST. Under `move_pin`'s rule it would be last.
    assert_eq!(reg.names().collect::<Vec<_>>(), vec!["a", "b", "c"]);
}

#[test]
fn move_before_refuses_an_unknown_project_and_a_self_move() {
    let mut reg = Registry::in_memory(vec![("a", PathBuf::from("/a")), ("b", PathBuf::from("/b"))]);
    assert_eq!(
        reg.move_before("zz", Some("a")).unwrap_err().sentence(),
        "I don't have a project called `zz`."
    );
    assert_eq!(
        reg.move_before("a", Some("a")).unwrap_err().sentence(),
        "`a` is already where it is."
    );
    assert_eq!(reg.names().collect::<Vec<_>>(), vec!["a", "b"]);
}

// ──────────────────────────────────────────────────────────────────────────
//  §14.5 — a corrupt registry is NOT fail-open, and refusing does not write
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn an_unparseable_registry_is_an_error_and_a_write_against_it_is_refused() {
    let home = scratch_home();
    let h = home.path();
    fs::create_dir_all(cauldron_dir(h)).unwrap();
    fs::write(registry_path(h), b"{ this is not json").unwrap();
    let before = registry_bytes(h);

    let err = Registry::load(h).unwrap_err();
    assert!(err.sentence().contains("not valid JSON"), "{err}");

    let dir = h.join("code/p");
    fs::create_dir_all(&dir).unwrap();
    let err = Registry::update(h, |r| r.register(&dir, None).map(|_| ())).unwrap_err();
    assert!(matches!(err, UpdateError::Load(_)), "{err:?}");

    // WITNESS: the bad bytes are untouched. A fail-open writer would have
    // replaced them with a one-entry document.
    assert_eq!(registry_bytes(h), before);
}

#[test]
fn a_version_this_crate_does_not_know_is_refused_in_a_sentence() {
    let home = scratch_home();
    let h = home.path();
    fs::create_dir_all(cauldron_dir(h)).unwrap();
    fs::write(registry_path(h), br#"{"version":7,"projects":[]}"#).unwrap();
    let err = Registry::load(h).unwrap_err();
    assert_eq!(
        err.sentence(),
        "Your project registry is version 7 and I only know version 1."
    );
}

#[test]
fn a_duplicate_name_a_duplicate_path_and_a_registered_bucket_are_all_refused() {
    let dup_name = r#"{"version":1,"projects":[
      {"name":"a","path":"/x","registered_at":"2026-09-21T03:12:44Z"},
      {"name":"a","path":"/y","registered_at":"2026-09-21T03:12:44Z"}]}"#;
    assert!(matches!(
        Registry::from_json(dup_name),
        Err(LoadError::DuplicateName(_))
    ));

    let dup_path = r#"{"version":1,"projects":[
      {"name":"a","path":"/x","registered_at":"2026-09-21T03:12:44Z"},
      {"name":"b","path":"/x","registered_at":"2026-09-21T03:12:44Z"}]}"#;
    assert!(matches!(
        Registry::from_json(dup_path),
        Err(LoadError::DuplicatePath(_))
    ));

    let bucket = r#"{"version":1,"projects":[
      {"name":"miscellaneous","path":"/x","registered_at":"2026-09-21T03:12:44Z"}]}"#;
    assert!(matches!(
        Registry::from_json(bucket),
        Err(LoadError::BucketRegistered)
    ));
}

// ──────────────────────────────────────────────────────────────────────────
//  The bucket, resolve, and the §12 witness
// ──────────────────────────────────────────────────────────────────────────

/// **§14.11's witness, at the crate's half of the boundary.** Three registered
/// projects: `names()` counts THREE and `view()` draws FOUR rows. An assertion
/// on either number alone would pass under the other's bug.
#[test]
fn the_bucket_is_synthesized_last_and_is_not_counted_among_the_projects() {
    let home = scratch_home();
    let h = home.path();
    let reg = Registry::in_memory(vec![
        ("a", h.join("code/a")),
        ("b", h.join("code/b")),
        ("c", h.join("code/c")),
    ]);

    assert_eq!(reg.names().count(), 3, "the bucket is not a project");

    let rows = reg.view(h);
    assert_eq!(rows.len(), 4, "the bucket is always drawn");
    assert_eq!(
        rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        vec!["a", "b", "c", "miscellaneous"]
    );
    assert!(rows.last().unwrap().bucket, "the bucket flag comes from the crate");
    assert!(!rows[..3].iter().any(|r| r.bucket));
    assert_eq!(rows.last().unwrap().path, bucket_root(h));
}

#[test]
fn an_empty_registry_still_has_the_bucket_and_no_write_can_remove_it() {
    let home = scratch_home();
    let h = home.path();
    let rows = Registry::empty().view(h);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].bucket);
    assert_eq!(rows[0].name, BUCKET);
}

#[test]
fn resolve_answers_the_bucket_a_registered_name_and_nothing_else() {
    let home = scratch_home();
    let h = home.path();
    let reg = Registry::in_memory(vec![("harness", h.join("code/harness"))]);

    assert_eq!(reg.resolve(h, BUCKET), Some(bucket_root(h)));
    assert_eq!(reg.resolve(h, "harness"), Some(h.join("code/harness")));
    // THE POINT OF THE OPTION: an unknown name has no honest path, so it has
    // none at all. A fallback here is a silent `mkdir -p` somewhere new.
    assert_eq!(reg.resolve(h, "never-registered"), None);
    assert_eq!(reg.resolve(h, "_miscellaneous"), None);
}

#[test]
fn a_registered_path_that_stops_existing_keeps_its_row_and_is_drawn_missing() {
    let home = scratch_home();
    let h = home.path();
    let proj = h.join("code/moved");
    fs::create_dir_all(&proj).unwrap();
    Registry::update(h, |r| r.register(&proj, None).map(|_| ())).unwrap();
    fs::remove_dir_all(&proj).unwrap();

    let reg = Registry::load(h).unwrap().unwrap();
    assert_eq!(reg.len(), 1, "his list is not silently shortened");
    let rows = reg.view(h);
    assert!(rows[0].missing, "the row says the directory is gone");
    assert!(!rows.last().unwrap().missing, "the bucket is never missing");
}

// ──────────────────────────────────────────────────────────────────────────
//  The file itself
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn the_document_round_trips_and_is_pretty_one_entry_per_line() {
    let home = scratch_home();
    let h = home.path();
    make_minds(h, &[("airbnb", "code/airbnb"), ("plan", "plan")]);
    let reg = Registry::derive_from_minds(h, "test");
    let text = reg.to_json();

    assert!(text.contains("\"version\": 1"));
    assert!(text.contains("\"bootstrap\""));
    assert!(text.ends_with("}\n"));
    assert_eq!(Registry::from_json(&text).unwrap(), reg);
}

#[test]
fn load_of_an_absent_registry_is_none_not_an_error() {
    let home = scratch_home();
    assert!(Registry::load(home.path()).unwrap().is_none());
}

#[test]
fn load_or_derive_says_which_source_it_used() {
    let home = scratch_home();
    let h = home.path();
    make_minds(h, &[("a", "code/a")]);

    let (reg, src) = Registry::load_or_derive(h).unwrap();
    assert_eq!(src, Source::DerivedFromMinds);
    assert_eq!(reg.names().collect::<Vec<_>>(), vec!["a"]);

    Registry::bootstrap_if_absent(h, "test").unwrap();
    let (reg, src) = Registry::load_or_derive(h).unwrap();
    assert_eq!(src, Source::File);
    assert_eq!(reg.names().collect::<Vec<_>>(), vec!["a"]);
}

// ──────────────────────────────────────────────────────────────────────────
//  The snapshot
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn the_snapshot_sees_a_registration_made_behind_its_back_without_a_restart() {
    let home = scratch_home();
    let h = home.path();
    let snap = Snapshot::new(h);

    assert_eq!(snap.get().len(), 0);
    assert_eq!(snap.status(), SnapshotStatus::Absent);

    let a = h.join("code/a");
    fs::create_dir_all(&a).unwrap();
    Registry::update(h, |r| r.register(&a, None).map(|_| ())).unwrap();

    // No restart, no invalidation call: the next `get` stats and re-parses.
    assert_eq!(snap.get().names().collect::<Vec<_>>(), vec!["a"]);
    assert_eq!(snap.status(), SnapshotStatus::Loaded);

    let b = h.join("code/b");
    fs::create_dir_all(&b).unwrap();
    Registry::update(h, |r| r.register(&b, None).map(|_| ())).unwrap();
    assert_eq!(snap.get().names().collect::<Vec<_>>(), vec!["a", "b"]);
}

#[test]
fn an_unreadable_file_makes_the_snapshot_say_so_rather_than_say_empty() {
    let home = scratch_home();
    let h = home.path();
    let a = h.join("code/a");
    fs::create_dir_all(&a).unwrap();
    Registry::update(h, |r| r.register(&a, None).map(|_| ())).unwrap();
    let snap = Snapshot::new(h);
    assert_eq!(snap.get().len(), 1);

    fs::write(registry_path(h), b"broken").unwrap();
    let _ = snap.get();
    match snap.status() {
        SnapshotStatus::Unreadable(s) => assert!(s.contains("not valid JSON"), "{s}"),
        other => panic!("an unreadable registry must not look like an empty one: {other:?}"),
    }
    assert!(snap.get_checked().is_err());
}

#[test]
fn an_in_memory_snapshot_never_touches_a_disk() {
    let snap = Snapshot::in_memory(Registry::in_memory(vec![("p", PathBuf::from("/p"))]));
    assert_eq!(snap.get().names().collect::<Vec<_>>(), vec!["p"]);
    assert_eq!(snap.status(), SnapshotStatus::Loaded);
}
