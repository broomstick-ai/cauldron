//! The bootstrap: idempotence, the halfway crash, the stray sweep, and the
//! derivation's fidelity to today's list. `0071` §14 obligations 1, 2, 9, 10.
//!
//! The N >= 2 half of the bootstrap proof is NOT here — a single-process test
//! of a race is not a test of the race. It is in `concurrency.rs`, across real
//! processes.

mod support;

use std::fs;

use projects::*;
use support::*;

// ──────────────────────────────────────────────────────────────────────────
//  §14.1 — idempotence, proved by making the second run UNABLE to derive
// ──────────────────────────────────────────────────────────────────────────

/// The claim is "a second run is a stat and a return". The way to stop that
/// being a claim is to make the derivation IMPOSSIBLE between the two runs: if
/// the second run touched `~/minds` at all it would come back with an empty
/// list and overwrite his 32 projects with nothing.
///
/// So: bootstrap, then `chmod 000` the minds root, then bootstrap again. The
/// second run must return `AlreadyPresent` and the bytes must be identical.
#[test]
fn the_second_bootstrap_is_a_stat_and_a_return_even_when_minds_cannot_be_read() {
    let home = scratch_home();
    let h = home.path();
    make_minds(h, &[("airbnb", "code/airbnb"), ("harness", "code/harness")]);

    let first = Registry::bootstrap_if_absent(h, "test").unwrap();
    assert!(first.was_created());
    let bytes = registry_bytes(h);

    // The derivation's input is now unreadable. Anything that reads it loses.
    let minds = minds_root(h);
    let mode = fs::metadata(&minds).unwrap().permissions();
    fs::set_permissions(&minds, <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o000)).unwrap();

    let second = Registry::bootstrap_if_absent(h, "test").unwrap();

    fs::set_permissions(&minds, mode).unwrap();

    // **THE WITNESS THAT `~/minds` WAS NEVER READ.** `!was_created()` is NOT
    // enough, and the forced-failure campaign proved it: with the `stat`
    // short-circuit deleted, the second run derives an EMPTY list from the
    // unreadable directory, writes its temp, loses the `hard_link`, and comes
    // back with the winner's document — byte-identical, not created, and it
    // read the relic anyway. Only the VARIANT tells the two apart:
    // `AlreadyPresent` is the stat-and-return; `LostRace` means it derived.
    assert!(
        matches!(second, Bootstrapped::AlreadyPresent(_)),
        "the second run derived instead of returning on the stat: {second:?}"
    );
    assert!(!second.was_created(), "the second run must not write");
    assert!(strays(h).is_empty(), "the second run wrote a temp");
    assert_eq!(
        second.registry().names().collect::<Vec<_>>(),
        vec!["airbnb", "harness"],
        "the second run returned the file's contents, not a fresh derivation"
    );
    assert_eq!(registry_bytes(h), bytes, "the document is byte-identical");
}

#[test]
fn a_third_and_fourth_run_change_nothing_either() {
    let home = scratch_home();
    let h = home.path();
    make_minds(h, &[("a", "code/a")]);
    Registry::bootstrap_if_absent(h, "test").unwrap();
    let bytes = registry_bytes(h);
    for _ in 0..3 {
        assert!(!Registry::bootstrap_if_absent(h, "test").unwrap().was_created());
    }
    assert_eq!(registry_bytes(h), bytes);
    assert!(strays(h).is_empty(), "no run left a temp behind");
}

// ──────────────────────────────────────────────────────────────────────────
//  §14.2 — the halfway crash
// ──────────────────────────────────────────────────────────────────────────

