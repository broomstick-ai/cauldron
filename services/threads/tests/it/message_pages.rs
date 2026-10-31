//! # `threads.pages` — a thread's message history, read through the resource door
//!
//! R2.S3d, item C. The harness's `thread-messages` read historical message
//! ranges with `threads/{id}/query`, a request. The ruling removes the ability
//! to request data, so the `threads` shape now CARRIES the resource ids of the
//! thread's message pages (`pages: [{id, from, to}]`, `crate::shapes::pages`)
//! and the existing door — `resource/{id}/read`, `GET /resource/{id}` — serves
//! each page. No new verb: the door's id is opaque and the producer resolves it.
//!
//! Driven end to end on `127.0.0.1:0` (the chassis this test starts; never the
//! operator's daemon). Each test names the mutation that reddened it.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::config::Config;
use threads::event::{StoredEvent, ThreadEvent};
use threads::shapes::pages;
use threads::{build_manager, ThreadManager, ThreadSettings};

struct Up {
    manager: ThreadManager,
    addr: SocketAddr,
    _tmp: tempfile::TempDir,
}

async fn start() -> Up {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    std::env::set_var(
        "THREADS_CLAUDE_BIN",
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(first_exec::warmed("tests/fixtures/fake-claude")),
    );
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "first".to_string(),
        tmp.path().join("first").to_string_lossy().to_string(),
    );
    threads::clock::testing::ticking();
    let manager = build_manager(Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    })
    .unwrap();
    let chassis = threads::chassis::chassis(Arc::new(manager.clone()));
    let listener = shapewire::server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let (addr, h) = chassis
        .serve_on_with_shutdown(listener, std::future::pending::<()>())
        .unwrap();
    std::mem::forget(h);
    Up {
        manager,
        addr,
        _tmp: tmp,
    }
}

/// A thread holding `n` user messages, each with message id = its seq. Returns
/// `(thread_id, first_id, last_id)`.
fn seed(m: &ThreadManager, cwd: &str, n: u64) -> (String, u64, u64) {
    seed_with(m, cwd, n, |seq| format!("message number {seq}"))
}

