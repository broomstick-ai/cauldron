//! # Rule 2.7's third stage, on the live wire and on a real reopen
//!
//! `docs/shapes.md` 2.7: *"`durable` — committed such that it survives a crash,
//! OPT-IN only, via a barrier that answers after commit. A producer that cannot
//! promise durability REFUSES outright rather than silently downgrading."*
//!
//! The RULE's logic lives in `shapewire::command::barrier` and is unit
//! tested there against a fake producer — which refusal comes before the fsync,
//! that the ceiling is named and never clamped, that an unminted id is
//! `unknown`. This file proves the two things a fake cannot:
//!
//! 1. **the route exists and `threads` is wired to it**, so `durable/barrier` is
//!    reachable from a socket and its contract is readable off `shapes.list`
//!    before anybody asks; and
//! 2. **the data is still there afterwards**, across a process that was killed
//!    with nothing closed and nothing flushed. That is the assertion this file
//!    exists for. A test that asserts the barrier RETURNED proves one line of
//!    Rust; the equivalent trap on the promise side was a test asserting the
//!    promise book was EMPTY, which proved the resolve ran and never that a
//!    frame carried the id.
//!
//! Every test here was **forced red before it was made green** and names its
//! mutation.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use shapewire::envelope::codes;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::config::Config;
use threads::event::{StoredEvent, ThreadEvent};
use threads::store::Store;
use threads::{build_manager, ThreadManager};

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

