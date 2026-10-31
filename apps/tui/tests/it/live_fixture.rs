//! **A DAEMON'S FACTS, FED INTO THE THREADS TAB'S STORE WITHOUT A SOCKET**
//! (REL-6). The Threads tab has no fake data of its own any more: it draws
//! the chat face's `Store`, which the daemon feeds. The sweeps and look tests
//! that walk every screen of the tab need rows to walk, so this hands the
//! store the facts a daemon would — through the store's OWN doors
//! (`adopt_projects` for the indexes, `Store::apply` for the daemon's rows,
//! `load_transcript` for an open thread's tail) — never by writing the tab's
//! rows directly. `threads_live.rs` drives the same tab over a real socket.
//!
//! The fixture: P (pinned in an index, with two subagent threads K1 and K2,
//! one running), R1 and R2 (his recent threads no index carries; R1 active),
//! and X (closed in the index: Archived). Every thread has a short
//! transcript with a tool call.

use serde_json::{json, Value};
use tui::cauldron::app::App;
use tui::cauldron::threads;
use tui::proto::ThreadEvent;
use tui::store::LedgerRow;
use tui::transport::TransportEvent;

pub const P: &str = "fx-pinned-0001";
pub const R1: &str = "fx-recent-0002";
pub const R2: &str = "fx-recent-0003";
pub const X: &str = "fx-closed-0004";
pub const K1: &str = "fx-child-0005";
pub const K2: &str = "fx-child-0006";

pub const P_NAME: &str = "Coordinator for the halloween release";
pub const R1_NAME: &str = "Threads tab, real";
pub const R2_NAME: &str = "Answer the tax letter";
pub const X_NAME: &str = "An old closed thread";

/// The threads on the list, in no particular order.
pub const LISTED: [&str; 4] = [P, R1, R2, X];

const AT: &str = "2026-10-30T10:00:00Z";

fn index_thread(id: &str, name: &str, pinned: bool, closed: bool) -> tui::index::IndexThread {
    serde_json::from_value(json!({
        "id": id, "session_id": id, "summary": name, "pinned": pinned, "closed": closed,
        "last_turn_at": AT, "last_user_input_at": AT, "turn_count": 3,
    }))
    .expect("an index header")
}

/// **FEED THE DAEMON'S FACTS** into `a`'s store and project them.
pub fn feed(a: &mut App) {
    let s = &mut a.live.store;
    s.adopt_projects(vec![tui::index::Project {
        name: "harness".into(),
        root: "/Users/fixture/minds/harness".into(),
        threads: vec![index_thread(P, P_NAME, true, false), index_thread(X, X_NAME, false, true)],
        index: tui::index::IndexState::Read,
    }]);
    let row = |id: &str, name: &str, state: &str| {
        json!({"thread_id": id, "cwd": "/Users/fixture/minds/harness", "model": "opus", "state": state,
            "executing": state == "active", "last_user_input_at": AT, "name": name,
            "identity": true, "app_id": "tui"})
    };
    let rows = vec![row(P, P_NAME, "idle"), row(R1, R1_NAME, "active"), row(R2, R2_NAME, "idle"), row(X, X_NAME, "idle"), row(K1, "carve region R7", "active"), row(K2, "scout the raw list", "idle")];
    a.live.feed(TransportEvent::Threads { rows, capped: false });
    let rolled = |display: &str, status: &str| json!({"status": status, "display": display, "active_descendants": 0});
    let statuses = vec![
        json!({"thread_id": P, "state": "idle", "executing": false, "queued": 0, "unread": 2,
            "rolled": {"status": "active", "display": "active (+1)", "active_descendants": 1, "children_summary": "1 active, 1 idle"},
            "children": {"outstanding": [K1], "runs": [
                {"id": K1, "agent": "core:builder", "name": "carve region R7", "started_at": AT},
                {"id": K2, "agent": "Explore", "name": "scout the raw list", "started_at": AT, "finished": {"outcome": "ok", "at": AT}}]}}),
        json!({"thread_id": R1, "state": "active", "executing": true, "queued": 0, "unread": 0, "rolled": rolled("active", "active")}),
        json!({"thread_id": R2, "state": "idle", "executing": false, "queued": 0, "unread": 1, "rolled": rolled("idle", "idle")}),
        json!({"thread_id": X, "state": "idle", "executing": false, "queued": 0, "unread": 0, "rolled": rolled("idle", "idle")}),
        json!({"thread_id": K1, "state": "active", "executing": true, "parent_id": P, "rolled": rolled("active", "active")}),
        json!({"thread_id": K2, "state": "idle", "executing": false, "parent_id": P, "rolled": rolled("idle", "idle")}),
    ];
    a.live.feed(TransportEvent::Fleet { statuses, capped: false });
    a.live.feed(TransportEvent::Connected { boot_id: "boot-fixture".into(), resync: tui::transport::ResyncKind::First });
    threads::settle(a);
}

fn ledger(seq: u64, event: Value) -> LedgerRow {
    LedgerRow {
        seq,
        message_id: None,
        ts: chrono::DateTime::parse_from_rfc3339(AT).ok().map(|t| t.with_timezone(&chrono::Utc)),
        received: chrono::Utc::now(),
        event: ThreadEvent::parse(event),
    }
}

/// **THE OPEN THREAD'S TAIL, AS THE DAEMON'S LOG WOULD HAND IT** — for
/// whichever fixture thread the store has open; then projected.
pub fn transcript(a: &mut App) {
    let Some(id) = a.live.store.open_id().map(str::to_string) else { return };
    let rows = vec![
        ledger(1, json!({"kind": "user_message", "text": format!("what is the state of {id}?")})),
        ledger(2, json!({"kind": "tool_use", "id": "tu-1", "name": "Read", "input": {"file_path": "/Users/fixture/briefs/spec.md"}})),
        ledger(3, json!({"kind": "tool_result", "tool_use_id": "tu-1", "content": "ok", "is_error": false})),
        ledger(4, json!({"kind": "assistant_text", "text": "Two open questions, both acted on."})),
    ];
    a.live.store.load_transcript(&id, rows, 1);
    threads::settle(a);
}

/// Open `id` on the Threads tab and load its tail.
pub fn open(a: &mut App, id: &str) {
    a.threads.open_thread(id);
    threads::settle(a);
    transcript(a);
}