/// [`seed`], with each message's text chosen by `text(seq)`.
fn seed_with(
    m: &ThreadManager,
    cwd: &str,
    n: u64,
    text: impl Fn(u64) -> String,
) -> (String, u64, u64) {
    let mut s = ThreadSettings::new(cwd.to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    let id = m.create_thread(None, s).unwrap();
    let base = m.store.max_seq(&id).unwrap();
    for i in 0..n {
        let seq = base + 1 + i;
        m.store
            .append_event(&StoredEvent {
                thread_id: id.clone(),
                seq,
                message_id: Some(seq),
                ts: format!("2026-10-02T10:{:02}:00Z", seq % 60),
                event: ThreadEvent::UserMessage {
                    text: text(seq),
                    intent: Default::default(),
                    metadata: Value::Null,
                },
            })
            .unwrap();
    }
    (id, base + 1, base + n)
}

struct Client {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    bearer: String,
}

impl Client {
    async fn connect(addr: SocketAddr) -> Self {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .unwrap();
        let mut c = Self {
            ws,
            bearer: String::new(),
        };
        let welcome = c.next().await.expect("a welcome");
        c.bearer = welcome["bearer"].as_str().unwrap_or_default().to_string();
        c
    }

    async fn next(&mut self) -> Option<Value> {
        use tokio_tungstenite::tungstenite::Message;
        loop {
            let got =
                tokio::time::timeout(std::time::Duration::from_millis(1500), self.ws.next()).await;
            match got {
                Err(_) | Ok(None) => return None,
                Ok(Some(Ok(Message::Text(t)))) => return serde_json::from_str(&t).ok(),
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) => return None,
            }
        }
    }

    async fn request(&mut self, id: &str, method: &str, mut params: Value) -> Value {
        use tokio_tungstenite::tungstenite::Message;
        if method.ends_with("/subscribe") {
            params["consumer"] = json!({"name": "threads-tests", "shape_version": "shapes.md@rev4"});
        }
        self.ws
            .send(Message::Text(
                json!({"type":"request","id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        loop {
            let Some(f) = self.next().await else {
                panic!("no response to {method}");
            };
            if f["type"] == "response" && f["correlate"] == id {
                return f;
            }
        }
    }

    /// The `threads` row for `key` — subscribe, then the newest row frame.
    async fn threads_row(&mut self, key: &str) -> Value {
        let r = self
            .request("s", "shapes/threads/subscribe", json!({"key": key}))
            .await;
        assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
        let mut row = Value::Null;
        while let Some(f) = self.next().await {
            if f["type"] == "shape" && f["shape"] == "threads" && f["key"] == key {
                if let Some(r) = f.get("row").filter(|r| !r.is_null()) {
                    row = r.clone();
                } else if f["event"]["kind"] == "row" {
                    row = f["event"]["row"].clone();
                }
            }
        }
        assert!(!row.is_null(), "no `threads` row for {key}");
        row
    }

    /// One page through `resource/{id}/read`, decoded.
    async fn page(&mut self, rid: &str) -> Result<Value, Value> {
        let r = self
            .request("p", &format!("resource/{rid}/read"), json!({}))
            .await;
        if r["outcome"]["type"] != "success" {
            return Err(r["outcome"].clone());
        }
        let bytes = shapewire::mirror::inline_bytes(&r["outcome"]["payload"])
            .expect("the chassis decodes it");
        Ok(serde_json::from_slice(&bytes).expect("a page is JSON"))
    }
}

async fn http_cache_control(addr: SocketAddr, target: &str, bearer: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Authorization: Bearer {bearer}\r\nContent-Length: 0\r\n\r\n"
    );
    s.write_all(head.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(10), s.read_to_end(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    let status = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    let cc = text
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("cache-control:"))
        .map(|l| l.splitn(2, ':').nth(1).unwrap().trim().to_string())
        .unwrap_or_default();
    (status, cc)
}

/// `GET {target}` on the by-id door: `(status, body)`. The door answers with an
/// honest `Content-Length` and `Connection: close`, so the body is everything
/// after the headers.
async fn http_get(addr: SocketAddr, target: &str, bearer: &str) -> (u16, Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Authorization: Bearer {bearer}\r\nContent-Length: 0\r\n\r\n"
    );
    s.write_all(head.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(10), s.read_to_end(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("headers must terminate");
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, buf[split + 4..].to_vec())
}

/// **THE SHAPE NAMES THE PAGES, AND THE DOOR SERVES EVERY MESSAGE ONCE, IN
/// RANGE.** 300 messages cross the 256-id page boundary, so there are two pages;
/// reading every page the row names must return every seeded message exactly
/// once, each inside its page's `[from, to]`, each spelled as the query spelled
/// it (`message_id`, `kind`, `text`).
///
/// **MUTATIONS.** (a) `pages_of` stops cutting at the newest id (`to = from + P
/// - 1` always): red, the door refuses the open page as `unknown`. (b)
/// `fetch_page` drops `from` from the query: red, the second page carries ids
/// of the first. (c) the `pages` cell exports `[]`: red, no page is named.
#[tokio::test]
async fn the_shape_names_the_pages_and_the_door_serves_every_message_once() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    let (id, first, last) = seed(&up.manager, &cwd, 300);
    let mut c = Client::connect(up.addr).await;
    let row = c.threads_row(&id).await;

    let named = row["pages"].as_array().cloned().unwrap_or_default();
    // **LITERALS FROM THE SEEDED STORE, NOT THE CONSTANT UNDER TEST** (R2.S3d
    // inspect, finding 4). Deriving the expectation from `PAGE_MESSAGES` let a
    // 255-wide page pass here; the fixture's newest id is 301 and a page is 256
    // ids, so the answer is two pages, written out.
    assert_eq!(last, 301, "the fixture: 300 messages after the creation event");
    let ranges: Vec<(u64, u64)> = named
        .iter()
        .map(|p| (p["from"].as_u64().unwrap(), p["to"].as_u64().unwrap()))
        .collect();
    assert_eq!(
        ranges,
        [(0, 255), (256, 301)],
        "one full page of 256 ids and the open page cut at the newest: {named:?}"
    );

    let mut seen = Vec::new();
    for p in &named {
        let rid = p["id"].as_str().expect("a page carries its resource id");
        let (from, to) = (p["from"].as_u64().unwrap(), p["to"].as_u64().unwrap());
        let body = c.page(rid).await.unwrap_or_else(|e| panic!("{rid} refused: {e}"));
        assert_eq!(body["thread_id"], json!(id));
        for m in body["messages"].as_array().unwrap() {
            let mid = m["message_id"].as_u64().unwrap();
            assert!(
                (from..=to).contains(&mid),
                "{rid} served message {mid}, outside [{from}, {to}]"
            );
            assert_eq!(m["kind"], json!("user_message"));
            assert_eq!(m["text"], json!(format!("message number {mid}")));
            seen.push(mid);
        }
    }
    assert_eq!(
        seen,
        (first..=last).collect::<Vec<_>>(),
        "every message, once, oldest first"
    );
}

/// **A FULL PAGE IS IMMUTABLE AND THE OPEN PAGE IS NOT.**
///
/// **MUTATION.** `fetch_page` declares every page `Retention::Fixed`: red, the
/// open page is served `immutable` and a consumer would cache a page the next
/// message changes.
#[tokio::test]
async fn a_full_page_is_immutable_and_the_open_page_is_not() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    let (id, _, last) = seed(&up.manager, &cwd, 300);
    let mut c = Client::connect(up.addr).await;
    let row = c.threads_row(&id).await;
    let named = row["pages"].as_array().cloned().unwrap();
    let full = named[0]["id"].as_str().unwrap().to_string();
    let open = named.last().unwrap()["id"].as_str().unwrap().to_string();
    assert_ne!(full, open);
    assert_eq!(last, 301, "the fixture's last page, 256-301, is open");

    let (s1, cc_full) = http_cache_control(up.addr, &format!("/resource/{full}"), &c.bearer).await;
    let (s2, cc_open) = http_cache_control(up.addr, &format!("/resource/{open}"), &c.bearer).await;
    assert_eq!((s1, s2), (200, 200));
    assert!(cc_full.contains("immutable"), "a full page: {cc_full}");
    assert!(!cc_open.contains("immutable"), "the open page is re-named by the next message: {cc_open}");
}

/// **AN ID NO SHAPE PUBLISHED IS REFUSED `unknown`** — a range past the newest
/// message, and a range that is not a page.
///
/// **MUTATION.** Delete `fetch_page`'s `to > newest` refusal: red, the range
/// past the newest message is served (as an empty page) where the row never
/// named it.
#[tokio::test]
async fn an_unpublished_page_is_refused_unknown() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    let (id, _, last) = seed(&up.manager, &cwd, 10);
    let mut c = Client::connect(up.addr).await;
    for rid in [
        // A FULL page past the thread's newest message: a well-formed page id
        // whose range the `pages` cell never named.
        pages::page_id(&id, 256, 511).unwrap(),
        // Not a page at all: `from` is not on a page boundary.
        format!("{id}.m1-{last}"),
    ] {
        let e = c.page(&rid).await.expect_err(&format!("{rid} must be refused"));
        assert_eq!(e["code"], json!("unknown"), "{rid}: {e}");
    }
}

