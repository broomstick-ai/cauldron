//! **THE `cauldron` BINARY, DRIVEN AS A REAL PROCESS.**
//!
//! Every test here spawns `CARGO_BIN_EXE_projects` with `$CAULDRON_HOME`
//! pointed at a `tempfile` root, so nothing in this file can reach
//! `~/.cauldron/projects.json`. That is not a convention, it is the whole
//! safety story: a test that wrote his real registry would replace 32 projects
//! with one, which is the exact failure `refuse_if_unminted` exists to prevent.
//!
//! The arbitration proof at the bottom is at N >= 2 ACROSS REAL PROCESSES
//! (`core:writing-tools`), and it ships with a DETERMINISTIC NEGATIVE CONTROL:
//! `the_lost_update_check_can_fail` makes the very assertion that scores the
//! arbitration proof go red, using the crate's own documented stale-clobber
//! path. Without that control, "15 entries survived" is evidence-shaped rather
//! than evidence — a counting assertion that cannot fail proves nothing about
//! the lock.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use projects::*;
use support::*;

const BIN: &str = env!("CARGO_BIN_EXE_projects");

// ──────────────────────────────────────────────────────────────────────────
//  Driving the binary
// ──────────────────────────────────────────────────────────────────────────

struct Ran {
    code: i32,
    out: String,
    err: String,
}

impl Ran {
    fn of(o: Output) -> Ran {
        Ran {
            code: o.status.code().expect("the child was not signalled"),
            out: String::from_utf8_lossy(&o.stdout).to_string(),
            err: String::from_utf8_lossy(&o.stderr).to_string(),
        }
    }
    fn ok(&self) -> &Ran {
        assert_eq!(self.code, 0, "expected success\nstdout:{}\nstderr:{}", self.out, self.err);
        self
    }
}

/// **The only way this file starts the binary.** `$CAULDRON_HOME` is set on
/// every single invocation, so there is no path through this file that reads
/// or writes his real registry.
fn run(home: &Path, args: &[&str]) -> Ran {
    let o = Command::new(BIN)
        .args(args)
        .env("CAULDRON_HOME", home)
        .stdin(Stdio::null())
        .output()
        .expect("the cauldron binary should run");
    Ran::of(o)
}

/// A directory that exists, so `register` has something real to canonicalize.
fn project(home: &Path, rel: &str) -> PathBuf {
    let p = home.join(rel);
    fs::create_dir_all(&p).expect("project dir");
    p
}

// ──────────────────────────────────────────────────────────────────────────
//  list
// ──────────────────────────────────────────────────────────────────────────

/// The coexistence window (`0071` §10): before the registry is minted, `list`
/// shows today's `~/minds` list and SAYS that is what it is doing. It must
/// also write nothing at all — witnessed by the whole tree, not by the
/// absence of one file.
#[test]
fn list_before_the_registry_exists_derives_from_minds_and_writes_nothing() {
    let home = scratch_home();
    let h = home.path();
    make_minds(h, &[("harness", "code/harness"), ("type_3", "code/type_3")]);
    let before = tree(h);

    let r = run(h, &["list"]);
    r.ok();

    assert!(r.out.contains("registry not created yet"), "got: {}", r.out);
    assert!(r.out.contains("harness"), "got: {}", r.out);
    // VERBATIM. `type_3` must not have become `type-3` on its way to a screen.
    assert!(r.out.contains("type_3"), "got: {}", r.out);
    assert!(r.out.contains(BUCKET), "the bucket is always drawn: {}", r.out);
    assert!(!registry_path(h).exists(), "`list` minted a registry");
    assert_eq!(before, tree(h), "`list` wrote something");
}

/// The bucket is LAST and is not counted, and a path whose directory has gone
/// keeps its row and is drawn `missing` (`0071` §3).
#[test]
fn list_draws_the_registered_rows_then_the_bucket_and_marks_a_missing_one() {
    let home = scratch_home();
    let h = home.path();
    let here = project(h, "code/here");
    let gone = project(h, "code/gone");
    run(h, &["register", here.to_str().unwrap()]).ok();
    run(h, &["register", gone.to_str().unwrap()]).ok();
    fs::remove_dir_all(&gone).expect("remove the checkout out from under the entry");

    let r = run(h, &["list"]);
    r.ok();
    let lines: Vec<&str> = r.out.lines().collect();

    assert!(lines[0].starts_with("2 projects"), "header counts the registered only: {}", lines[0]);
    assert!(lines[1].contains("here") && !lines[1].contains("(missing)"));
    assert!(lines[2].contains("gone") && lines[2].contains("(missing)"), "got: {}", lines[2]);
    assert!(lines[3].contains(BUCKET) && lines[3].contains("(bucket)"), "got: {}", lines[3]);
    assert_eq!(lines.len(), 4, "nothing after the bucket: {:?}", lines);
}

