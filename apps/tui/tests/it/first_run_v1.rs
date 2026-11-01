//! **V1.2 — FIRST RUN** (release R-V1 item 2; product spec §2-§3, Q-F1).
//!
//! A root with no environment opens on ONE action, `+ create your first
//! cauldron`, and makes nothing; the cauldron made from it is the one the face
//! opens next time nobody names one; and a cauldron with no cluster opens with
//! the sidebar's root `+` selected and focused. Hermetic: every root is a fresh
//! scratch directory, used as both HOME and CAULDRON_HOME, with
//! CAULDRON_TYPE_ONE=off and CAULDRON_OPENER=none. Never the real HOME, never a
//! running face, never a daemon.
//!
//! The first-cluster state needs a cauldron that reads NO seed data; until
//! REL-3.S2 (seeds only in `cauldron-build`) lands, every such cauldron here is
//! opened with an EMPTY `--seeds` directory.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use tui::cauldron::app::{App, Focus, Modal};
use tui::cauldron::render_to_text;
use tui::cauldron::store::{seed_dir, read_last_env};
use tui::cauldron::view::FIRST_RUN_ACTION;

fn scratch() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "first-run-v1-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// A seed directory with NO demo data: only the ripple catalog
/// (`ripples.json`), which is product policy every cauldron needs —
/// `ripples::Store::open` refuses to open without it, so a literally empty
/// directory cannot open any cauldron (measured: `read …/ripples.json: No such
/// file or directory`).
fn empty_seeds(root: &Path) -> PathBuf {
    let d = root.join("seeds-empty");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::copy(seed_dir().join("ripples.json"), d.join("ripples.json")).unwrap();
    d
}

fn env_in(root: &Path, name: &str) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: name.into(), name_source: Source::Default, data_root: root.join("data").join(name) }
}

/// The BUILT face, `--cauldron` plus `args`, in root `root` (HOME too), with
/// the process environment `vars` and nothing inherited that names one.
fn face(root: &Path, args: &[&str], vars: &[(&str, &str)]) -> (i32, String, String) {
    // MERGE-V1e: through the isolation helper (as MERGE-9b did for REL-3.S2's
    // four); its defaults live in a sibling scratch dir, and HOME and
    // CAULDRON_HOME are still this test's own root.
    let mut c = crate::face::Face::new(&root.with_extension("face"));
    c.arg("--cauldron")
        .args(args)
        .env("HOME", root)
        .env("CAULDRON_HOME", root)
        .env("CAULDRON_TYPE_ONE", "off")
        .env("CAULDRON_OPENER", "none")
        .env_remove("CAULDRON_ENV")
        .env_remove("CAULDRON_ENV_SOURCE")
        .env_remove("ENV")
        .stdin(Stdio::null());
    for (k, v) in vars {
        c.env(k, v);
    }
    let o = c.output().expect("spawn tui");
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn type_text(a: &mut App, s: &str) {
    for c in s.chars() {
        a.handle(key(KeyCode::Char(c)));
    }
}

/// The environment the top bar names: the first word of the environment
/// bar, the row its dropdown's `▾` is on (V1.1: row 1, under the global bar).
fn top_env(frame: &str) -> String {
    frame.lines().find(|l| l.contains('▾')).unwrap_or("").split_whitespace().next().unwrap_or("").to_string()
}

/// G2's predicate: every non-blank row of `frame` that, with the action's own
/// words taken out, still holds FOUR OR MORE words — a sentence. Border and
/// box-drawing glyphs are not words.
fn prose_rows(frame: &str) -> Vec<String> {
    frame
        .lines()
        .filter(|l| {
            let rest = l.replace(FIRST_RUN_ACTION, " ");
            rest.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric)).count() >= 4
        })
        .map(str::to_string)
        .collect()
}

// ── G1 + G2 + G4 + G6: the first-run screen on an empty root ────────────────

