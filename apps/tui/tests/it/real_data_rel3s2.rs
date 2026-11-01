//! REL-3.S2: **the face reads real or demo data only, never a repo seed.**
//!
//! A face on a real environment opens with `store::no_seeds`, so an
//! environment shows only its own data (empty when fresh) and its own seed.
//! `cauldron-build` is the one environment with a seed of its own, written by
//! `soup::demo::install` — through the CLI's `env create` and through this
//! face's own `store::create_env`. Every guard here pairs a NEGATIVE arm (a
//! fresh environment shows nothing) with a POSITIVE arm over the same
//! predicate (the fixture, or the curated seed, does show), so neither a
//! constant "nothing" nor a constant "everything" passes.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use tui::cauldron::app::App;
use tui::cauldron::ripples::Kind;
use tui::cauldron::store;

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "rel3s2-{}-{}-{}",
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

/// The face as `tui --cauldron` opens a real environment.
fn open_real(root: &Path, name: &str) -> App {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    let env = env_in(root, name);
    let seeds = store::no_seeds(&env);
    App::open(env, seeds, false).expect("opens")
}

/// What a face shows of seed data, as one comparable census.
#[derive(Debug, PartialEq)]
struct Shown {
    clusters: Vec<String>,
    questions: usize,
    actions: usize,
    artifacts: usize,
    ledger: usize,
    threads: usize,
    dumps: usize,
    accounts: usize,
}

fn shown(a: &App) -> Shown {
    let f = &a.forest;
    Shown {
        clusters: f.clusters.iter().map(|c| c.name.clone()).collect(),
        questions: f.clusters.iter().map(|c| c.questions.len()).sum(),
        actions: f.clusters.iter().map(|c| c.human_actions.len()).sum(),
        artifacts: a.arts.artifacts.len(),
        ledger: a.ledger.dumps.len(),
        // MERGE-15: REL-6's Threads tab reads the DAEMON, never a seed, so
        // this counts the rows it projects (no daemon in an App a test
        // builds: none).
        threads: a.threads.rows.len(),
        dumps: a.sends.sends.len(),
        accounts: a.dash.threads.accounts.len(),
    }
}

/// The census of an environment with nothing in it: its one bare cluster.
fn nothing(name: &str) -> Shown {
    Shown { clusters: vec![name.to_string()], questions: 0, actions: 0, artifacts: 0, ledger: 0, threads: 0, dumps: 0, accounts: 0 }
}

/// Behaviour 1: a fresh environment — `prod` included, the name every repo
/// fallback used to serve — shows nothing seeded, while the SAME face opened
/// on the fixture shows the fixture (the predicate can see data).
#[test]
fn a_fresh_environment_shows_nothing_seeded_and_the_fixture_still_shows_when_named() {
    let root = scratch_root();
    for name in ["prod", "dev", "scratch"] {
        let a = open_real(&root, name);
        assert_eq!(shown(&a), nothing(name), "{name}: a fresh environment shows only its own (empty) data");
    }
    // Positive arm: the fixture, named, is seen on prod.
    let fx = App::open(env_in(&root, "prod"), store::seed_dir(), false).expect("fixture opens");
    let s = shown(&fx);
    // The ripple POLICY a fresh environment starts with is the catalog's
    // (the same definitions as the fixture's), with nothing it did.
    let fresh = open_real(&root, "prod");
    assert_eq!(fresh.rstore.seed_policy.file, fx.rstore.seed_policy.file, "the default policy is the catalog");
    assert!(fresh.rstore.seed.runs.is_empty() && fresh.rstore.seed.bus.is_empty() && fresh.rstore.seed.stats.is_empty(), "and nothing ran");
    assert!(!fx.rstore.seed.runs.is_empty() && !fx.rstore.seed.bus.is_empty(), "the fixture's runs are visible when named");
    assert!(s.clusters.iter().any(|c| c == "Cauldron") && s.questions > 0 && s.artifacts > 0 && s.ledger > 0 && s.accounts > 0, "the fixture is visible when named: {s:?}");
    // MERGE-15 (REL-6 x REL-3.S2): the fixture still carries threads.json,
    // and the Threads tab must NOT show it — threads come only from the
    // daemon. Sentinel first: the seed has threads, so a tab that read it
    // would count them here.
    let seed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(store::seed_dir().join("threads.json")).expect("sentinel: the fixture's threads.json")).expect("json");
    assert!(seed["threads"].as_array().is_some_and(|t| !t.is_empty()), "sentinel: the fixture's threads.json has threads in it");
    assert_eq!(s.threads, 0, "the Threads tab showed seed threads; REL-6 reads only the daemon");
}

