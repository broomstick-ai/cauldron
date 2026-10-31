//! A cauldron that carries its OWN seed (`<data_root>/tui/soup/seed/`, task
//! 0168), written into a scratch root: the shape the design cauldron has
//! (8e4fca10 msg 186), small enough to read. Shared by the tests that prove
//! the own seed and by the sweeps that hold every screen of it to the laws.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// The real design cauldron's own seed on this machine, if it has been made.
pub fn real_design_seed() -> Option<PathBuf> {
    let home = std::env::var_os("CAULDRON_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cauldron")))?;
    let dir = home.join("data/design/tui/soup/seed");
    dir.join("clusters.json").is_file().then_some(dir)
}

/// Copy the real design seed into `root`'s environment `env`, so a test reads
/// it without ever writing the real one. Its artifacts stay where they are.
pub fn copy_real_design_seed(from: &Path, root: &Path, env: &str) {
    let to = seed_dir(root, env);
    std::fs::create_dir_all(&to).unwrap();
    for f in ["clusters.json", "artifacts.json", "ripples.json"] {
        if from.join(f).is_file() {
            std::fs::copy(from.join(f), to.join(f)).unwrap();
        }
    }
}

/// `<root>/data/<env>/tui/soup/seed`.
pub fn seed_dir(root: &Path, env: &str) -> PathBuf {
    root.join("data").join(env).join("tui").join("soup").join("seed")
}

/// A line wider than any modal: the System mockups are 120 columns.
pub const WIDE: &str = "│ ▾ DUMP RECEIVED ──────────────────────────────────────────────────────────────────────────────────────── user_input  │";

/// Write the fixture into `root`'s environment `env`: a root cluster and a
/// System child whose first question carries two inserted artifacts (one by
/// id, one by path), an assumed question, two services; plus its own
/// artifacts seed. Returns the artifact files' directory.
pub fn write(root: &Path, env: &str) -> PathBuf {
    let seed = seed_dir(root, env);
    let arts = root.join("data").join(env).join("artifacts");
    std::fs::create_dir_all(&seed).unwrap();
    std::fs::create_dir_all(&arts).unwrap();
    let mock = arts.join("system-root-C.txt");
    std::fs::write(&mock, format!("SYSTEM ROOT, SCHEME C\n\n{WIDE}\n│ ▌   ──● intake ▸            aggregate   where cluster ≠ none │\n")).unwrap();
    let flow = arts.join("system-flow-2.txt");
    std::fs::write(&flow, "SPLASH FLOW 2\n\n○ load_intake\n│ itemize\n● receipt\n").unwrap();
    let clusters = serde_json::json!({
        "_note": "own-seed fixture",
        "clusters": [{
            "id": "X-1",
            "name": "Design",
            "description": "The interface's design, asked inside the product.",
            "tasks": [{"serial": 1, "title": "Put every open question here", "state": "done"}],
            "children": [{
                "id": "X-2",
                "name": "System",
                "description": "How the System tab shows the ripples.",
                "regions": ["R-1"],
                "tasks": [{"serial": 2, "title": "Build System on his picks", "state": "blocked", "blocked_by": ["Q-1"]}],
                "questions": [{
                    "serial": 1, "value": 95, "blocking": true,
                    "question": "How should the System tab's top level be organized?",
                    "why": "It decides whether you read the flow at a glance.",
                    "options": ["C — ENTRIES, THEN THE LIBRARY. Reads in your order.", "A — ONE TREE. Five levels deep."],
                    "recommendation": "C. It only ever goes one page deep.",
                    "region": "R-1", "gates": "T-2",
                    "artifacts": ["ART-1", flow.to_string_lossy()]
                }, {
                    "serial": 2, "value": 40,
                    "question": "Should a ripple's page be flat bands?",
                    "why": "Each band header already says its summary.",
                    "options": ["FLAT. Start first.", "TWO PARENTS. Initialization and configuration."],
                    "recommendation": "Flat."
                }],
                "closed_questions": [{
                    "serial": 3, "question": "Where do a ripple's triggers live?", "status": "assumed",
                    "answer": "In the policy. Veto it to change it.", "resolved_by": "Q-30 · built as assumed", "date": "2026-10-21"
                }]
            }]
        }],
        "cauldron": {
            "threads": {"link": "live", "warnings": 0},
            "mesh": [{
                "node": "example-mac", "role": "this machine", "status": "online",
                "load": [1.0, 1.0, 1.0], "cpu_pct": 10, "mem_pct": 50,
                "services": [
                    {"name": "cauldron daemon", "status": "running", "pid": 1, "uptime": "2h", "addr": "127.0.0.1:1", "version": "be1150ac", "warnings": 0, "errors": 0, "log": "/tmp/d.log"},
                    {"name": "threads", "status": "running", "pid": 2, "uptime": "2h", "addr": "127.0.0.1:2", "version": "8299dcc1", "warnings": 0, "errors": 0, "log": "/tmp/t.log"}
                ],
                "last_seen": "now"
            }]
        },
        "corpus": {
            "schema_version": 5,
            "regions": [{"id": "R-1", "handle": "SYSTEM - the System tab.", "boundary": "Inside: how ripples are shown."}],
            "nodes": [{"id": "DEC-1", "type": "decision", "state": "decided", "handle": "A ripple made of ripples is a SPLASH.", "body": "msg 186.", "in_region": ["R-1"]}]
        }
    });
    std::fs::write(seed.join("clusters.json"), serde_json::to_string_pretty(&clusters).unwrap()).unwrap();
    let artifacts = serde_json::json!({
        "_note": "own-seed fixture",
        "directories": [{"name": "design artifacts", "path": arts.to_string_lossy(), "claimed_by": ["X-1"]}],
        "worktrees": [],
        "artifacts": [{
            "id": "ART-1", "handle": "System root, scheme C", "kind": "design", "location": mock.to_string_lossy(),
            "made_by": "0168-task-build-b4-design-cauldron", "for": "Q-1", "created": "2026-10-25T06:00:00Z", "regions": ["R-1"]
        }]
    });
    std::fs::write(seed.join("artifacts.json"), serde_json::to_string_pretty(&artifacts).unwrap()).unwrap();
    arts
}