/// A crash between the temp write and the link leaves a temp and NO registry.
/// The state to prove is that this is indistinguishable from "never ran": the
/// next run produces a correct registry and the stray is gone.
#[test]
fn a_crash_between_the_temp_and_the_link_leaves_no_registry_and_the_next_run_is_correct() {
    let home = scratch_home();
    let h = home.path();
    make_minds(h, &[("a", "code/a"), ("b", "code/b")]);

    // Reproduce the crashed run's exact residue: a fully written temp, aged
    // past the sweep threshold, and no `projects.json`.
    let dir = cauldron_dir(h);
    fs::create_dir_all(&dir).unwrap();
    let stray = dir.join("projects.json.tmp-99999-1");
    fs::write(&stray, b"{\"version\":1,\"projects\":[{\"name\":\"GHOST\",\"path\":\"/ghost\",\"registered_at\":\"2020-01-01T00:00:00Z\"}]}\n").unwrap();
    age_file(&stray, 600);

    // A stray is NOT a registry.
    assert!(Registry::load(h).unwrap().is_none(), "a temp is never read");

    let out = Registry::bootstrap_if_absent(h, "test").unwrap();
    assert!(out.was_created());

    let reg = Registry::load(h).unwrap().unwrap();
    // POSITIVE WITNESS: the real names are there AND the ghost is not. An
    // assertion that the ghost is absent would pass on an empty registry too.
    assert_eq!(reg.names().collect::<Vec<_>>(), vec!["a", "b"]);
    assert!(strays(h).is_empty(), "the stray was swept: {:?}", strays(h));
}

#[test]
fn a_fresh_temp_is_not_swept_because_it_might_belong_to_a_live_writer() {
    let home = scratch_home();
    let h = home.path();
    make_minds(h, &[("a", "code/a")]);
    let dir = cauldron_dir(h);
    fs::create_dir_all(&dir).unwrap();
    let fresh = dir.join("projects.json.tmp-12345-2");
    fs::write(&fresh, b"in flight").unwrap();

    Registry::bootstrap_if_absent(h, "test").unwrap();

    assert_eq!(
        strays(h),
        vec!["projects.json.tmp-12345-2".to_string()],
        "a temp younger than the threshold must survive"
    );
    // And it was still never read.
    assert_eq!(
        Registry::load(h).unwrap().unwrap().names().collect::<Vec<_>>(),
        vec!["a"]
    );
}

// ──────────────────────────────────────────────────────────────────────────
//  §14.9 / §14.10 — the derivation's rules
// ──────────────────────────────────────────────────────────────────────────

/// `type_3` must not become `type-3`. Normalizing it would re-key its settings
/// override and its replica directory and detach nothing visibly until
/// something failed.
#[test]
fn bootstrapped_names_are_the_basename_verbatim() {
    let home = scratch_home();
    let h = home.path();
    make_minds(
        h,
        &[
            ("type_3", "type_3"),
            ("broomstick-splash", "code/broomstick-splash-redesign"),
            ("UPPER_Case", "code/UPPER_Case"),
        ],
    );
    let reg = Registry::derive_from_minds(h, "test");
    let names: Vec<&str> = reg.names().collect();
    assert_eq!(names, vec!["UPPER_Case", "broomstick-splash", "type_3"]);

    // And the name is NOT derived from the target's basename.
    assert_eq!(
        reg.get("broomstick-splash").unwrap().path,
        fs::canonicalize(h.join("code/broomstick-splash-redesign")).unwrap()
    );
}

/// Today `discover_roots` follows the link's metadata, so a dangling link is
/// INVISIBLE. The day the registry lands must show exactly yesterday's list —
/// so a dangling link is skipped, not registered as a `missing` row.
#[test]
fn dangling_links_are_skipped_so_nothing_appears_that_was_not_there_yesterday() {
    let home = scratch_home();
    let h = home.path();
    make_minds(h, &[("alive", "code/alive")]);
    make_dangling(h, "dead");

    let reg = Registry::derive_from_minds(h, "test");
    // POSITIVE WITNESS: `alive` IS there (so the derivation ran) AND `dead` is not.
    assert_eq!(reg.names().collect::<Vec<_>>(), vec!["alive"]);
    assert!(reg.view(h).iter().all(|r| !r.missing));

    // MEASURED, not assumed: this skip is DOUBLY protected, and one mutation
    // cannot reveal it. The `fs::metadata` gate already fails on a dangling
    // link, so the `canonicalize`-failure arm below it never runs for one.
    // Removing EITHER layer alone leaves this test green (forced-failure
    // campaign, F10); removing both turns it red (F10b). The `canonicalize`
    // arm is not dead — it catches a link whose target became unreadable
    // between the two syscalls — but it is not what defends this behaviour.
}

