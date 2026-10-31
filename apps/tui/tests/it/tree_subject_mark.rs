//! **TUI-P1d (TC-12): THE ENTERED NODE CARRIES `▸`, AND ONLY IT DOES.**
//!
//! TUI-P1c kept the agent tree rooted at the thread he opened while he walks
//! down it, and marked the node whose chat is above with `▸` — but no test
//! read that mark (TUI-P1c inspect, finding 2). This one does, on the RENDERED
//! frame, through the keys he presses: before entering, the root (the open
//! thread) carries the mark and no child does; after `→`/`enter` on a child,
//! that child carries it and the root does not.
//!
//! Nothing here touches his machine: `$HOME` is a scratch directory this
//! process made and the seam is `mirror::spawn_dead` (port 1, nothing behind
//! it), so no daemon is asked anything.
//!
//! FORCED RED (the agent's forcings, in `libs/tui-render`'s `tree_row_line`
//! caller): the mark drawn on the root row always — red on the after-enter
//! arm; the mark drawn on no row — red on the before-enter arm.

use chrono::Utc;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tui::app::App;
use tui::aui::AuiEvent;
use tui::event::{Ev, Input};
use tui::mirror;
use tui::transport::TransportEvent;
use tui_render::TreeId;

const W: u16 = 120;
const H: u16 = 30;

fn tmp() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "tui-p1d-tree-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("mkdir");
    p
}

/// A `$HOME` with ONE registered project holding ONE thread, `t1`.
fn home() -> PathBuf {
    let home = tmp();
    let root = home.join("elsewhere").join("alpha");
    std::fs::create_dir_all(root.join(".mind")).expect("mkdir");
    std::fs::write(
        root.join(".mind/threads.json"),
        r#"{"threads":[{"id":"t1","session_id":"t1","summary":"the root thread",
            "last_turn_at":"2026-01-02T00:00:00Z","turn_count":3}]}"#,
    )
    .expect("write");
    let mut reg = projects::Registry::empty();
    reg.register(&root, None).expect("registered");
    reg.save(&home).expect("saved");
    home
}

fn app_at(home: &Path) -> App {
    let (handle, _rx) = mirror::spawn_dead();
    let (tx, _erx) = mpsc::channel::<Ev>(16);
    App::new(handle, tx, "ws://127.0.0.1:1/ws".into(), home.to_str().expect("utf8").to_string(), 0)
}

fn key(app: &mut App, code: KeyCode) {
    app.apply(Ev::Input(vec![Input::Key(KeyEvent::new(code, KeyModifiers::NONE))]));
}

fn run(id: &str, name: &str) -> Value {
    json!({"id": id, "agent": "scout", "name": name, "wake": "end",
           "started_at": "2026-10-25T20:00:00Z", "request_id": id, "finished": null})
}

fn kids(ids: &[(&str, &str)]) -> Value {
    json!({"outstanding": ids.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
           "runs": ids.iter().map(|(i, n)| run(i, n)).collect::<Vec<_>>()})
}

fn rolled(status: &str, x: u64, summary: Option<&str>) -> Value {
    let display = if x > 0 { format!("{status} (+{x})") } else { status.to_string() };
    json!({"status": status, "display": display, "active_descendants": x, "children_summary": summary})
}

/// `t1` with two managed children: `c-a` (carve, idle with a running child
/// `g-1` grind) and `c-b` (bake, running).
fn the_fleet(app: &mut App) {
    let st = |id: &str, state: &str, exec: bool, parent: &str, r: Value| {
        json!({"thread_id": id, "state": state, "executing": exec, "queued": 0,
               "unread": 0, "parent_id": parent, "rolled": r})
    };
    let mut ca = st("c-a", "idle", false, "t1", rolled("active", 1, Some("1 active, 0 idle")));
    ca["children"] = kids(&[("g-1", "grind")]);
    app.apply(Ev::Transport(TransportEvent::Fleet {
        statuses: vec![
            json!({"thread_id": "t1", "state": "idle", "executing": false, "queued": 0,
                   "unread": 0, "last_seq": 1,
                   "children": kids(&[("c-a", "carve"), ("c-b", "bake")]),
                   "rolled": rolled("active", 2, Some("2 active, 0 idle"))}),
            ca,
            st("c-b", "active", true, "t1", rolled("active", 0, None)),
            st("g-1", "active", true, "c-a", rolled("active", 0, None)),
        ],
        capped: false,
    }));
}

/// The rendered frame, one string per row.
fn frame(app: &App) -> Vec<String> {
    tui_render::frame(app.state(), Utc::now(), W, H)
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

/// For each tree node, by a word only its row carries: does its row carry
/// the `▸` mark? The rows are found on the RENDERED frame.
fn marks(app: &App) -> Vec<(&'static str, bool)> {
    let f = frame(app);
    // The tree band: every row below its `─── agents` rule. The header's
    // title line also names the root thread, so it must not be searched.
    let rule = f
        .iter()
        .position(|l| l.starts_with("─── agents"))
        .unwrap_or_else(|| panic!("no agents band on the frame:\n{}", f.join("\n")));
    let tree: Vec<&String> = f[rule + 1..].iter().collect();
    [("root", "the root thread"), ("c-a", "carve"), ("c-b", "bake")]
        .iter()
        .map(|(who, word)| {
            let row = tree
                .iter()
                .find(|l| l.contains(word))
                .unwrap_or_else(|| panic!("{who}'s row ({word}) is not on the frame:\n{}", f.join("\n")));
            (*who, row.trim_start().starts_with('▸'))
        })
        .collect()
}

#[tokio::test]
async fn tui_p1d_the_entered_node_carries_the_mark_and_only_it_does() {
    let home = home();
    let mut app = app_at(&home);
    app.apply(Ev::Aui(AuiEvent::Invalidate { node: "root".into() }));
    app.lay(W, H);
    app.open("t1");
    the_fleet(&mut app);
    app.lay(W, H);

    // Before entering: the subject is the open thread, the root row.
    assert_eq!(app.state().tree.subject, TreeId::Parent, "the fixture did not start on the root");
    assert_eq!(
        marks(&app),
        vec![("root", true), ("c-a", false), ("c-b", false)],
        "before entering, the root and only the root carries ▸:\n{}",
        frame(&app).join("\n")
    );

    // Into the tree, onto c-a, and enter it — the keys he presses.
    key(&mut app, KeyCode::Down);
    for _ in 0..app.state().tree.rows.len() {
        if app.state().tree.cursor == TreeId::Thread("c-a".into()) {
            break;
        }
        key(&mut app, KeyCode::Down);
    }
    assert_eq!(app.state().tree.cursor, TreeId::Thread("c-a".into()), "could not walk to c-a");
    key(&mut app, KeyCode::Enter);
    app.lay(W, H);

    assert_eq!(
        marks(&app),
        vec![("root", false), ("c-a", true), ("c-b", false)],
        "after entering c-a, c-a and only c-a carries ▸:\n{}",
        frame(&app).join("\n")
    );
    let _ = std::fs::remove_dir_all(&home);
}