/// **G1, G2, G4, G6.** An empty root: `--dump` exits 0 (G6) and prints a frame
/// whose ONLY text is the action (G1), with no sentence anywhere (G2); and
/// nothing is made, `data/prod` least of all (G4). The same for a NAMED
/// environment in an empty root (`--env alpha`, `$CAULDRON_ENV=prod`): the
/// first-run screen, never `no environment (none yet)`.
#[test]
fn an_empty_root_dumps_the_first_run_screen_with_one_action_and_makes_nothing() {
    let root = scratch();
    for (args, vars) in [
        (vec!["--dump", "--size", "120x40"], vec![]),
        (vec!["--dump", "--size", "120x40", "--env", "alpha"], vec![]),
        (vec!["--dump", "--size", "120x40"], vec![("CAULDRON_ENV", "prod")]),
    ] {
        let (rc, frame, err) = face(&root, &args, &vars);
        assert_eq!(rc, 0, "G6: {args:?} {vars:?} exits 0 on an empty root: {err}");
        println!("── first-run frame, {args:?} {vars:?} ──\n{frame}");
        assert!(prose_rows(&frame).is_empty(), "G2: no sentence: {:?}", prose_rows(&frame));
        let text: Vec<&str> = frame.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        assert_eq!(text, vec![FIRST_RUN_ACTION], "G1: the action is the frame's only text: {frame}");
    }
    assert!(!root.join("data").join("prod").exists(), "G4: no data/prod was made");
    assert!(!root.join("data").join("alpha").exists(), "G4: no data/alpha was made by naming it");
    assert!(env_in(&root, "prod").environments().unwrap().is_empty(), "G4: no environment at all");
}

/// **G1, in-process: the action is the ONLY selectable thing.** The frame
/// records no click target but the action's own rect.
#[test]
fn the_first_run_screen_has_no_click_target_but_the_action() {
    let root = scratch();
    let mut a = App::open_first_run(env_in(&root, "prod"), empty_seeds(&root), true).unwrap();
    let frame = render_to_text(&mut a, 120, 40).unwrap().join("\n");
    assert!(frame.contains(FIRST_RUN_ACTION), "{frame}");
    assert!(a.hits.is_empty(), "G1: no hit region but the action: {:?}", a.hits);
    assert!(a.first_run_at.width as usize >= FIRST_RUN_ACTION.chars().count(), "the action's rect was recorded: {:?}", a.first_run_at);
    // G4, in-process: the live first-run App wrote nothing.
    assert!(env_in(&root, "prod").environments().unwrap().is_empty(), "G4: opening the first-run screen made an environment");
    assert!(!root.join("data").join("prod").exists(), "G4: data/prod");
}

/// **G1 opposition.** With one environment, the normal face opens on it: the
/// action is absent and the top bar names the environment.
#[test]
fn with_one_environment_the_normal_face_opens_and_the_action_is_absent() {
    let root = scratch();
    std::fs::create_dir_all(root.join("data").join("alpha")).unwrap();
    let seeds = empty_seeds(&root);
    let (rc, frame, err) = face(&root, &["--dump", "--size", "120x40", "--seeds", seeds.to_str().unwrap()], &[]);
    assert_eq!(rc, 0, "{err}");
    assert!(!frame.contains("create your first"), "G1 opposition: {frame}");
    assert_eq!(top_env(&frame), "alpha", "{frame}");
}

/// **G2 opposition: the predicate can fire.** A welcome sentence is prose; the
/// action alone, and chrome, are not.
#[test]
fn the_no_prose_predicate_fires_on_a_sentence_and_not_on_the_action() {
    assert!(prose_rows(&format!("    {FIRST_RUN_ACTION}    ")).is_empty());
    assert!(prose_rows("╭────────╮\n│        │\n╰────────╯").is_empty());
    assert_eq!(prose_rows("  welcome to cauldron, make one  ").len(), 1);
    assert_eq!(prose_rows(&format!("{FIRST_RUN_ACTION} to begin using it")).len(), 1);
}

// ── G3 + G4 opposition: the first cauldron is made, landed on, remembered ───