fn config(tmp: &std::path::Path) -> Config {
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    let mut accounts = BTreeMap::new();
    accounts.insert("first".to_string(), tmp.join("first").to_string_lossy().to_string());
    Config {
        data_dir: Some(tmp.to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    }
}

async fn start() -> (ThreadManager, SocketAddr, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let manager = build_manager(config(tmp.path())).unwrap();
    let (addr, h) = threads::chassis::serve(
        Arc::new(manager.clone()),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();
    std::mem::forget(h);
    (manager, addr, tmp)
}

struct Client {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .unwrap();
        let mut c = Self { ws };
        c.next().await.expect("a welcome");
        c
    }

    async fn next(&mut self) -> Option<Value> {
        use tokio_tungstenite::tungstenite::Message;
        loop {
            let got =
                tokio::time::timeout(std::time::Duration::from_millis(900), self.ws.next()).await;
            match got {
                Err(_) | Ok(None) => return None,
                Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).ok(),
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) => return None,
            }
        }
    }

    async fn request(&mut self, id: &str, method: &str, params: Value) -> Value {
        use tokio_tungstenite::tungstenite::Message;
        self.ws
            .send(Message::Text(
                json!({"type":"request","id":id,"method":method,"params":params}).to_string().into(),
            ))
            .await
            .unwrap();
        loop {
            let Some(f) = self.next().await else {
                panic!("no response to {method}");
            };
            if f["type"] == "response" && f["correlate"] == id {
                return f["outcome"].clone();
            }
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. THE WIRE
// ═════════════════════════════════════════════════════════════════════════════

/// **THE BARRIER IS REACHABLE FROM A SOCKET AND ANSWERS THE STAGE.**
///
/// `threads/create` is a command, so its reply carries a `promise_id` (rule
/// 2.7's `accepted`). The barrier then names that id and answers `durable`. The
/// two replies together are the rule: `accepted` is free and arrived first,
/// `durable` cost an ask.
///
/// **Forced red** by removing `"durable/barrier"` from `CHASSIS_METHODS`: the
/// outcome comes back `{"code":"unknown_method","error":"unknown method:
/// durable/barrier","type":"error"}` and the first assertion fails, *left:
/// String("error"), right: "success"*.
#[tokio::test]
async fn the_barrier_answers_the_stage_for_a_promise_this_producer_issued() {
    let (_m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;

    let created = c
        .request(
            "1",
            "threads/create",
            json!({ "settings": { "cwd": "/tmp", "model": "sonnet" } }),
        )
        .await;
    let id = created["payload"]["promise_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("a command answers `accepted` with an id: {created}"));

    let out = c
        .request("2", "durable/barrier", json!({ "promises": [id] }))
        .await;
    assert_eq!(out["type"], "success", "{out}");
    assert_eq!(out["payload"]["stage"], json!("durable"), "{out}");
    assert_eq!(out["payload"]["promises"], json!([id]), "{out}");
    assert!(
        out["payload"].get("synchronous").is_none()
            && out["payload"].get("checkpointed_frames").is_none(),
        "milestones, not protocol details: {out}"
    );
    drop(tmp);
}

/// **AN ID NOBODY ISSUED IS REFUSED, THROUGH THE WHOLE STACK.**
///
/// The unit test proves `barrier()` refuses it; this proves the refusal keeps
/// its TYPED code all the way out through `Outcome`, which is where R1's whole
/// point lives — a consumer must not have to match on the English.
///
/// **Forced red** by `Refusal::untyped` in place of `Refusal::new(codes::UNKNOWN,
/// …)`. The refusal still arrives with the right SENTENCE and the wrong code —
/// *left: String("error"), right: String("unknown")* — which is the whole of R1
/// in one failure: a consumer matching on the English would have seen no
/// change.
#[tokio::test]
async fn an_id_this_producer_never_issued_is_refused_with_its_own_code() {
    let (_m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    let out = c
        .request("1", "durable/barrier", json!({ "promises": [4096] }))
        .await;
    assert_eq!(out["type"], "error", "{out}");
    assert_eq!(out["code"], json!(codes::UNKNOWN), "{out}");
    drop(tmp);
}

/// **THE CONTRACT RIDES `shapes.list`, SO A CONSUMER READS IT BEFORE IT ASKS** —
/// rule 2.8's last clause, which `chassis_api.rs` already pins for every
/// per-shape ceiling. This is the producer-level one: durability belongs to the
/// STORE and no shape has its own, so it is a sibling of `shapes` rather than a
/// field inside one.
///
/// Its ABSENCE is the informative case and it is why the key is skipped rather
/// than nulled: absent means *this producer promises no durability and the
/// barrier will refuse `unsupported`* — the same claim, one round trip earlier.
///
/// **Forced red** by `return None;` at the top of `threads`' `durable()`
/// override, which is what deleting it would amount to — `Producer::durable`'s
/// default is `None`. `shapes.list` comes back with every shape and no `durable`
/// key at all: *left: Null, right: Object {"max_promises": Number(1024)}*.
#[tokio::test]
async fn shapes_list_declares_the_barrier_contract_before_a_consumer_asks() {
    let (_m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;
    let out = c.request("1", "shapes/list", json!({})).await;
    let p = &out["payload"];
    assert!(p["shapes"].is_array(), "{out}");
    assert_eq!(
        p["durable"],
        json!({ "max_promises": shapewire::command::MAX_PROMISES_PER_BARRIER }),
        "the ceiling a barrier refusal would have named, readable first: {out}"
    );
    drop(tmp);
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. THE DATA, ACROSS A REAL CRASH
// ═════════════════════════════════════════════════════════════════════════════

const CHILD: &str = "THREADS_DURABLE_CRASH_CHILD";
const EVENTS: u64 = 40;

fn ev(seq: u64) -> StoredEvent {
    StoredEvent {
        thread_id: "t".to_string(),
        seq,
        message_id: Some(seq),
        ts: threads::store::now_rfc3339(),
        event: ThreadEvent::ContextInjected {
            text: format!("event {seq}"),
        },
    }
}

/// **THE CHILD.** Not a test of anything on its own — it is the crash. Run
/// directly it does nothing; run with [`CHILD`] set it opens the store named by
/// that variable, writes, asks for the barrier, and then **dies without
/// unwinding**: no `Drop`, no `sqlite3_close`, no checkpoint, no graceful
/// shutdown, not even the test harness's own epilogue.
///
/// `libc::_exit` and not `std::process::exit`, deliberately: `exit` runs
/// `atexit` handlers, and the thing being tested is what is on disk when
/// nothing runs at all.
#[test]
fn durable_crash_child() {
    let Ok(db) = std::env::var(CHILD) else {
        return;
    };
    let s = Store::open(&db).unwrap();
    for i in 1..=EVENTS {
        s.append_event(&ev(i)).unwrap();
    }
    let outcome = s.durability_barrier().expect("the barrier must answer");
    eprintln!("child: {} events, barrier {outcome:?}", EVENTS);
    unsafe { libc::_exit(42) }
}

/// **THE PROOF: A PROCESS THAT WAS KILLED WITH NOTHING CLOSED, AND THE EVENTS
/// ARE STILL THERE.**
///
/// This is the trap the brief names, avoided. *"Asserting the barrier RETURNED
/// rather than that the data survives"* is an assertion about one line of Rust;
/// `Ok(())` is free to lie. So the observation here is taken by a DIFFERENT
/// PROCESS, from a store this process never opened: the child writes 40 events,
/// calls the barrier, and `_exit(42)`s mid-test with no destructor run and no
/// connection closed. The parent then opens the same path for the first time and
/// counts.
///
/// What it proves and what it does not, stated so the next reader does not
/// overclaim it. It proves the committed rows are recoverable by an independent
/// process from the files on disk, with no clean teardown anywhere in the story
/// — which is what a consumer asking for `durable` is usually asking about, and
/// which is strictly more than the barrier returning. It does NOT isolate the
/// `fsync`: the page cache outlives a process, so only power loss or a kernel
/// panic separates `synchronous=FULL` from `NORMAL`, and no test in a user
/// process can stage either. The assertion that DOES bear on the sync is
/// `store.rs`'s `a_barrier_puts_the_rows_in_the_database_file_and_not_only_in_
/// the_log`, which copies the database file away from its own write-ahead log.
///
/// **Forced red** by having the child write to a DIFFERENT path than the one it
/// was handed (`db + ".other"`), which is the shape of every way this could be
/// green while proving nothing — the parent opens a store nobody wrote to and
/// the count fails *left: 0, right: 40*. It is pinned to HOW MANY survived and
/// not to "something did": `EVENTS` is one constant read by both halves, so the
/// child cannot write fewer than the parent demands, and `store.rs`'s
/// `commits_survive_a_reopen_with_no_clean_shutdown_at_all` carries the
/// partial-write mutation (`append_event` skipping even `seq`s, which fails
/// `13` against `25`).
#[test]
fn every_event_survives_a_process_that_died_with_nothing_closed() {
    let dir = std::env::temp_dir().join(format!("threads-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("threads.db");

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "durable_barrier::durable_crash_child", "--nocapture"])
        .env(CHILD, &db)
        .status()
        .expect("spawning this test binary as the crashing child");
    assert_eq!(
        status.code(),
        Some(42),
        "the child must have reached `_exit(42)` — any other code means it \
         returned normally and the crash never happened"
    );

    // FIRST open in this process. Whatever is here, the dead child put it here.
    let s = Store::open(&db).unwrap();
    let rows = s.events_by_range("t", None, None, None, None).unwrap();
    assert_eq!(
        rows.len() as u64,
        EVENTS,
        "every event the child committed before the barrier must be readable \
         by a process that has never seen this store before"
    );
    assert_eq!(rows.first().unwrap().seq, 1);
    assert_eq!(rows.last().unwrap().seq, EVENTS);
    let _ = std::fs::remove_dir_all(&dir);
}
