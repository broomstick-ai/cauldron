//! **THE ARBITRATION PROOFS, AT N >= 2, ACROSS REAL PROCESSES.**
//!
//! `core:writing-tools`: anything that arbitrates between two or more of
//! something — a lock, a lease, a race — is proven at N >= 2 or not proven,
//! because its empty and single cases cannot exercise the contention it exists
//! to manage. A single-process test of a locking scheme is not a test of the
//! locking scheme.
//!
//! So these suites fork. Each spawns this same test binary with
//! `--ignored --exact child_worker` and an environment saying which role to
//! play and when to start; the children rendezvous on a shared wall-clock
//! instant so they arrive at the contended instruction together, and each
//! writes its verdict to its own file. The parent reads the files.
//!
//! Three things are arbitrated and each is proved here:
//!
//! 1. **Creation** — `hard_link`'s `EEXIST`. One winner, N-1 losers, one
//!    document. Witnessed by counting actual `LostRace` outcomes, not by
//!    inferring contention from the clock.
//! 2. **The lock** — `update`'s read-modify-write. N writers, zero lost
//!    updates. Witnessed by a positive count of surviving entries, not by the
//!    absence of an error.
//! 3. **Tearing** — write-temp/fsync/rename. A reader looping through the
//!    storm parses every time, and is REQUIRED to have observed the document
//!    change under it, so a reader that never ran cannot score a pass.

mod support;

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use projects::*;
use support::*;

const E_HOME: &str = "CAULDRON_CHILD_HOME";
const E_ROLE: &str = "CAULDRON_CHILD_ROLE";
const E_OUT: &str = "CAULDRON_CHILD_OUT";
const E_START: &str = "CAULDRON_CHILD_START_NANOS";
const E_TAG: &str = "CAULDRON_CHILD_TAG";
const E_N: &str = "CAULDRON_CHILD_N";

// ──────────────────────────────────────────────────────────────────────────
//  The child
// ──────────────────────────────────────────────────────────────────────────

/// **The worker process.** Never run by `cargo test`; spawned by the suites
/// below with `--ignored --exact child_worker`.
#[test]
#[ignore = "child process worker, spawned by the concurrency suites"]
fn child_worker() {
    let home = PathBuf::from(env::var(E_HOME).expect("child needs a home"));
    let role = env::var(E_ROLE).expect("child needs a role");
    let out = PathBuf::from(env::var(E_OUT).expect("child needs an out path"));
    let tag = env::var(E_TAG).unwrap_or_default();
    let n: usize = env::var(E_N).ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    rendezvous();

    let verdict = match role.as_str() {
        "bootstrap" => {
            let r = Registry::bootstrap_if_absent(&home, &tag).expect("bootstrap");
            let outcome = if r.was_created() {
                "created"
            } else if r.lost_race() {
                "lost"
            } else {
                "present"
            };
            format!("{outcome} {}", digest(&r.registry().to_json()))
        }
        "update" => {
            let mut done = 0usize;
            for i in 0..n {
                let name = format!("p-{tag}-{i}");
                let dir = home.join("code").join(&name);
                fs::create_dir_all(&dir).expect("project dir");
                match Registry::update(&home, |r| r.register(&dir, Some(&name)).map(|_| ())) {
                    Ok(()) => done += 1,
                    Err(e) => panic!("update {name} failed: {e}"),
                }
            }
            format!("updated {done}")
        }
        "read" => {
            // Loop until the stop file appears, recording what we saw. The
            // read that sees STOP still loads once more: STOP is written
            // after every writer has finished, so that last load is the one
            // that must see the final document (without it the loop could end
            // on a load begun before the last write).
            let stop = home.join("STOP");
            let mut parses = 0u64;
            let mut errors = 0u64;
            let mut absent = 0u64;
            let mut sizes: BTreeSet<usize> = BTreeSet::new();
            let mut last = false;
            while !last {
                last = stop.exists();
                match Registry::load(&home) {
                    Ok(Some(r)) => {
                        parses += 1;
                        sizes.insert(r.len());
                    }
                    Ok(None) => absent += 1,
                    Err(_) => errors += 1,
                }
            }
            format!(
                "read parses={parses} errors={errors} absent={absent} distinct_sizes={} max={}",
                sizes.len(),
                sizes.iter().next_back().copied().unwrap_or(0)
            )
        }
        other => panic!("unknown role {other}"),
    };

    fs::write(&out, verdict).expect("write verdict");
}

/// Spin until the shared start instant. A `sleep` to a deadline would be
/// enough for correctness; the spin is what makes the children actually
/// collide rather than merely overlap.
fn rendezvous() {
    let Ok(start) = env::var(E_START) else { return };
    let Ok(start) = start.parse::<u128>() else { return };
    loop {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        if now >= start {
            return;
        }
        std::hint::spin_loop();
    }
}