/// The fixture as a seed written BEFORE msg 235 — `seed/surfaces.json`, a
/// SLUG id per surface, serials per surface, a link as `{surface, id}`, the
/// artifacts claimed and `for` by slug — the shape the design cauldron's
/// writer wrote until task 0203, which the face now refuses.
pub fn write_legacy(root: &Path, env: &str) {
    let seed = seed_dir(root, env);
    std::fs::create_dir_all(&seed).unwrap();
    let surfaces = serde_json::json!({
        "_note": "own-seed fixture, before msg 235",
        "surfaces": [{
            "id": "design",
            "name": "Design",
            "description": "The interface's design.",
            "tasks": [{"serial": 1, "title": "Put every open question here", "state": "done"}],
            "questions": [{"serial": 1, "value": 50, "question": "Is one bucket enough?", "why": "One soup.", "options": ["YES.", "NO."], "recommendation": "YES."}],
            "children": [{
                "id": "system",
                "name": "System & splashes",
                "description": "How the ripples are shown.",
                "tasks": [{"serial": 1, "title": "Build it", "state": "blocked", "blocked_by": ["Q-1"]}],
                "questions": [{"serial": 1, "value": 95, "blocking": true, "question": "How is the top level organized?", "why": "The flow.", "options": ["C. Entries.", "A. One tree."], "recommendation": "C.", "gates": "T-1"}],
                "links": [{"surface": "design", "id": "Q-1"}]
            }]
        }]
    });
    std::fs::write(seed.join("surfaces.json"), serde_json::to_string_pretty(&surfaces).unwrap()).unwrap();
    let artifacts = serde_json::json!({
        "directories": [{"name": "design artifacts", "path": "/tmp", "claimed_by": ["system"]}],
        "worktrees": [],
        "artifacts": [{"id": "ART-1", "handle": "a mockup", "kind": "design", "location": "/tmp/none.txt", "made_by": "t", "for": "system/Q-1", "created": "2026-10-25T06:00:00Z", "regions": []}]
    });
    std::fs::write(seed.join("artifacts.json"), serde_json::to_string_pretty(&artifacts).unwrap()).unwrap();
}

/// The repo's ripple policy with every run cleared and nothing spent: the
/// design cauldron's own `ripples.json`.
pub fn write_quiet_ripples(root: &Path, env: &str, repo_seeds: &Path) {
    let mut r: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(repo_seeds.join("ripples.json")).unwrap()).unwrap();
    r["runs"] = serde_json::json!([]);
    for (_, st) in r["stats"].as_object_mut().unwrap() {
        let st = st.as_object_mut().unwrap();
        for k in ["today", "week", "running"] {
            if st.contains_key(k) {
                st.insert(k.into(), 0.into());
            }
        }
        st.remove("fires");
        st.remove("failed_last");
        if st.contains_key("last") {
            st.insert("last".into(), "".into());
        }
    }
    for (k, v) in r["week"].as_object_mut().unwrap() {
        *v = if k.starts_with("cost") { serde_json::json!(0.0) } else { serde_json::json!(0) };
    }
    std::fs::write(seed_dir(root, env).join("ripples.json"), serde_json::to_string_pretty(&r).unwrap()).unwrap();
}
