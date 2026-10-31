//! REL-3.S2d: **no missing reference anywhere stops the face opening.**
//!
//! REL-3.S2b and S2c each turned one kind of dangling reference from a
//! refusal into skip-and-say, and each found another. This step audits every
//! place the face's soup/env loaders refused because a referenced id, repo or
//! node was absent, and applies the one rule to all of them: the face opens,
//! the file is kept byte for byte, the reference is skipped in the view and
//! counted in the existing one-line notice.
//!
//! ONE table drives it. Every row is one reference kind, written as one edit
//! of ONE base environment in which every reference resolves. Each row runs
//! the REAL load path (`App::open` over a scratch cauldron) twice:
//! - the base (the opposed arm): opens, nothing is stale, nothing is said,
//!   and the reference SHOWS in the view;
//! - the base with that one edit: opens, exactly ONE stale line naming that
//!   reference, the notice says it, and the reference does NOT show.
//! So neither a constant notice, a constant silence, nor a constant skip
//! passes, and restoring any one refusal turns its row red (the open fails).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use env::paths::Source;
use env::Environment;
use tui::cauldron::app::App;
use tui::cauldron::store;

/// The base environment: every reference in it resolves. Sections: `soup`
/// (the cauldron's own seed, `seed/clusters.json`), `made` (clusters made
/// here, `clusters.json`), `bp` (the blueprint seed), `reg` (the global repo
/// registry), `tickets` (`tickets.json`).
const BASE: &str = r#"{
"soup": {"clusters": [
  {"id": "X-1", "name": "A", "description": "",
   "regions": ["R-c"],
   "links": ["Q-2", "T-2"],
   "repos": [{"name": "r", "focus": ["MOD-a-00000000"]}],
   "directives": [{"id": "DIR-1", "text": "t", "cite": "x#1", "region": "R-d"}],
   "objectives": [{"serial": 1, "title": "o", "serves": "DIR-1"}],
   "tasks": [
     {"serial": 1, "title": "t", "state": "blocked", "region": "R-t", "depends_on": [3], "waits_on": ["T-2"], "blocked_by": ["Q-1"],
      "edges": [{"kind": "builds", "to": "MOD-a-00000000"}]},
     {"serial": 3, "title": "u", "state": "ready"}],
   "milestones": [
     {"serial": 1, "handle": "a", "objective": 1, "tasks": [1], "depends_on": ["M-2", "T-3", "Q-1"], "region": "R-m"},
     {"serial": 2, "handle": "b", "objective": 1}],
   "questions": [{"serial": 1, "value": 1, "question": "q", "why": "w", "options": [], "recommendation": "r", "region": "R-q", "gates": "T-1"}],
   "human_actions": [{"serial": 1, "ask": "a", "status": "open", "filed": "2026-10-30", "raised_by": "Q-1", "blocks": ["T-3"], "region": "R-h"}]},
  {"id": "X-2", "name": "B", "description": "",
   "questions": [{"serial": 2, "value": 1, "question": "q2", "why": "w", "options": [], "recommendation": "r"}],
   "tasks": [{"serial": 2, "title": "b", "state": "ready"}]}],
 "corpus": {"schema_version": 5,
   "regions": [
     {"id": "R-c", "handle": "h", "boundary": "b"}, {"id": "R-d", "handle": "h", "boundary": "b"},
     {"id": "R-t", "handle": "h", "boundary": "b"}, {"id": "R-m", "handle": "h", "boundary": "b"},
     {"id": "R-q", "handle": "h", "boundary": "b"}, {"id": "R-h", "handle": "h", "boundary": "b"},
     {"id": "R-n", "handle": "h", "boundary": "b"}, {"id": "R-p", "handle": "h", "boundary": "b", "narrows": ["R-c"]}],
   "nodes": [{"id": "IS-1", "type": "issue", "handle": "h", "in_region": ["R-n"]}]}},
"made": [{"id": "X-90", "name": "Made here", "created_at": "2026-10-30T10:00:00Z", "parent": "X-2"}],
"bp": {"repos": [
  {"name": "r", "path": "/r", "branch": "main", "head": "0", "blueprint": {"schema_version": 1, "nodes": [
    {"id": "MOD-a-00000000", "type": "module", "state": "draft", "handle": "h"},
    {"id": "MOD-b-11111111", "type": "module", "state": "draft", "handle": "h", "part_of": "MOD-a-00000000",
     "edges": [{"kind": "owns", "to": "MOD-a-00000000"}]}]}},
  {"name": "r2", "path": "/r2", "branch": "main", "head": "0", "blueprint": {"schema_version": 1, "nodes": []}}]},