fn digest(s: &str) -> u64 {
    // FNV-1a. A hash, not a security property: two children agree or they do not.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ──────────────────────────────────────────────────────────────────────────
//  Spawning
// ──────────────────────────────────────────────────────────────────────────

struct Child {
    proc: std::process::Child,
    out: PathBuf,
}

fn spawn(home: &Path, role: &str, tag: &str, out: PathBuf, start: u128, n: usize) -> Child {
    let exe = env::current_exe().expect("test binary");
    let proc = Command::new(exe)
        .args(["--exact", "child_worker", "--ignored", "--nocapture"])
        .env(E_HOME, home)
        .env(E_ROLE, role)
        .env(E_OUT, &out)
        .env(E_TAG, tag)
        .env(E_START, start.to_string())
        .env(E_N, n.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn child");
    Child { proc, out }
}

fn start_in(ms: u64) -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        + (ms as u128) * 1_000_000
}

fn collect(children: Vec<Child>) -> Vec<String> {
    let mut out = Vec::new();
    for c in children {
        let Child { mut proc, out: p } = c;
        let status = proc.wait().expect("wait");
        assert!(status.success(), "a child process failed: {status}");
        out.push(fs::read_to_string(&p).expect("child verdict"));
    }
    out
}

// ──────────────────────────────────────────────────────────────────────────
//  1. Creation: N processes bootstrap the same home
// ──────────────────────────────────────────────────────────────────────────

/// **§14.3.** `TRIALS` independent races, `RACERS` processes each. Per trial:
/// exactly ONE `created`, and every child's document identical. Across the run
/// at least one child must actually have LOST the `hard_link` — otherwise the
/// rendezvous failed and this suite proved nothing but serialization.
#[test]
fn n_processes_bootstrapping_one_home_produce_one_document_and_one_winner() {
    const TRIALS: usize = 16;
    const RACERS: usize = 4;

    let mut total_lost = 0usize;
    let mut trials_with_contention = 0usize;

    for trial in 0..TRIALS {
        let home = scratch_home();
        let h = home.path();
        make_minds(h, &[("a", "code/a"), ("b", "code/b"), ("type_3", "type_3")]);

        let start = start_in(120);
        let children: Vec<Child> = (0..RACERS)
            .map(|i| {
                spawn(
                    h,
                    "bootstrap",
                    &format!("racer-{i}"),
                    h.join(format!("verdict-{i}")),
                    start,
                    1,
                )
            })
            .collect();

        let verdicts = collect(children);
        let created = verdicts.iter().filter(|v| v.starts_with("created")).count();
        let lost = verdicts.iter().filter(|v| v.starts_with("lost")).count();

        assert_eq!(
            created, 1,
            "trial {trial}: exactly one process may create the registry; got {verdicts:?}"
        );

        // Every child came back with the SAME document — not two, not a torn one.
        let digests: BTreeSet<&str> = verdicts
            .iter()
            .map(|v| v.split(' ').nth(1).expect("digest"))
            .collect();
        assert_eq!(
            digests.len(),
            1,
            "trial {trial}: the racers saw different documents: {verdicts:?}"
        );

        // And it is exactly what the derivation says, with the bucket synthesized.
        let reg = Registry::load(h).unwrap().unwrap();
        assert_eq!(reg.names().collect::<Vec<_>>(), vec!["a", "b", "type_3"]);
        assert!(strays(h).is_empty(), "trial {trial}: losers left temps behind");

        total_lost += lost;
        if lost > 0 {
            trials_with_contention += 1;
        }
    }

    // THE WITNESS THAT THE RACE HAPPENED. Without this a run in which the
    // children never overlapped — each finding the file already there — would
    // score a triumphant pass on a mechanism it never touched.
    assert!(
        total_lost > 0,
        "no process ever lost the hard_link: the racers never contended, so this proves nothing"
    );
    eprintln!(
        "bootstrap race: {trials_with_contention}/{TRIALS} trials had a real EEXIST loser, \
         {total_lost} losers in total across {} racers",
        TRIALS * RACERS
    );
}

// ──────────────────────────────────────────────────────────────────────────
//  2. The lock: N processes read-modify-write the same document
// ──────────────────────────────────────────────────────────────────────────

/// **§14.4's write half.** Four processes, twenty-five registrations each.
/// Under the lock the final document holds all one hundred. Without it, a
/// writer that loaded before another's rename overwrites it and entries VANISH
/// — which is why the assertion is a count of survivors and the names they
/// carry, not "no error was returned".
#[test]
fn four_processes_doing_a_hundred_registrations_lose_nothing() {
    const WRITERS: usize = 4;
    const EACH: usize = 25;

    let home = scratch_home();
    let h = home.path();
    Registry::bootstrap_if_absent(h, "test").unwrap();

    let start = start_in(150);
    let children: Vec<Child> = (0..WRITERS)
        .map(|w| {
            spawn(
                h,
                "update",
                &format!("w{w}"),
                h.join(format!("verdict-w{w}")),
                start,
                EACH,
            )
        })
        .collect();
    let verdicts = collect(children);
    for v in &verdicts {
        assert_eq!(v, &format!("updated {EACH}"), "a writer did not finish: {v}");
    }

    let reg = Registry::load(h).unwrap().unwrap();
    let got: BTreeSet<&str> = reg.names().collect();
    let want: BTreeSet<String> = (0..WRITERS)
        .flat_map(|w| (0..EACH).map(move |i| format!("p-w{w}-{i}")))
        .collect();
    let want_refs: BTreeSet<&str> = want.iter().map(String::as_str).collect();

    let missing: Vec<&&str> = want_refs.difference(&got).collect();
    assert!(
        missing.is_empty(),
        "{} of {} registrations were lost: {:?}",
        missing.len(),
        want_refs.len(),
        &missing[..missing.len().min(10)]
    );
    assert_eq!(reg.len(), WRITERS * EACH);
}

// ──────────────────────────────────────────────────────────────────────────
//  3. Tearing: a reader through the storm
// ──────────────────────────────────────────────────────────────────────────

/// **§14.4's read half.** A separate PROCESS loops `load` while three writers
/// rewrite the document a hundred times. It must parse successfully every
/// time, never find the file absent, and — the part that makes the pass mean
/// something — must have OBSERVED THE DOCUMENT CHANGE UNDER IT. A reader that
/// crashed on startup, or one that ran entirely after the writers finished,
/// would produce a clean "no parse errors" and prove nothing at all.
#[test]
fn a_reader_racing_a_hundred_writes_never_sees_a_torn_document() {
    const WRITERS: usize = 3;
    const EACH: usize = 34;

    let home = scratch_home();
    let h = home.path();
    Registry::bootstrap_if_absent(h, "test").unwrap();

    let start = start_in(200);
    let reader = spawn(h, "read", "r", h.join("verdict-r"), start, 1);
    let writers: Vec<Child> = (0..WRITERS)
        .map(|w| {
            spawn(
                h,
                "update",
                &format!("w{w}"),
                h.join(format!("verdict-w{w}")),
                start,
                EACH,
            )
        })
        .collect();

    let _ = collect(writers);
    fs::write(h.join("STOP"), b"").unwrap();
    let verdict = collect(vec![reader]).remove(0);

    let field = |k: &str| -> u64 {
        verdict
            .split_whitespace()
            .find_map(|t| t.strip_prefix(k).map(|v| v.parse::<u64>().unwrap()))
            .unwrap_or_else(|| panic!("no {k} in {verdict:?}"))
    };

    assert_eq!(field("errors="), 0, "the reader saw a torn document: {verdict}");
    assert_eq!(field("absent="), 0, "the reader saw the file vanish: {verdict}");
    assert!(field("parses=") > 0, "the reader never ran: {verdict}");
    assert!(
        field("distinct_sizes=") >= 2,
        "the reader never saw the document change, so it never raced the writers: {verdict}"
    );
    assert_eq!(
        field("max="),
        (WRITERS * EACH) as u64,
        "the reader never reached the final document: {verdict}"
    );
    eprintln!("torn-proof reader: {verdict}");
}

// ──────────────────────────────────────────────────────────────────────────
//  4. The same arbitration, in-process, as a second witness
// ──────────────────────────────────────────────────────────────────────────

/// Threads rather than processes — a WEAKER test, kept because it is the one
/// that also proves the lock is not accidentally re-entrant within a process
/// (`fd-lock` locks an open file DESCRIPTION, and `update` opens its own).
#[test]
fn eight_threads_in_one_process_also_lose_nothing() {
    const THREADS: usize = 8;
    const EACH: usize = 12;

    let home = scratch_home();
    let h = home.path().to_path_buf();
    Registry::bootstrap_if_absent(&h, "test").unwrap();

    std::thread::scope(|s| {
        for t in 0..THREADS {
            let h = h.clone();
            s.spawn(move || {
                for i in 0..EACH {
                    let name = format!("t{t}-{i}");
                    let dir = h.join("code").join(&name);
                    fs::create_dir_all(&dir).unwrap();
                    Registry::update(&h, |r| r.register(&dir, Some(&name)).map(|_| ())).unwrap();
                }
            });
        }
    });

    let reg = Registry::load(&h).unwrap().unwrap();
    assert_eq!(reg.len(), THREADS * EACH, "entries were lost under contention");
}

/// A `LostRace` deletes its own temp. Proved across processes above; asserted
/// here directly so the sweep is not the thing keeping the directory clean.
#[test]
fn a_bootstrap_that_loses_leaves_no_temp_behind() {
    let home = scratch_home();
    let h = home.path();
    make_minds(h, &[("a", "code/a")]);
    Registry::bootstrap_if_absent(h, "winner").unwrap();
    // The file exists, so this one short-circuits — and still writes no temp.
    let again = Registry::bootstrap_if_absent(h, "loser").unwrap();
    assert!(matches!(again, Bootstrapped::AlreadyPresent(_)));
    assert!(strays(h).is_empty());
}