/// Behaviour 1, end to end: the BUILT face, launched with no `--seeds`, shows
/// none of the fixture on prod; with `--seeds <fixture>` it does.
#[test]
fn the_built_face_without_seeds_shows_no_fixture_cluster() {
    let root = scratch_root();
    let run = |extra: &[&str]| {
        // MERGE-9b: through the isolation helper (its defaults live in a
        // sibling scratch dir); CAULDRON_HOME is still this test's own root.
        let o = crate::face::Face::new(&root.with_extension("face"))
            .args(["--cauldron", "--dump", "--env", "prod"])
            .args(extra)
            .env("CAULDRON_HOME", &root)
            .env("CAULDRON_TYPE_ONE", "off")
            .env("CAULDRON_OPENER", "none")
            .env_remove("ENV")
            .env_remove("CAULDRON_ENV")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    let seeds = store::seed_dir().to_string_lossy().into_owned();
    let fixture = run(&["--seeds", &seeds]);
    assert!(fixture.contains("Broomstick") && fixture.contains("Harness"), "positive arm: the named fixture is drawn:\n{fixture}");
    let real = run(&[]);
    assert!(!real.contains("Broomstick") && !real.contains("Harness"), "no fixture cluster on a real environment:\n{real}");
}

/// Behaviours 2 and 3 and 4: the face's own `create_env` seeds
/// `cauldron-build` (and nothing else); opened as a real environment it shows
/// the curated clusters, actions and ledger — the ledger read from its OWN
/// seed, with a fold's `thread` and the `type_three` kind word — and its
/// `secrets.env_file` is a known setting.
#[test]
fn the_faces_create_env_seeds_cauldron_build_and_it_shows_the_demo() {
    let root = scratch_root();
    let here = env_in(&root, "prod");
    here.ensure_dirs().unwrap();

    let other = store::create_env(&here, "fresh").expect("creates fresh");
    assert!(!store::own_seed_dir(&store::soup_dir(&other).unwrap()).exists(), "no other environment is seeded");
    assert_eq!(shown(&open_real(&root, "fresh")), nothing("fresh"));

    let cb = store::create_env(&here, soup::demo::ENV).expect("creates cauldron-build");
    let dir = store::soup_dir(&cb).unwrap();
    assert!(store::own_seed(&dir, "clusters.json").is_some() && store::own_seed(&dir, "ledger.json").is_some(), "the face's create seeded it");

    let a = open_real(&root, soup::demo::ENV);
    let s = shown(&a);
    for name in ["The face", "Soup", "Ripples", "Threads daemon", "Release"] {
        assert!(s.clusters.iter().any(|c| c == name), "demo cluster `{name}`: {s:?}");
    }
    assert!(s.actions >= 2 && s.questions > 0 && s.artifacts > 0, "{s:?}");
    // The ledger, from its own seed (not prod's fallback, not empty).
    assert_eq!(s.ledger, 6, "the curated ledger's six dumps: {s:?}");
    let fold = a.ledger.dumps.iter().find(|d| d.seq == 104).expect("dump 104");
    assert_eq!(fold.thread, "Ledger rails", "a fold carries its thread");
    assert!(a.ledger.dumps.iter().find(|d| d.seq == 101).is_some_and(|d| d.thread.is_empty()), "a dump is no fold");
    fn kinds(es: &[tui::cauldron::ledger::Exec], out: &mut Vec<Kind>) {
        for e in es {
            out.push(e.kind);
            kinds(&e.children, out);
        }
    }
    let mut ks = Vec::new();
    for d in &a.ledger.dumps {
        kinds(&d.ripples, &mut ks);
    }
    assert!(ks.contains(&Kind::TypeThree) && ks.contains(&Kind::TypeOne) && ks.contains(&Kind::TypeTwo), "type_three reads as a type 3: {ks:?}");

    // Behaviour 4: the install's `[secrets] env_file` is a setting, and read.
    let w = a.settings.warnings();
    assert!(!w.iter().any(|l| l.contains("is not a setting")), "{w:?}");
    let env_file = a.settings.get(tui::cauldron::settings::def("secrets.env_file"), &Default::default()).val;
    assert_eq!(env_file, tui::cauldron::settings::Val::Str(dir.join(soup::demo::SECRETS_ENV).to_string_lossy().into_owned()));
    // And a key that is NOT a setting still warns (the check can fire).
    std::fs::write(dir.join("settings.toml"), std::fs::read_to_string(dir.join("settings.toml")).unwrap() + "\n[nonsense]\nkey = 1\n").unwrap();
    let w = open_real(&root, soup::demo::ENV).settings.warnings();
    assert!(w.iter().any(|l| l.contains("nonsense.key") && l.contains("is not a setting")), "{w:?}");
}

/// Behaviour 5: the token board (`global.json`) is PER ENVIRONMENT — one
/// environment's own seed shows it, another environment on the same root
/// does not.
#[test]
fn the_global_board_is_per_environment() {
    let root = scratch_root();
    let a_dir = store::soup_dir(&env_in(&root, "alpha")).unwrap();
    let own = store::own_seed_dir(&a_dir);
    std::fs::create_dir_all(&own).unwrap();
    std::fs::copy(store::seed_dir().join("global.json"), own.join("global.json")).unwrap();
    let alpha = open_real(&root, "alpha");
    let beta = open_real(&root, "beta");
    assert!(!alpha.dash.threads.accounts.is_empty(), "alpha's own board shows in alpha");
    assert!(beta.dash.threads.accounts.is_empty(), "and not in beta: {:?}", beta.dash.threads.accounts.len());
}