#[test]
fn dot_entries_and_plain_files_are_skipped() {
    let home = scratch_home();
    let h = home.path();
    make_minds(h, &[("real", "code/real")]);
    let minds = minds_root(h);
    fs::write(minds.join(".DS_Store"), b"\x00\x01").unwrap();
    fs::create_dir_all(minds.join(".hidden-dir")).unwrap();
    fs::write(minds.join("a-file.txt"), b"not a project").unwrap();
    std::os::unix::fs::symlink(h.join("code/real/README"), minds.join("link-to-file")).unwrap();
    fs::create_dir_all(h.join("code/real")).unwrap();
    fs::write(h.join("code/real/README"), b"x").unwrap();

    let reg = Registry::derive_from_minds(h, "test");
    assert_eq!(reg.names().collect::<Vec<_>>(), vec!["real"]);
}

#[test]
fn a_real_directory_under_minds_keeps_the_relic_as_its_path() {
    let home = scratch_home();
    let h = home.path();
    let minds = minds_root(h);
    fs::create_dir_all(minds.join("born-here")).unwrap();
    let reg = Registry::derive_from_minds(h, "test");
    assert_eq!(reg.get("born-here").unwrap().path, minds.join("born-here"));
}

#[test]
fn a_machine_with_no_minds_folder_bootstraps_to_an_empty_list_and_the_bucket() {
    let home = scratch_home();
    let h = home.path();
    assert!(!minds_root(h).exists());

    let out = Registry::bootstrap_if_absent(h, "test").unwrap();
    assert!(out.was_created());
    let reg = Registry::load(h).unwrap().unwrap();
    assert_eq!(reg.len(), 0);
    assert_eq!(reg.view(h).len(), 1, "the bucket is still there");
}

#[test]
fn the_bootstrap_block_records_where_the_list_came_from() {
    let home = scratch_home();
    let h = home.path();
    make_minds(h, &[("a", "code/a")]);
    Registry::bootstrap_if_absent(h, "ui-server").unwrap();
    let b = Registry::load(h).unwrap().unwrap().bootstrap.unwrap();
    assert_eq!(b.from, minds_root(h));
    assert_eq!(b.by, "ui-server");
}

/// The bootstrap creates the FILE, its lock and its temp — and nothing else
/// under his home. Proved by walking the whole scratch root before and after.
#[test]
fn the_bootstrap_creates_nothing_outside_dot_cauldron() {
    let home = scratch_home();
    let h = home.path();
    make_minds(h, &[("a", "code/a")]);
    let before = tree(h);

    Registry::bootstrap_if_absent(h, "test").unwrap();

    let after = tree(h);
    let new: Vec<_> = after.keys().filter(|p| !before.contains_key(*p)).collect();
    assert_eq!(new.len(), 2, "unexpected new paths: {new:?}");
    assert!(new.iter().any(|p| p.ends_with(".cauldron")));
    assert!(new.iter().any(|p| p.ends_with("projects.json")));
}

/// Set a file's mtime into the past, so the sweep sees it as a stray.
fn age_file(path: &std::path::Path, seconds: u64) {
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(seconds);
    let f = fs::File::options().write(true).open(path).unwrap();
    f.set_times(fs::FileTimes::new().set_accessed(when).set_modified(when))
        .unwrap();
}