/// **A NEW MESSAGE RE-NAMES THE OPEN PAGE, LIVE.** The row's `pages` is
/// derived off `last_message_id` at export, so the producer's own event moves
/// it with no request.
///
/// **MUTATION.** The fold stops moving `last_message_id` on a live event (the
/// one register `pages` is derived from is then written only by the hydrate):
/// red, the open page still ends at the old id.
#[tokio::test]
async fn a_new_message_renames_the_open_page_live() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    // Through the producer's own allocator (`append_custom`), not `seed`'s
    // direct ledger writes: the live append below allocates the NEXT id, and an
    // allocator that never saw the seeded ids would hand out a duplicate.
    let (id, _, _) = seed(&up.manager, &cwd, 0);
    let mut last = 0;
    for n in 0..5 {
        let (mid, _) = up
            .manager
            .append_custom(&id, "note".into(), json!({"n": n}), true)
            .unwrap();
        last = mid.unwrap();
    }
    let mut c = Client::connect(up.addr).await;
    let before = c.threads_row(&id).await;
    assert_eq!(before["pages"].as_array().unwrap().last().unwrap()["to"], json!(last));

    let (mid, _) = up
        .manager
        .append_custom(&id, "note".into(), json!({"n": 1}), true)
        .unwrap();
    let mid = mid.unwrap();
    let mut after = Value::Null;
    while let Some(f) = c.next().await {
        if f["shape"] == "threads" && f["key"] == json!(id) && f["event"]["kind"] == "row" {
            after = f["event"]["row"].clone();
        }
    }
    let open = after["pages"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        open.last().map(|p| p["to"].clone()),
        Some(json!(mid)),
        "the open page now ends at the new message: {after}"
    );
}

