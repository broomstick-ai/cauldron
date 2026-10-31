//! SCRATCH PROBE — projects a window of REAL ledger rows and prints the face.
//! Not a test and not shipped: it exists to answer "does this build read HIS
//! events", which no fixture can answer, and it reads a scratch COPY of the db
//! dumped to JSON. The live daemon is never touched.
use tui::proto::{ServerMsg, ThreadEvent};
use tui::store::{self, Opened, Store};
use tui::transport::TransportEvent;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: real_ledger_probe <window.json>");
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("parse");
    let now = chrono::Utc::now();
    let mut s = Store::new("ws://x/ws".into(), "/Users/testuser".into());
    s.open("t");
    s.load_transcript("t", Vec::new(), 1);
    let total = rows.len();
    for (i, r) in rows.into_iter().enumerate() {
        s.apply(
            TransportEvent::Frame {
                msg: ServerMsg::Event {
                    thread_id: "t".into(),
                    seq: i as u64 + 1,
                    message_id: None,
                    ts: "2026-09-15T12:00:00Z".into(),
                    event: ThreadEvent::parse(r),
                },
                replayed: false,
            },
            now,
        );
    }
    // The stdout-only toggle, OFF — the face he starts on, which is the only
    // state a measurement of what he is looking at may use.
    let f = false;
    // **THE MEASUREMENT HE ASKED FOR**: what the chat area gains when the
    // subagents leave it. The same tail projected twice — once with the lens
    // (subagents as holes plus one tile each) and once without (every
    // attributed row in his chat, which is what he was looking at).
    let rows = store::open_rows(&s);
    let agents = tui::agents::direct_attribute(&rows, now);
    drop(rows);
    let with_tree = store::thread_blocks(
        &s,
        f,
        false,
        &Opened::new(),
        &agents,
        &tui_render::TreeId::Parent,
        None,
    );
    let without = store::thread_blocks(&s, f, false, &Opened::new(), &[], &tui_render::TreeId::Parent, None);
    let closed = with_tree;
    println!(
        "=== {total} real ledger events -> {} rows in his chat (was {}) ===",
        closed.len(),
        without.len()
    );
    if !without.is_empty() {
        println!(
            "=== CHAT AREA RECLAIMED: {} of {} rows ({:.1}%) · {} subagents · tree rows {} ===",
            without.len() - closed.len(),
            without.len(),
            100.0 * (without.len() - closed.len()) as f64 / without.len() as f64,
            agents.len(),
            store::agent_tree(&s, &agents, tui_render::TreeId::Parent, tui_render::TreeId::Parent, &mut store::TreeCtx::default())
                .rows
                .len()
        );
    }
    for a in &agents {
        println!(
            "agent {:>6} {:<44} {:<11} tok {:?} calls {:?} bash {:?} kids {:?}",
            &a.id[..a.id.len().min(6)],
            a.label.chars().take(44).collect::<String>(),
            a.state.word(),
            a.tokens,
            a.calls,
            a.bash_in_flight,
            a.agents_running
        );
    }
    for b in closed.iter() {
        let t = b.text();
        let kind = if b.is_message() { "BOX " } else { "lane" };
        println!(
            "{kind} |{}| {}",
            if b.is_expandable() { "+" } else { " " },
            t.chars().take(140).collect::<String>()
        );
    }
}