/// `--json` carries the schema the crate actually has — name, path,
/// `registered_at`, and the row's `missing` — plus the source, so a caller can
/// tell a minted registry from a derived one.
#[test]
fn list_json_carries_the_real_schema_and_the_source() {
    let home = scratch_home();
    let h = home.path();
    let p = project(h, "code/thing");
    run(h, &["register", p.to_str().unwrap()]).ok();

    let r = run(h, &["list", "--json"]);
    r.ok();
    let v: serde_json::Value = serde_json::from_str(&r.out).expect("--json emits JSON");

    assert_eq!(v["source"], "file");
    assert_eq!(v["count"], 1);
    assert_eq!(v["registry"], serde_json::json!(registry_path(h)));
    let one = &v["projects"][0];
    assert_eq!(one["name"], "thing");
    assert_eq!(one["path"], serde_json::json!(fs::canonicalize(&p).unwrap()));
    assert_eq!(one["missing"], false);
    assert!(
        one["registered_at"].as_str().is_some_and(|s| s.contains('T')),
        "registered_at is RFC 3339: {one}"
    );
    assert_eq!(v["bucket"]["name"], BUCKET);
    assert_eq!(v["bucket"]["path"], serde_json::json!(bucket_root(h)));

    // The derived source is spelled differently, and a caller can act on it.
    let other = scratch_home();
    make_minds(other.path(), &[("a", "code/a")]);
    let d = run(other.path(), &["list", "--json"]);
    d.ok();
    let dv: serde_json::Value = serde_json::from_str(&d.out).unwrap();
    assert_eq!(dv["source"], "derived-from-minds");
}

// ──────────────────────────────────────────────────────────────────────────
//  register
// ──────────────────────────────────────────────────────────────────────────

/// The name defaults to the basename of the CANONICALIZED path, verbatim, and
/// `--name` overrides it. Verbatim is the load-bearing half: a normalizing CLI
/// would re-key `type_3`'s settings and its replica directory.
#[test]
fn register_defaults_the_name_to_the_basename_verbatim_and_honours_dash_name() {
    let home = scratch_home();
    let h = home.path();
    let a = project(h, "code/type_3");
    let b = project(h, "code/broomstick-splash-redesign");

    run(h, &["register", a.to_str().unwrap()]).ok();
    run(h, &["register", b.to_str().unwrap(), "--name", "broomstick-splash"]).ok();

    let reg = Registry::load(h).unwrap().expect("the file is there");
    let names: Vec<&str> = reg.names().collect();
    assert_eq!(names, vec!["type_3", "broomstick-splash"], "and in the order they were added");
    assert_eq!(reg.get("type_3").unwrap().path, fs::canonicalize(&a).unwrap());
}