/// **A PAGE LARGER THAN THE INLINE CAP IS STILL READABLE — WHOLE, THROUGH THE
/// BY-ID DOOR** (R2.S3f).
///
/// A page is 256 message ids, and 256 long messages exceed
/// `shapewire::resource::MAX_WS_INLINE_BYTES`, so the inline door refuses it
/// `too_large` — correctly: that cap is the producer's bound on its own frame
/// memory. The SAME opaque id is served by `GET /resource/{id}`, which streams
/// and has no such cap, and that door is the one the refusal points at. This
/// test pins the producer half: the oversize page reads whole through the GET
/// door, every message in its range, and in the same format as a small page
/// read through the websocket.
///
/// **POSITIVE SENTINEL.** The small (open) page of the same thread, read
/// through `resource/{id}/read`, is byte-for-byte the JSON the GET door answers
/// for it, and the oversize page has its keys and its message spelling.
///
/// **MUTATIONS.** (a) `fetch_page` queries `[from + P, to + P]` — the door
/// answers the WRONG page: red, page `m0-255` carries ids 256..301. (b) the
/// fixture's bodies shrink below the cap: red, the fixture no longer exceeds
/// the inline cap and the test proves nothing about an oversize page.
#[tokio::test]
async fn an_oversize_page_reads_whole_through_the_by_id_door() {
    const BODY_BYTES: usize = 6000;
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    let text = |seq: u64| format!("message number {seq} {}", "x".repeat(BODY_BYTES));
    let (id, first, last) = seed_with(&up.manager, &cwd, 300, text);
    assert_eq!(last, 301, "the fixture: 300 messages after the creation event");
    let mut c = Client::connect(up.addr).await;
    let row = c.threads_row(&id).await;
    let named = row["pages"].as_array().cloned().unwrap_or_default();
    let ranges: Vec<(u64, u64)> = named
        .iter()
        .map(|p| (p["from"].as_u64().unwrap(), p["to"].as_u64().unwrap()))
        .collect();
    assert_eq!(ranges, [(0, 255), (256, 301)], "{named:?}");
    let big = named[0]["id"].as_str().unwrap().to_string();
    let small = named[1]["id"].as_str().unwrap().to_string();

    // 1. The oversize page, by id over HTTP: every message of [0, 255], whole.
    let (status, bytes) = http_get(up.addr, &format!("/resource/{big}"), &c.bearer).await;
    assert_eq!(status, 200, "{big} on the by-id door: {}", String::from_utf8_lossy(&bytes));
    let got: Value = serde_json::from_slice(&bytes).expect("a page is JSON");
    assert_eq!((&got["thread_id"], &got["from"], &got["to"]), (&json!(id), &json!(0), &json!(255)));
    let ids: Vec<u64> = got["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["message_id"].as_u64().unwrap())
        .collect();
    assert_eq!(
        ids,
        (first..=255).collect::<Vec<_>>(),
        "the by-id door answered {big} with the wrong page"
    );
    for m in got["messages"].as_array().unwrap() {
        let mid = m["message_id"].as_u64().unwrap();
        assert_eq!(m["text"], json!(text(mid)), "message {mid} is served whole");
    }
    assert!(
        bytes.len() as u64 > shapewire::resource::MAX_WS_INLINE_BYTES,
        "the fixture must exceed the inline cap: {big} is {} bytes, the cap {}",
        bytes.len(),
        shapewire::resource::MAX_WS_INLINE_BYTES
    );

    // 2. The same id on the inline door is refused `too_large` — the code a
    //    consumer branches on to take the by-id door with the id it holds.
    let e = c.page(&big).await.expect_err("an oversize page is not answered inline");
    assert_eq!(e["code"], json!("too_large"), "{e}");

    // 3. POSITIVE SENTINEL: the small page, on both doors, and the format.
    let ws_small = c.page(&small).await.unwrap_or_else(|e| panic!("{small} refused: {e}"));
    let (s2, get_small) = http_get(up.addr, &format!("/resource/{small}"), &c.bearer).await;
    assert_eq!(s2, 200);
    let get_small: Value = serde_json::from_slice(&get_small).unwrap();
    assert_eq!(ws_small, get_small, "the two doors answer one page identically");
    let small_ids: Vec<u64> = ws_small["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["message_id"].as_u64().unwrap())
        .collect();
    assert_eq!(small_ids, (256..=last).collect::<Vec<_>>());
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys(&got), keys(&ws_small), "the oversize page has a page's keys");
    assert_eq!(
        keys(&got["messages"][0]),
        keys(&ws_small["messages"][0]),
        "a message on the oversize page is spelled as on a small page"
    );
}

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