/// **G3.** Enter on the action, `alpha`, Enter: `alpha` is made through the
/// path `+ new` takes, the face is IN it, the dropdown lists it, and it is the
/// last opened. A second launch naming nothing opens `alpha` — even with
/// `prod` and `beta` there, which the defaults would otherwise pick.
/// **G4 opposition:** `data/alpha` exists, so G4's check can see a made env.
#[test]
fn the_first_cauldron_is_made_landed_on_and_opened_next_time() {
    let root = scratch();
    let seeds = empty_seeds(&root);
    let mut a = App::open_first_run(env_in(&root, "prod"), seeds.clone(), true).unwrap();
    a.handle(key(KeyCode::Enter));
    assert!(matches!(a.modal, Some(Modal::NewCauldron { .. })), "Enter opens the naming modal");
    // Esc goes back to the one action, not to a dropdown of no cauldrons.
    a.handle(key(KeyCode::Esc));
    assert!(a.modal.is_none() && a.dropdown.is_none() && a.first_run.is_some());
    a.handle(key(KeyCode::Enter));
    type_text(&mut a, "alpha");
    a.handle(key(KeyCode::Enter));
    assert!(a.first_run.is_none(), "the first-run screen is gone: {:?}", a.modal);
    assert_eq!(a.env.name, "alpha");
    assert!(a.persist, "the face is live in it");
    assert!(root.join("data").join("alpha").is_dir(), "G4 opposition: data/alpha was made");
    assert!(!root.join("data").join("prod").exists(), "G4: and prod was not");
    a.open_cauldrons();
    let listed: Vec<String> = a.dropdown.as_ref().unwrap().entries.iter().map(|e| e.0.clone()).collect();
    assert_eq!(listed, vec!["alpha".to_string()], "the dropdown shows alpha");
    assert_eq!(read_last_env(&a.env).unwrap().as_deref(), Some("alpha"), "alpha is the last opened");
    drop(a);

    // Two more environments, each of which a default would prefer to alpha
    // (prod by name, beta... by nothing — but prod is there).
    std::fs::create_dir_all(root.join("data").join("prod")).unwrap();
    std::fs::create_dir_all(root.join("data").join("beta")).unwrap();
    let s = seeds.to_str().unwrap();
    let (rc, frame, err) = face(&root, &["--dump", "--size", "120x40", "--seeds", s], &[]);
    assert_eq!(rc, 0, "{err}");
    println!("── second launch, no name ──\n{frame}");
    assert_eq!(top_env(&frame), "alpha", "G3: a second launch naming nothing opens the last opened: {frame}");
    // `cauldron tui` hands the face CAULDRON_ENV=prod and says it was the
    // DEFAULT: that is no name either.
    let (rc, frame, err) = face(&root, &["--dump", "--seeds", s], &[("CAULDRON_ENV", "prod"), ("CAULDRON_ENV_SOURCE", "default")]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(top_env(&frame), "alpha", "G3: the router's defaulted prod is not a name: {frame}");
    // G3 opposition: a name given wins over the last opened.
    let (rc, frame, err) = face(&root, &["--dump", "--seeds", s], &[("CAULDRON_ENV", "beta")]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(top_env(&frame), "beta", "G3 opposition: $CAULDRON_ENV wins: {frame}");
    let (rc, frame, err) = face(&root, &["--dump", "--seeds", s], &[("CAULDRON_ENV", "prod"), ("CAULDRON_ENV_SOURCE", "$CAULDRON_ENV")]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(top_env(&frame), "prod", "G3 opposition: a prod he named through the router wins: {frame}");
    let (rc, frame, err) = face(&root, &["--dump", "--seeds", s, "--env", "beta"], &[]);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(top_env(&frame), "beta", "G3 opposition: --env wins: {frame}");
    // --dump records nothing: alpha is still the last opened.
    assert_eq!(read_last_env(&env_in(&root, "alpha")).unwrap().as_deref(), Some("alpha"));
}

/// **--dump makes nothing from the first-run screen either.** Driving the
/// modal to Enter refuses, and says why.
#[test]
fn a_dump_driving_the_first_run_modal_makes_nothing() {
    let root = scratch();
    let (rc, frame, err) = face(&root, &["--dump", "--size", "120x40", "--keys", "enter a l p h a enter"], &[]);
    assert_eq!(rc, 0, "{err}");
    assert!(frame.contains("nothing is created by --dump"), "{frame}");
    assert!(!root.join("data").join("alpha").exists());
}

// ── G5: the first cluster ───────────────────────────────────────────────────

/// **G5.** A cauldron with no cluster (empty seeds) opens with the root `+`
/// selected and the focus on it; Enter opens the new-cluster modal at the
/// root. Its frame from the built face is printed for the report.
#[test]
fn a_cauldron_with_no_cluster_opens_on_the_root_plus() {
    let root = scratch();
    let seeds = empty_seeds(&root);
    std::fs::create_dir_all(root.join("data").join("alpha")).unwrap();
    let mut a = App::open(env_in(&root, "alpha"), seeds.clone(), true).unwrap();
    assert!(tui::cauldron::store::has_no_cluster(&seeds, "alpha", &tui::cauldron::store::soup_dir(&a.env).unwrap()), "the cauldron has no cluster of its own");
    assert_eq!(a.forest.clusters.iter().filter(|g| !g.whole).count(), 1, "only the placeholder named after it");
    assert!(a.on_add_root, "G5: the root + is selected");
    assert_eq!(a.focus, Focus::Tree, "G5: the focus is on it, not the bucket");
    a.handle(key(KeyCode::Enter));
    assert!(matches!(a.modal, Some(Modal::NewCluster { parent: None, .. })), "G5: Enter names the first cluster: {:?}", a.modal);
    let (rc, frame, err) = face(&root, &["--dump", "--env", "alpha", "--seeds", seeds.to_str().unwrap(), "--size", "120x40"], &[]);
    assert_eq!(rc, 0, "{err}");
    println!("── first-cluster frame ──\n{frame}");
    assert_eq!(top_env(&frame), "alpha");
    assert!(!prose_rows(&frame).iter().any(|l| l.contains("cluster") && l.contains("first")), "no first-cluster sentence");
}

/// **G5 opposition.** A cauldron WITH clusters (the repo's prod seed) opens on
/// its first cluster, not on `+`.
#[test]
fn a_cauldron_with_clusters_opens_on_a_cluster_not_the_plus() {
    let root = scratch();
    std::fs::create_dir_all(root.join("data").join("prod")).unwrap();
    let a = App::open(env_in(&root, "prod"), seed_dir(), false).unwrap();
    assert!(!tui::cauldron::store::has_no_cluster(&seed_dir(), "prod", &tui::cauldron::store::soup_dir(&a.env).unwrap()), "the seed has clusters");
    assert!(!a.on_add_root, "G5 opposition: not on +");
    assert!(!a.forest.get(a.selected).whole, "on a cluster of its own");
}

// ── G6 opposition: the typo guard survives ──────────────────────────────────

/// **G6 opposition.** `--dump --env nosuch` with another environment there is
/// still refused, naming the ones there are (audit 0118 F13); and `prod` is no
/// longer exempt from that (V1.2: never made behind his back).
#[test]
fn a_named_missing_environment_is_still_refused_when_others_exist() {
    let root = scratch();
    std::fs::create_dir_all(root.join("data").join("dev")).unwrap();
    for args in [vec!["--dump", "--env", "nosuch"], vec!["--dump", "--env", "prod"]] {
        let (rc, out, err) = face(&root, &args, &[]);
        assert_ne!(rc, 0, "{args:?}: {out}");
        let name = args[2];
        assert!(err.contains(&format!("no environment `{name}` (dev)")), "{args:?}: {err}");
    }
    assert!(!root.join("data").join("nosuch").exists() && !root.join("data").join("prod").exists(), "nothing was made");
}

/// **G5 opposition, without a seed.** A cauldron with no seed but ONE cluster
/// he made opens on that cluster — the predicate is "no cluster of his", not
/// "no seed".
#[test]
fn a_cauldron_with_one_made_cluster_and_no_seed_opens_on_it() {
    use tui::cauldron::store::{soup_dir, CreatedCluster, CreatedClusters};
    let root = scratch();
    let seeds = empty_seeds(&root);
    std::fs::create_dir_all(root.join("data").join("alpha")).unwrap();
    let env = env_in(&root, "alpha");
    let made: CreatedCluster = serde_json::from_value(serde_json::json!({"id": "X-2", "name": "first", "created_at": "2026-10-30T00:00:00Z"})).unwrap();
    CreatedClusters::new(&soup_dir(&env).unwrap()).add(made, |_| false).unwrap();
    let a = App::open(env, seeds, true).unwrap();
    assert!(!a.on_add_root, "G5 opposition: not on +");
    assert_eq!(a.forest.get(a.selected).key, "X-1", "on the first cluster in the sidebar");
    assert!(a.forest.find("X-2").is_some(), "his cluster is there");
}