"reg": {"unregistered": ["r2"]},
"tickets": {"done": ["X-1/HA-1/step"], "submitted": []}
}"#;

/// One reference kind: its name, the one edit that makes it dangle, the
/// text its stale line carries, and which part of the notice counts it.
struct Row {
    kind: &'static str,
    from: &'static str,
    to: &'static str,
    why: &'static str,
    said: &'static str,
}

const REF: &str = "1 reference to an item that no longer exists";
const LINK: &str = "1 cluster link to a repo no longer exists";

const ROWS: &[Row] = &[
    // corpus.rs `Corpus::build`
    Row { kind: "region narrows a region", from: r#""narrows": ["R-c"]"#, to: r#""narrows": ["R-9"]"#, why: "region `R-p` names region `R-9`", said: REF },
    Row { kind: "node attests a region", from: r#""in_region": ["R-n"]"#, to: r#""in_region": ["R-9"]"#, why: "node `IS-1` names region `R-9`", said: REF },
    Row { kind: "cluster claims a region", from: r#""regions": ["R-c"]"#, to: r#""regions": ["R-9"]"#, why: "cluster `X-1` names region `R-9`", said: REF },
    // model.rs `Soup::from_file`
    Row { kind: "directive cites a region", from: r#""region": "R-d""#, to: r#""region": "R-9""#, why: "cites region `R-9`", said: REF },
    Row { kind: "task cites a region", from: r#""region": "R-t""#, to: r#""region": "R-9""#, why: "cites region `R-9`", said: REF },
    Row { kind: "milestone cites a region", from: r#""region": "R-m""#, to: r#""region": "R-9""#, why: "cites region `R-9`", said: REF },
    Row { kind: "question cites a region", from: r#""region": "R-q""#, to: r#""region": "R-9""#, why: "cites region `R-9`", said: REF },
    Row { kind: "human action cites a region", from: r#""region": "R-h""#, to: r#""region": "R-9""#, why: "cites region `R-9`", said: REF },
    // model.rs `attach`
    Row { kind: "cluster made under a cluster", from: r#""parent": "X-2""#, to: r#""parent": "X-77""#, why: "`X-90` was made under `X-77`", said: REF },
    // model.rs `Forest::from_specs`
    Row { kind: "task waits on a task", from: r#""waits_on": ["T-2"]"#, to: r#""waits_on": ["T-9"]"#, why: "waits on `T-9`, which is in no cluster", said: REF },
    Row { kind: "cluster links a question", from: r#""links": ["Q-2""#, to: r#""links": ["Q-9""#, why: "links `Q-9`", said: REF },
    Row { kind: "cluster links a task", from: r#", "T-2"]"#, to: r#", "T-9"]"#, why: "links `T-9`", said: REF },
    // model.rs `Forest::insert`
    Row { kind: "task depends on a task", from: r#""depends_on": [3]"#, to: r#""depends_on": [9]"#, why: "depends on T-9", said: REF },
    // model.rs `validate_plan`
    Row { kind: "objective serves a directive", from: r#""serves": "DIR-1""#, to: r#""serves": "DIR-9""#, why: "serves `DIR-9`", said: REF },
    Row { kind: "milestone serves an objective", from: r#""handle": "a", "objective": 1"#, to: r#""handle": "a", "objective": 9"#, why: "serves OBJ-9", said: REF },
    Row { kind: "milestone holds a task", from: r#""tasks": [1]"#, to: r#""tasks": [9]"#, why: "holds T-9", said: REF },
    Row { kind: "milestone depends on a milestone", from: r#"["M-2""#, to: r#"["M-9""#, why: "depends on `M-9`", said: REF },
    Row { kind: "milestone depends on a task", from: r#""T-3", "Q-1"]"#, to: r#""T-9", "Q-1"]"#, why: "depends on `T-9`", said: REF },
    Row { kind: "milestone depends on a question", from: r#""Q-1"], "region""#, to: r#""Q-9"], "region""#, why: "depends on `Q-9`", said: REF },
    Row { kind: "task blocked by an item", from: r#""blocked_by": ["Q-1"]"#, to: r#""blocked_by": ["Q-9"]"#, why: "T-1 names `Q-9`", said: REF },
    Row { kind: "human action raised by an item", from: r#""raised_by": "Q-1""#, to: r#""raised_by": "Q-9""#, why: "HA-1 names `Q-9`", said: REF },
    Row { kind: "human action blocks an item", from: r#""blocks": ["T-3"]"#, to: r#""blocks": ["T-9"]"#, why: "HA-1 names `T-9`", said: REF },
    Row { kind: "question gates an item", from: r#""gates": "T-1""#, to: r#""gates": "T-9""#, why: "Q-1 names `T-9`", said: REF },
    // blueprint.rs `Repos::build_with`
    Row { kind: "blueprint node edge", from: r#""kind": "owns", "to": "MOD-a-00000000""#, to: r#""kind": "owns", "to": "MOD-z-99999999""#, why: "`MOD-b-11111111` points at `MOD-z-99999999`", said: REF },
    Row { kind: "blueprint node part_of", from: r#""part_of": "MOD-a-00000000""#, to: r#""part_of": "MOD-z-99999999""#, why: "`MOD-b-11111111` points at `MOD-z-99999999`", said: REF },
    // REL-3.S2c, kept in the same table
    Row { kind: "cluster claims a repo", from: r#"{"name": "r", "focus""#, to: r#"{"name": "ghost", "focus""#, why: "claims repo `ghost`", said: LINK },
    Row { kind: "claim focuses a node", from: r#""focus": ["MOD-a-00000000"]"#, to: r#""focus": ["MOD-z-99999999"]"#, why: "focuses `MOD-z-99999999`", said: LINK },
    Row { kind: "task edge to a node", from: r#""kind": "builds", "to": "MOD-a-00000000""#, to: r#""kind": "builds", "to": "MOD-z-99999999""#, why: "edge to `MOD-z-99999999`", said: LINK },
    // REL-3.S2b, kept in the same table
    Row { kind: "registry unregisters a repo", from: r#""unregistered": ["r2"]"#, to: r#""unregistered": ["r9"]"#, why: "r9", said: "1 unregistered repo no longer exists" },
    Row { kind: "ticket names a human action", from: r#""X-1/HA-1/step""#, to: r#""X-1/HA-9/step""#, why: "X-1/HA-9/step", said: "1 ticket refers to an item that no longer exists" },
];

fn scratch_root() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "rel3s2d-{}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn env_in(root: &Path) -> Environment {
    Environment { root: root.to_path_buf(), root_source: Source::CauldronHome, name: "prod".into(), name_source: Source::Default, data_root: root.join("data").join("prod") }
}

/// Write `doc` as a scratch prod cauldron under `root`. Returns the files
/// written, and the seeds dir (`tui-no-seeds`, what a real face reads).
fn write_env(root: &Path, doc: &str) -> (Vec<PathBuf>, PathBuf) {
    let v: serde_json::Value = serde_json::from_str(doc).unwrap();
    let env = env_in(root);
    let soup = store::soup_dir(&env).unwrap();
    let seeds = store::no_seeds(&env);
    let global = store::global_dir(&env);
    let files = [
        (soup.join("seed").join("clusters.json"), v["soup"].clone()),
        (soup.join("clusters.json"), serde_json::json!({"clusters": v["made"]})),
        (seeds.join("blueprints.json"), v["bp"].clone()),
        (global.join("repos.json"), v["reg"].clone()),
        (soup.join("tickets.json"), v["tickets"].clone()),
    ];
    let mut out = Vec::new();
    for (p, j) in files {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, serde_json::to_string_pretty(&j).unwrap()).unwrap();
        out.push(p);
    }
    (out, seeds)
}

fn open(root: &Path, seeds: &Path, persist: bool) -> anyhow::Result<App> {
    std::env::set_var("CAULDRON_TYPE_ONE", "off");
    App::open(env_in(root), seeds.to_path_buf(), persist)
}

/// Every stale line the loaders recorded, from every source the notice
/// counts.
fn stale_lines(a: &App) -> Vec<String> {
    let mut v = a.forest.stale.clone();
    v.extend(a.corpus.stale.iter().cloned());
    v.extend(a.repos.stale_refs.iter().cloned());
    v.extend(a.repos.stale_links.iter().cloned());
    v.extend(a.repos.stale_unregistered.iter().cloned());
    v.extend(a.tickets.unknown(&a.forest));
    v
}

/// Whether the row's reference SHOWS in the view: what the view reads.
fn shown(a: &App, kind: &str) -> bool {
    let f = &a.forest;
    let c = &a.corpus;
    let x1 = f.find("X-1").expect("X-1 is always there");
    let g = f.get(x1);
    let r = |id: &str| c.find_region(id);
    let repo = a.repos.find_repo("r");
    match kind {
        "region narrows a region" => r("R-p").is_some_and(|p| !c.regions[p].narrows.is_empty()),
        "node attests a region" => c.nodes.iter().any(|n| n.id == "IS-1" && !n.in_region.is_empty()),
        "cluster claims a region" => !c.claims[x1].is_empty(),
        "directive cites a region" => g.directives[0].region.is_some(),
        "task cites a region" => g.tasks[0].region.is_some(),
        "milestone cites a region" => g.milestones[0].region.is_some(),
        "question cites a region" => g.questions[0].region.is_some(),
        "human action cites a region" => g.human_actions[0].region.is_some(),
        "cluster made under a cluster" => f.find("X-90").is_some(),
        "task waits on a task" => !f.waits(&[x1], false).is_empty(),
        "cluster links a question" => !f.linked_questions(x1).is_empty(),
        "cluster links a task" => !f.linked_tasks(x1).is_empty(),
        "task depends on a task" => !g.tasks[0].depends_on.is_empty(),
        "objective serves a directive" => g.objectives.iter().any(|o| g.directives.iter().any(|d| d.id == o.serves)),
        "milestone serves an objective" => g.milestone(1).is_some_and(|m| g.objective(m.objective).is_some()),
        "milestone holds a task" => !g.milestones[0].tasks.is_empty(),
        "milestone depends on a milestone" => g.milestones[0].depends_on.iter().any(|d| d.starts_with("M-")),
        "milestone depends on a task" => g.milestones[0].depends_on.iter().any(|d| d.starts_with("T-")),
        "milestone depends on a question" => g.milestones[0].depends_on.iter().any(|d| d.starts_with("Q-")),
        "task blocked by an item" => !g.tasks[0].blocked_by.is_empty(),
        "human action raised by an item" => g.human_actions[0].raised_by.is_some(),
        "human action blocks an item" => !g.human_actions[0].blocks.is_empty(),
        "question gates an item" => g.questions[0].gates.is_some(),
        "blueprint node edge" => repo.is_some_and(|ri| a.repos.repos[ri].nodes.iter().any(|n| !n.edges.is_empty())),
        "blueprint node part_of" => repo.is_some_and(|ri| a.repos.repos[ri].nodes.iter().any(|n| n.part_of.is_some())),
        "cluster claims a repo" => !a.repos.claims[x1].is_empty(),
        "claim focuses a node" => a.repos.claims[x1].first().is_some_and(|cl| !cl.focus.is_empty()),
        "task edge to a node" => a.repos.locate(&g.tasks[0].edges[0].to).is_some(),
        "registry unregisters a repo" => a.repos.find_repo("r2").is_none(),
        "ticket names a human action" => a.tickets.is_done("X-1", 1, "step"),
        other => panic!("no view check for row `{other}`"),
    }
}

/// THE table: every reference kind, each against its resolving arm.
#[test]
fn every_dangling_reference_kind_opens_skips_and_is_counted_against_its_resolving_arm() {
    // The resolving arm: everything lands, nothing is said, everything shows.
    let root = scratch_root();
    let (_, seeds) = write_env(&root, BASE);
    let a = open(&root, &seeds, false).expect("the base environment opens");
    assert!(stale_lines(&a).is_empty(), "the base dangles nowhere: {:?}", stale_lines(&a));
    assert_eq!(a.stale_notice(), None, "nothing is said when nothing dangles");
    for row in ROWS {
        assert!(shown(&a, row.kind), "{}: the resolving arm shows the reference", row.kind);
    }
    // Each kind, alone.
    let mut red = Vec::new();
    for row in ROWS {
        assert_eq!(BASE.matches(row.from).count(), 1, "{}: the edit `{}` must hit exactly once", row.kind, row.from);
        let doc = BASE.replacen(row.from, row.to, 1);
        let root = scratch_root();
        let (files, seeds) = write_env(&root, &doc);
        let before: Vec<Vec<u8>> = files.iter().map(|p| std::fs::read(p).unwrap()).collect();
        let mut a = match open(&root, &seeds, true) {
            Ok(a) => a,
            Err(e) => {
                red.push(format!("{}: REFUSED TO OPEN: {e:#}", row.kind));
                continue;
            }
        };
        let lines = stale_lines(&a);
        if lines.len() != 1 || !lines[0].contains(row.why) {
            red.push(format!("{}: want one stale line with `{}`, got {lines:?}", row.kind, row.why));
        }
        if a.stale_notice().as_deref() != Some(row.said) {
            red.push(format!("{}: want the notice `{}`, got {:?}", row.kind, row.said, a.stale_notice()));
        }
        if !a.notice.as_deref().unwrap_or("").contains(row.said) {
            red.push(format!("{}: the face's notice does not say it: {:?}", row.kind, a.notice));
        }
        if a.error.is_some() {
            red.push(format!("{}: a notice, not an error: {:?}", row.kind, a.error));
        }
        if shown(&a, row.kind) {
            red.push(format!("{}: the dangling reference still shows in the view", row.kind));
        }
        a.sync();
        if a.error.is_some() {
            red.push(format!("{}: a re-read errs: {:?}", row.kind, a.error));
        }
        let after: Vec<Vec<u8>> = files.iter().map(|p| std::fs::read(p).unwrap()).collect();
        if after != before {
            red.push(format!("{}: a file was rewritten; it must be kept byte for byte", row.kind));
        }
    }
    assert!(red.is_empty(), "REL-3.S2d: {} of {} reference kinds fail:\n{}", red.len(), ROWS.len(), red.join("\n"));
}

/// Every kind at once: the face opens, ONE line with every count, and the
/// files are untouched through a re-read. A made cluster that was skipped
/// keeps its id taken.
#[test]
fn an_environment_where_every_reference_kind_dangles_opens_with_one_notice() {
    let mut doc = BASE.to_string();
    for row in ROWS {
        doc = doc.replacen(row.from, row.to, 1);
    }
    let root = scratch_root();
    let (files, seeds) = write_env(&root, &doc);
    let before: Vec<Vec<u8>> = files.iter().map(|p| std::fs::read(p).unwrap()).collect();
    let mut a = open(&root, &seeds, true).expect("REL-3.S2d: no missing reference stops the face opening");
    let refs = ROWS.iter().filter(|r| r.said == REF).count();
    // Two of the three repo-link rows: once the claim names `ghost`, its
    // focus is never looked at (the claim is skipped whole).
    let want = format!(
        "1 ticket refers to an item that no longer exists · 1 unregistered repo no longer exists · 2 cluster links to repos no longer exist · {refs} references to items that no longer exist"
    );
    assert_eq!(a.stale_notice().as_deref(), Some(want.as_str()), "{:?}", stale_lines(&a));
    assert!(a.notice.as_deref().unwrap_or("").contains(&want), "{:?}", a.notice);
    assert!(a.error.is_none(), "{:?}", a.error);
    a.sync();
    assert!(a.error.is_none(), "{:?}", a.error);
    let after: Vec<Vec<u8>> = files.iter().map(|p| std::fs::read(p).unwrap()).collect();
    assert_eq!(after, before, "every file kept byte for byte");
    assert_eq!(a.forest.skipped, vec!["X-90".to_string()]);
    assert_eq!(store::mint_cluster_id(&a.forest), "X-91", "the skipped cluster's id stays taken");
    // Rendered (the first frame) without a panic, the notice in it.
    let s = tui::cauldron::render_to_text(&mut a, 300, 50).unwrap().join("\n");
    assert!(s.contains("references to items that no longer exist"), "{s}");
}

/// The BUILT face, headless, on a scratch HOME whose prod soup dangles every
/// kind of reference: `rc 0` and the notice.
#[test]
fn the_built_face_opens_an_environment_where_every_reference_dangles_rc_0() {
    let mut doc = BASE.to_string();
    for row in ROWS {
        doc = doc.replacen(row.from, row.to, 1);
    }
    let home = scratch_root();
    let root = home.join(".cauldron");
    let (files, _) = write_env(&root, &doc);
    let before: Vec<Vec<u8>> = files.iter().map(|p| std::fs::read(p).unwrap()).collect();
    // MERGE-9b: through the isolation helper (its defaults live in a
    // sibling scratch dir); HOME and CAULDRON_HOME are still this test's own.
    let o = crate::face::Face::new(&home.with_extension("face"))
        .args(["--cauldron", "--dump", "--env", "prod", "--size", "200x48"])
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
    assert!(out.contains("references to items that no longer exist"), "the first frame says it:\n{out}");
    let after: Vec<Vec<u8>> = files.iter().map(|p| std::fs::read(p).unwrap()).collect();
    assert_eq!(after, before, "kept on disk");
}