/// Every refusal the crate owns reaches a shell as exit 1 plus the crate's own
/// sentence on stderr, and NOTHING is written. The sentences are asserted
/// verbatim because `src/refusal.rs` exists so that the one he hears from the
/// AUI is the one he reads here.
#[test]
fn register_speaks_the_crates_refusals_on_stderr_exits_one_and_writes_nothing() {
    let home = scratch_home();
    let h = home.path();
    let good = project(h, "code/good");
    run(h, &["register", good.to_str().unwrap()]).ok();
    let after_the_one_good_write = fs::read(registry_path(h)).unwrap();

    let file = h.join("code/a-file");
    fs::write(&file, b"not a directory").unwrap();
    let missing = h.join("code/never-existed");
    let bucketish = project(h, "code/_Miscellaneous");
    let second = project(h, "code/second");

    let cases: Vec<(Vec<&str>, String)> = vec![
        (
            vec!["register", missing.to_str().unwrap()],
            nothing_at_path(&missing).sentence().to_string(),
        ),
        (
            vec!["register", file.to_str().unwrap()],
            not_a_directory(&file).sentence().to_string(),
        ),
        (
            vec!["register", bucketish.to_str().unwrap()],
            name_reserved("_Miscellaneous").sentence().to_string(),
        ),
        (
            vec!["register", second.to_str().unwrap(), "--name", "good"],
            name_taken("good").sentence().to_string(),
        ),
        (
            vec!["register", good.to_str().unwrap(), "--name", "again"],
            path_already_registered(&fs::canonicalize(&good).unwrap(), "good")
                .sentence()
                .to_string(),
        ),
        (
            vec!["register", second.to_str().unwrap(), "--name", "a/b"],
            unusable_name("a/b", "a name cannot contain a path separator")
                .sentence()
                .to_string(),
        ),
    ];

    for (args, sentence) in cases {
        let r = run(h, &args);
        assert_eq!(r.code, 1, "{args:?} should exit 1, said {}", r.out);
        assert_eq!(r.err.trim_end(), sentence, "{args:?}");
        assert!(r.out.is_empty(), "{args:?} printed to stdout: {}", r.out);
        assert_eq!(
            fs::read(registry_path(h)).unwrap(),
            after_the_one_good_write,
            "{args:?} changed the registry"
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  remove
// ──────────────────────────────────────────────────────────────────────────

/// De-register removes the ENTRY. Not the directory, not its `.mind/`, not
/// `~/minds/<name>` (`0071` §3) — witnessed by the whole tree outside
/// `~/.cauldron`, because an assertion that the directory still exists would
/// not notice `.mind/` being emptied.
#[test]
fn remove_takes_the_entry_out_and_leaves_every_other_byte_alone() {
    let home = scratch_home();
    let h = home.path();
    let p = project(h, "code/thing");
    fs::create_dir_all(p.join(".mind")).unwrap();
    fs::write(p.join(".mind/threads.json"), b"{\"threads\":[]}").unwrap();
    // `~/minds/thing -> code/thing`, and the registry minted from it exactly as
    // `ui-server` would at boot — so the entry being removed is one that still
    // has a relic symlink wearing its name. That is the case where "removes the
    // entry and nothing else" is worth asserting.
    make_minds(h, &[("thing", "code/thing")]);
    Registry::bootstrap_if_absent(h, "test-not-ui-server").expect("mint");
    assert_eq!(Registry::load(h).unwrap().unwrap().names().collect::<Vec<_>>(), vec!["thing"]);
    assert_eq!(Registry::load(h).unwrap().unwrap().get("thing").unwrap().path, fs::canonicalize(&p).unwrap());

    let code_before = tree(&h.join("code"));
    let minds_before = tree(&h.join("minds"));

    let r = run(h, &["remove", "thing"]);
    r.ok();
    assert!(r.out.contains("Removed `thing`"), "got: {}", r.out);

    assert!(Registry::load(h).unwrap().unwrap().is_empty(), "the entry is gone");
    assert_eq!(code_before, tree(&h.join("code")), "the checkout was touched");
    assert_eq!(minds_before, tree(&h.join("minds")), "the ~/minds relic was touched");
}

#[test]
fn remove_refuses_an_unknown_name_and_the_bucket() {
    let home = scratch_home();
    let h = home.path();
    run(h, &["register", project(h, "code/real").to_str().unwrap()]).ok();
    let before = fs::read(registry_path(h)).unwrap();

    let r = run(h, &["remove", "nope"]);
    assert_eq!(r.code, 1);
    assert_eq!(r.err.trim_end(), no_such_project("nope").sentence());

    let b = run(h, &["remove", BUCKET]);
    assert_eq!(b.code, 1);
    assert_eq!(b.err.trim_end(), cannot_remove_bucket().sentence());

    assert_eq!(fs::read(registry_path(h)).unwrap(), before);
}

// ──────────────────────────────────────────────────────────────────────────
//  THE GUARD: this binary is not the minter
// ──────────────────────────────────────────────────────────────────────────

/// **The defect this binary refuses to commit.**
///
/// `Registry::update` on an absent file starts from an EMPTY registry, and
/// `bootstrap_if_absent`'s trigger is that same absence. So a `register` run
/// before the AUI's first boot would create a one-entry document AND remove the
/// only trigger that would ever have read the 32 symlinks in `~/minds`. The
/// refusal is the deliverable; the silent write is the defect.
#[test]
fn register_refuses_to_write_before_the_registry_is_minted_when_minds_has_something_to_lose() {
    let home = scratch_home();
    let h = home.path();
    make_minds(h, &[("one", "code/one"), ("two", "code/two"), ("three", "code/three")]);
    let p = project(h, "code/new-thing");
    let before = tree(h);

    let r = run(h, &["register", p.to_str().unwrap()]);
    assert_eq!(r.code, 1, "it should refuse, it said: {}", r.out);
    assert!(r.err.contains("isn't there yet"), "got: {}", r.err);
    assert!(r.err.contains("3 projects"), "it names what is at stake: {}", r.err);
    assert!(r.err.contains("Start the AUI once"), "it names the fix: {}", r.err);

    assert!(!registry_path(h).exists(), "the refusal minted a registry");
    // Not even `~/.cauldron` itself: the guard runs BEFORE `update`'s
    // `create_dir_all`, so a refused write leaves no trace at all.
    assert_eq!(before, tree(h), "the refusal wrote something");

    // And `remove` refuses on the same ground rather than silently reporting
    // "I don't have a project called x" about a list it never read.
    let rm = run(h, &["remove", "one"]);
    assert_eq!(rm.code, 1);
    assert!(rm.err.contains("isn't there yet"), "got: {}", rm.err);
}

/// The other side of the same guard, and the case the crate blesses by name:
/// nothing in `~/minds` means nothing to lose, so the first `register` on a
/// machine the AUI has never booted writes the first registry.
#[test]
fn with_nothing_in_minds_to_lose_the_first_register_writes_the_first_registry() {
    let home = scratch_home();
    let h = home.path();
    let p = project(h, "code/first");

    run(h, &["register", p.to_str().unwrap()]).ok();

    let reg = Registry::load(h).unwrap().expect("it wrote one");
    assert_eq!(reg.names().collect::<Vec<_>>(), vec!["first"]);
    assert_eq!(reg.version, VERSION);
    // A registry this binary wrote carries no bootstrap provenance: it did not
    // derive anything, and claiming it had would be a lie in the file.
    assert!(reg.bootstrap.is_none(), "the CLI must not claim to have bootstrapped");
}

/// A `~/minds` full of DANGLING links has nothing to lose either, because the
/// derivation cannot see them — the guard counts through the crate's own
/// predicate, so it agrees with the bootstrap by construction rather than by a
/// second `read_dir` that could drift.
#[test]
fn the_guard_counts_what_the_bootstrap_would_actually_derive_not_what_read_dir_returns() {
    let home = scratch_home();
    let h = home.path();
    make_dangling(h, "ghost-one");
    make_dangling(h, "ghost-two");
    let p = project(h, "code/real");

    run(h, &["register", p.to_str().unwrap()]).ok();
    assert_eq!(Registry::load(h).unwrap().unwrap().len(), 1);
}

// ──────────────────────────────────────────────────────────────────────────
//  The command line itself
// ──────────────────────────────────────────────────────────────────────────

#[test]
fn a_command_line_it_does_not_understand_is_exit_two_and_never_exit_one() {
    let home = scratch_home();
    let h = home.path();
    for args in [
        vec![],
        vec!["frobnicate"],
        vec!["register"],
        vec!["register", "/tmp", "--name"],
        vec!["remove"],
        vec!["remove", "a", "b"],
        vec!["list", "--wat"],
    ] {
        let r = run(h, &args);
        assert_eq!(r.code, 2, "{args:?} should be a usage error, got {}", r.code);
        assert!(r.err.contains("projects list"), "usage is printed: {}", r.err);
    }
    let help = run(h, &["--help"]);
    help.ok();
    assert!(help.out.contains("projects register <path>"));
    assert!(!registry_path(h).exists(), "parsing minted a registry");
}

// ──────────────────────────────────────────────────────────────────────────
//  ARBITRATION AT N >= 2, ACROSS REAL PROCESSES
// ──────────────────────────────────────────────────────────────────────────

/// **Four CLI processes and an AUI-shaped writer, all at once, lose nothing.**
///
/// The CLI is a SECOND WRITER of a file whose crate says `ui-server` is the
/// only one. This is the proof that the claim in the binary's module note —
/// "a second writer cannot lose an update" — is true of the binary and not
/// just of the library function it calls.
///
/// The fifth writer is the AUI's own write pattern: `Registry::update` in this
/// process, which is exactly what `apps/aui/src/dispatch/mod.rs:271` does. It
/// is NOT the real `ui-server` and never touches it.
///
/// They rendezvous on a gate file rather than on spawn order, so all five
/// arrive at the contended instruction together instead of trusting process
/// startup jitter to overlap them.
#[test]
fn four_cli_processes_and_an_aui_shaped_writer_racing_one_registry_lose_nothing() {
    const PROCS: usize = 4;
    const EACH: usize = 3;
    const IN_PROCESS: usize = 3;

    let home = scratch_home();
    let h = home.path().to_path_buf();
    // Minted first, exactly as `ui-server` would at boot, so the guard is not
    // what this test is measuring.
    Registry::bootstrap_if_absent(&h, "test-not-ui-server").expect("mint");

    let gate = h.join("GATE");
    let mut expected: Vec<String> = Vec::new();
    let mut kids = Vec::new();

    for k in 0..PROCS {
        let mut script = format!("while [ ! -e '{}' ]; do :; done\n", gate.display());
        for i in 0..EACH {
            let name = format!("cli-{k}-{i}");
            let dir = h.join("code").join(&name);
            fs::create_dir_all(&dir).expect("project dir");
            script.push_str(&format!(
                "'{}' register '{}' --name '{name}' || exit 1\n",
                BIN,
                dir.display()
            ));
            expected.push(name);
        }
        kids.push(
            Command::new("/bin/sh")
                .arg("-c")
                .arg(&script)
                .env("CAULDRON_HOME", &h)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn a cauldron child"),
        );
    }

    // Everybody is spinning on the gate; open it.
    fs::write(&gate, b"go").expect("gate");

    // The AUI-shaped writer, in this process, against the same lock.
    for i in 0..IN_PROCESS {
        let name = format!("aui-shaped-{i}");
        let dir = h.join("code").join(&name);
        fs::create_dir_all(&dir).expect("project dir");
        Registry::update(&h, |r| r.register(&dir, Some(&name)).map(|_| ()))
            .expect("the in-process writer");
        expected.push(name);
    }

    for kid in kids {
        let o = kid.wait_with_output().expect("child");
        assert!(
            o.status.success(),
            "a child failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    let reg = Registry::load(&h).unwrap().expect("the document is still there and still parses");
    let mut got: Vec<String> = reg.names().map(str::to_string).collect();
    got.sort();
    expected.sort();
    assert_eq!(
        got, expected,
        "{} writers, {} registrations, zero lost updates",
        PROCS + 1,
        PROCS * EACH + IN_PROCESS
    );
}

/// **THE NEGATIVE CONTROL FOR THE TEST ABOVE, AND IT IS DETERMINISTIC.**
///
/// The proof above scores itself by counting surviving entries. A count that
/// cannot come out short is not a measurement, so here is the same count coming
/// out short: two writers that hold a STALE copy and call `save` — the path
/// `Registry::save`'s own doc comment warns about ("a caller holding a stale
/// copy overwrites a change it never saw") — and the second one silently
/// erases the first.
///
/// No race is needed to show it, which is the point: if the assertion style
/// used above could not detect a lost update, this test would be green with
/// `assert_eq!(names.len(), 2)`.
#[test]
fn the_lost_update_check_can_fail() {
    let home = scratch_home();
    let h = home.path();
    Registry::bootstrap_if_absent(h, "control").expect("mint");

    let a = project(h, "code/alpha");
    let b = project(h, "code/beta");

    // Two writers, each with the document as it was before either wrote.
    let mut first = Registry::load(h).unwrap().unwrap();
    let mut second = Registry::load(h).unwrap().unwrap();
    first.register(&a, Some("alpha")).unwrap();
    second.register(&b, Some("beta")).unwrap();
    first.save(h).unwrap();
    second.save(h).unwrap();

    let names: Vec<String> = Registry::load(h).unwrap().unwrap().names().map(str::to_string).collect();
    assert_eq!(
        names,
        vec!["beta".to_string()],
        "the second stale writer must have erased the first — if BOTH are here, \
         the lost-update assertion in the proof above is not measuring anything"
    );
    assert!(!names.contains(&"alpha".to_string()), "alpha was lost, and that is the control");
}
