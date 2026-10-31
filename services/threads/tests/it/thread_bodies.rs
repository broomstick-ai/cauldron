//! # Message bodies — the first resource, through both doors
//!
//! Three things, and the third is the ruling:
//!
//! 1. The `threads` shape publishes a SUMMARY of the newest message's body —
//!    one id, its size, the doors it is served on — and never the content and
//!    never a list.
//! 2. That id resolves on `GET /resource/<id>` **and** on the websocket's
//!    `resource/{id}/read`, to the same bytes, through one
//!    `Resources::fetch`.
//! 3. **The consuming chassis chooses between them per payload, and an
//!    application driving `shapewire::mirror` cannot tell which it got.**
//!
//! These go through a real socket and a real HTTP door on purpose, unlike most
//! of `thread_shapes.rs`: the claim being tested is that two TRANSPORTS agree,
//! and a test that drove the hub directly would have no transports in it.
//!
//! Every test here was forced red by a named mutation; the mutation is in the
//! test's own docstring.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;

use shapewire::envelope::Refusal;
use shapewire::mirror::{ContentError, Fetch, Mirror, ResourceDoor};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::config::Config;
use threads::event::{StoredEvent, ThreadEvent};
use threads::shapes::body;
use threads::shapes::threads as tshape;
use threads::{build_manager, ThreadManager, ThreadSettings};

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

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
    // THE CLOCK SEAM. Every `Hub::emit` stamps with the clock and every cell
    // refuses an input at or before its `folded_at`, so two emits in one
    // microsecond mean the second is DROPPED — which is what an optimized
    // build does to a tight emit loop. See `src/clock.rs`.
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

/// One thread with the events given, appended straight to the ledger in order.
fn seed(m: &ThreadManager, tmp_cwd: &str, events: &[ThreadEvent]) -> String {
    let mut s = ThreadSettings::new(tmp_cwd.to_string(), "fable".to_string());
    s.injection = threads::settings::Injection::NOTHING;
    let id = m.create_thread(None, s).unwrap();
    let base = m.store.max_seq(&id).unwrap();
    for (i, ev) in events.iter().enumerate() {
        let seq = base + 1 + i as u64;
        m.store
            .append_event(&StoredEvent {
                thread_id: id.clone(),
                seq,
                message_id: Some(seq),
                ts: format!("2026-10-02T10:{:02}:00Z", seq % 60),
                event: ev.clone(),
            })
            .unwrap();
    }
    id
}

fn user(text: &str) -> ThreadEvent {
    ThreadEvent::UserMessage {
        text: text.to_string(),
        intent: Default::default(),
        metadata: Value::Null,
    }
}

// ── the websocket client ─────────────────────────────────────────────────────

struct Client {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    bearer: String,
}

/// **RULE 2.1'S CONSUMER IDENTITY, STATED IN ONE PLACE FOR EVERY SUBSCRIBE THIS
/// FILE SENDS.**
///
/// Identity is COMPULSORY at subscribe time since the operator's ruling of
/// 2026-10-08 (*"consumers to be forced to say who they are so we can track
/// consumers"*), so a subscribe carrying no `consumer` is refused `malformed`.
/// The tests in this file are about hydration, unsubscribe, retention, paging
/// and the rest — NOT about identity — so they state a well-formed one here,
/// once, exactly as a real consumer does: `seam/link.rs`'s `subscribe_params`
/// builds its identity in one place for the same reason, and the board's
/// `api.py` now does too.
///
/// **IT NEVER OVERWRITES AN IDENTITY THE CALLER STATED.** A test that sends a
/// deliberately blank or absent one to prove the refusal still can, by saying
/// `consumer` itself. That is how the rule is actually proven, and it is proven
/// elsewhere on purpose — `shapes.rs`'s
/// `an_absent_identity_is_refused_at_the_wire_and_so_is_a_blank_one` at the wire
/// and `shapewire::ledger`'s `an_absent_identity_is_refused_and_so_is_a_blank_one`
/// at the parser — never by this helper, which exists to keep unrelated tests
/// from asserting identity by accident.
fn identified(method: &str, params: Value) -> Value {
    if !method.ends_with("/subscribe") {
        return params;
    }
    let mut params = params;
    if params.get("consumer").is_some() {
        return params;
    }
    if let Some(o) = params.as_object_mut() {
        o.insert(
            "consumer".into(),
            json!({"name": "threads-tests", "shape_version": "shapes.md@rev4"}),
        );
    }
    params
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

    async fn request(&mut self, id: &str, method: &str, params: Value) -> Value {
        let params = identified(method, params);
        use tokio_tungstenite::tungstenite::Message;
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

    /// Every frame that arrives until the socket goes quiet.
    async fn drain(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        while let Some(v) = self.next().await {
            out.push(v);
        }
        out
    }
}

// ── the HTTP door ────────────────────────────────────────────────────────────

struct Res {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

async fn http_get(addr: SocketAddr, target: &str, bearer: &str) -> Res {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Authorization: Bearer {bearer}\r\nContent-Length: 0\r\n\r\n"
    );
    s.write_all(head.as_bytes()).await.unwrap();
    s.flush().await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        s.read_to_end(&mut buf),
    )
    .await
    .expect("the response did not finish")
    .unwrap();
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("headers must terminate");
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let body = buf[split + 4..].to_vec();
    let mut lines = head.lines();
    let status: u16 = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    Res {
        status,
        headers,
        body,
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. The shape says what exists
// ═════════════════════════════════════════════════════════════════════════════

/// **ONE SUMMARY, NEVER AN ENUMERATION — and that holds at 200 messages.**
///
/// Forced red by adding a `"body_id"` field to `ThreadCells::to_json` beside
/// `body` — a second spelling of one id, which is the duplication rule 2.4 is
/// for deleting: `the export names 2 resource ids for a 200-message thread; a
/// summary is a summary`.
///
/// **The "the content does not ride the frame" assertion CANNOT be forced red,
/// and that is recorded rather than faked.** There is nowhere to put it: the
/// `body` cell holds a `ResourceSummary`, whose three fields are an id, a byte
/// count and a list of door names, and no mutation short of adding a field to
/// `shapewire::wire::ResourceSummary` and a reader for it can get a message
/// body onto a `threads` frame. The assertion is a REGRESSION GUARD against
/// exactly that future edit — someone adding an `excerpt` or a `preview` — and
/// it has no witness today because the type is what prevents it. The honest
/// statement of the claim is the type, and this is the test that notices if the
/// type stops saying it.
#[tokio::test]
async fn the_row_names_one_body_and_the_size_the_doors_will_serve() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    // A long log, so "a summary is not an enumeration" is tested against a
    // thread whose enumeration would be visible.
    let many: Vec<ThreadEvent> = (0..200)
        .map(|n| user(&format!("operator message {n}")))
        .collect();
    let id = seed(&up.manager, &cwd, &many);

    let mut c = Client::connect(up.addr).await;
    let r = c
        .request("1", &format!("shapes/{}/subscribe", "threads"), json!({"key": id}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
    let frames = c.drain().await;
    let export = frames
        .iter()
        .filter(|f| f["type"] == "shape" && f["op"] == "read" && f["key"] == json!(id))
        .last()
        .expect("an export");

    let summary = &export["row"]["body"];
    let newest = &export["row"]["newest"];
    let last = 199;
    assert_eq!(
        summary["id"],
        json!(format!("{id}.{}", newest["seq"].as_u64().unwrap())),
        "the id names the event the row's head names: {export}"
    );
    assert_eq!(
        summary["bytes"],
        json!(format!("operator message {last}").len()),
        "the size is the body's, not the payload's"
    );
    assert_eq!(summary["transports"], json!(["ws", "get"]));
    assert_eq!(
        export["cells"]["body"]["state"],
        json!("known"),
        "a readable payload is a known cell"
    );

    // **TWO OTHER CELLS ARE SPELLED `<id>.…` SINCE R2.S3d, AND NEITHER IS A
    // BODY.** `context_id` is `<id>.<last_compaction_message_id>` (the same
    // spelling as a body id, by design — `context.rs`), and `pages` names the
    // thread's message PAGES, one resource per 256 message ids. Both are taken
    // off the frame here and checked for what they are, so the count below is
    // still over every OTHER field and still fails an id list hidden in one.
    let mut export = export.clone();
    let row = export["row"].as_object_mut().expect("a row");
    let ctx = row.remove("context_id").expect("the context id cell");
    assert_eq!(ctx, json!(format!("{id}.0")), "never compacted: the first context");
    let pages = row.remove("pages").expect("the pages cell");
    assert_eq!(
        pages.as_array().map(Vec::len),
        Some(1),
        "200 messages are ONE page of 256 ids — an index of pages, not of bodies: {pages}"
    );
    assert!(
        pages[0]["id"].as_str().is_some_and(|p| p.starts_with(&format!("{id}.m"))),
        "a page id, never a body id: {pages}"
    );

    // **A SUMMARY, NOT AN ENUMERATION.** 200 bodies exist and the whole export
    // mentions exactly one id. This counts occurrences of the id PREFIX across
    // the entire frame, so an id list hidden in any field fails it.
    let whole = export.to_string();
    let ids = whole.matches(&format!("{id}.")).count();
    assert_eq!(
        ids, 1,
        "the export names {ids} resource ids for a 200-message thread; a summary \
         is a summary"
    );
    // And the CONTENT is not on the frame either — that is what the door is for.
    assert!(
        !whole.contains(&format!("operator message {last}")),
        "the body's text rode the frame the resource exists to keep it off"
    );
}

/// **AN EVENT KIND WITH NO BODY IS `known` AND `null`, NOT A ZERO-BYTE BODY.**
/// *There is no body* and *the body is empty* are two statements and this is the
/// one place they could have collapsed into one spelling.
///
/// Forced red by having the fold emit the summary only when there IS one
/// (`if let Some(s) = ... { row.body.emit(at, Some(s)) }`), which leaves the
/// PREVIOUS message's id beside a newer `newest` — a summary naming a resource
/// the row is no longer about.
///
/// ## The sibling case has no witness, and that is recorded rather than faked
///
/// `body_text_of_payload` also has an `Err` arm — a body-bearing KIND whose
/// payload carries no `text` — which makes the cell `NotHydrated` while `newest`
/// stays `Known`, and that asymmetry is the stated reason `body` is a cell of
/// its own. **It cannot be reached end to end through this crate.** The only
/// writer of `events` is `Store::append_event`, which serialises a typed
/// `ThreadEvent`, so a `user_message` row with no `text` column cannot be
/// produced by any code path here — it would have to come from an import, a
/// hand-edited database or a schema change. The arm is therefore defensive, and
/// its witness is the unit test
/// `shapes::body::tests::a_kind_with_no_body_and_a_payload_that_will_not_parse_are_two_answers`
/// plus the `not_hydrated` branch in `Threads::hydrates`. Writing an
/// integration test for it would have meant adding a raw-insert method to
/// `Store` that exists for no production caller, which is a worse trade than
/// saying so here.
#[tokio::test]
async fn an_event_kind_with_no_body_is_a_known_absence() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    let key = seed(
        &up.manager,
        &cwd,
        &[user("said"), ThreadEvent::TurnEnded { exit_code: Some(0) }],
    );

    let mut c = Client::connect(up.addr).await;
    let r = c
        .request("1", "shapes/threads/subscribe", json!({"key": key}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
    let frames = c.drain().await;
    let export = frames
        .iter()
        .filter(|f| f["type"] == "shape" && f["op"] == "read" && f["key"] == json!(key))
        .last()
        .expect("an export");
    assert_eq!(
        export["cells"]["body"]["state"],
        json!("known"),
        "a kind with no body is a COMPLETE answer about the world: {export}"
    );
    assert_eq!(
        export["row"]["body"],
        Value::Null,
        "and the absence is null, not a zero-byte summary: {export}"
    );
    assert_eq!(export["cells"]["newest"]["state"], json!("known"));
    assert_eq!(export["row"]["newest"]["kind"], json!("turn_ended"));
    // AND THE STALE ID IS GONE. The previous event DID have a body; if the fold
    // or the hydrate left its summary behind, this is where it would show.
    // `context_id` and `pages` (R2.S3d) are spelled `<key>.…` and are not body
    // ids — see the summary test above — so they are taken off first.
    let mut export = export.clone();
    if let Some(row) = export["row"].as_object_mut() {
        row.remove("context_id");
        row.remove("pages");
    }
    assert!(
        !export.to_string().contains(&format!("{key}.")),
        "no resource id may survive the head moving past the body: {export}"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. Both doors, one `fetch`
// ═════════════════════════════════════════════════════════════════════════════

/// **THE SAME ID, THE SAME BYTES, THROUGH TWO TRANSPORTS.**
///
/// Forced red by making the websocket arm resolve the id itself (a second
/// `event_payload_at` call with its own parse) instead of calling
/// `Resources::fetch`.
#[tokio::test]
async fn one_id_serves_the_same_bytes_on_the_socket_and_over_http() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    let text = "the body of a message, held once and served twice";
    let id = seed(&up.manager, &cwd, &[user(text)]);
    let seq = up.manager.store.max_seq(&id).unwrap();
    let rid = body::body_id(&id, seq).expect("spellable");

    let mut c = Client::connect(up.addr).await;

    // ── the socket ────────────────────────────────────────────────────────
    let r = c
        .request("1", &format!("resource/{rid}/read"), json!({}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
    let payload = &r["outcome"]["payload"];
    assert_eq!(payload["bytes"], json!(text.len()));
    assert_eq!(payload["content_type"], json!(body::BODY_CONTENT_TYPE));
    let inline = shapewire::mirror::inline_bytes(payload).expect("the chassis decodes it");
    assert_eq!(inline, text.as_bytes());

    // ── the HTTP door ─────────────────────────────────────────────────────
    let res = http_get(up.addr, &format!("/resource/{rid}"), &c.bearer).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    assert_eq!(res.body, text.as_bytes());
    assert_eq!(
        res.headers.get("content-type").map(String::as_str),
        Some(body::BODY_CONTENT_TYPE)
    );
    assert_eq!(
        res.headers.get("etag").map(String::as_str),
        Some(format!("\"{rid}\"").as_str()),
        "the ETag is the id, tautologically"
    );
    assert!(
        res.headers
            .get("cache-control")
            .is_some_and(|c| c.contains("immutable")),
        "a message body is `Retention::Fixed`: {:?}",
        res.headers.get("cache-control")
    );

    // AND THE TWO AGREE, which is the claim.
    assert_eq!(inline, res.body);
}

/// **AN ID FOR AN EVENT THAT DOES NOT EXIST, AND ONE THAT IS NOT AN ID, ARE
/// BOTH `unknown` — ON BOTH DOORS.** A consumer cannot distinguish the doors by
/// which ids fail either.
///
/// Forced red by answering `codes::GONE` for a seq this store does not hold —
/// which is the guess `chassis-resources.md` 2.3 says a store that cannot tell
/// the two apart must not make.
#[tokio::test]
async fn a_miss_is_the_same_typed_refusal_on_both_doors() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    let id = seed(&up.manager, &cwd, &[user("said")]);
    let mut c = Client::connect(up.addr).await;

    for bad in [
        format!("{id}.99999"),        // a seq nothing wrote
        "not-an-id".to_string(),      // no separator
        "unknown-thread.1".to_string(), // a thread nothing created
    ] {
        let r = c
            .request("1", &format!("resource/{bad}/read"), json!({}))
            .await;
        assert_eq!(
            r["outcome"]["code"],
            json!("unknown"),
            "socket, id {bad}: {r}"
        );
        let res = http_get(up.addr, &format!("/resource/{bad}"), &c.bearer).await;
        assert_eq!(res.status, 404, "http, id {bad}");
        let v: Value = serde_json::from_slice(&res.body).unwrap();
        assert_eq!(v["code"], json!("unknown"), "http, id {bad}: {v}");
    }

    // ── A BODILESS EVENT IS NOW SERVED, AND THE OLD ASSERTION HERE EXPIRED ───
    //
    // **This block asserted `unknown` for a bodiless id until 2026-10-06, and the
    // reason it gave was `"A bodiless event's id is never PUBLISHED"`.** That was
    // true of every shape that existed when it was written. `crate::shapes::log`
    // publishes a summary for rows of EVERY kind — a window of a thread's ledger
    // whose oversized rows name the door — so the premise is gone, and a door that
    // refused those ids would make the window a shape claiming something exists
    // that cannot be read.
    //
    // What replaces it is the same end-to-end check with the opposite verdict: the
    // bodiless event's own PAYLOAD, under `application/json`, the same bytes on
    // both doors. `threads.body` is untouched — it still publishes NO summary for a
    // bodiless head (`an_event_kind_with_no_body_is_a_known_absence`, above, still
    // asserts `row["body"] == null`) — so the two statements that used to be one
    // are now apart: this shape publishes no id for it, and the DOOR serves it.
    //
    // The genuine miss is still a miss, and it is the case above: an id whose
    // `(thread_id, seq)` this store does not hold.
    let seq = up.manager.store.max_seq(&id).unwrap() + 1;
    up.manager
        .store
        .append_event(&StoredEvent {
            thread_id: id.clone(),
            seq,
            message_id: None,
            ts: "2026-10-02T10:30:00Z".into(),
            event: ThreadEvent::TurnEnded { exit_code: Some(0) },
        })
        .unwrap();
    let expect = serde_json::to_string(&ThreadEvent::TurnEnded { exit_code: Some(0) }).unwrap();
    let r = c
        .request("1", &format!("resource/{id}.{seq}/read"), json!({}))
        .await;
    assert_eq!(
        r["outcome"]["type"],
        json!("success"),
        "a bodiless event's PAYLOAD is served now — `crate::shapes::log` publishes \
         an id for it, so refusing would be a summary naming the unfetchable: {r}"
    );
    let payload = &r["outcome"]["payload"];
    assert_eq!(payload["content_type"], json!(body::PAYLOAD_CONTENT_TYPE));
    let inline = shapewire::mirror::inline_bytes(payload).expect("the chassis decodes it");
    assert_eq!(
        inline,
        expect.as_bytes(),
        "the whole event, as the store holds it: {r}"
    );
    let res = http_get(up.addr, &format!("/resource/{id}.{seq}"), &c.bearer).await;
    assert_eq!(
        res.status, 200,
        "and the same verdict on the other door, through the same `fetch`"
    );
    assert_eq!(res.body, expect.as_bytes(), "and the same bytes");
}

/// **THE NO-QUERY RULE HOLDS ON THE SECOND DOOR TOO**, where a parallel
/// namespace arrives as a second PARAM rather than as `?path=`.
///
/// Forced red by having the websocket arm read `params["id"]` directly instead
/// of going through `opaque_id_from_params` — which ignores every other key, and
/// an ignored key is how `?path=` gets added later and observed to "work".
#[tokio::test]
async fn a_second_param_is_refused_rather_than_ignored_on_the_socket() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    let id = seed(&up.manager, &cwd, &[user("said")]);
    let seq = up.manager.store.max_seq(&id).unwrap();
    let rid = body::body_id(&id, seq).unwrap();
    let mut c = Client::connect(up.addr).await;

    // The control: the id alone works.
    let ok = c
        .request("1", &format!("resource/{rid}/read"), json!({}))
        .await;
    assert_eq!(ok["outcome"]["type"], json!("success"), "{ok}");

    for extra in [
        json!({"path": "/foo/bar"}),
        json!({"as": "json"}),
        json!({"thread": "x", "seq": 1}),
        json!({"range": "bytes=0-1"}),
    ] {
        let r = c
            .request("2", &format!("resource/{rid}/read"), extra.clone())
            .await;
        assert_eq!(
            r["outcome"]["code"],
            json!("malformed"),
            "{extra} must be refused, not ignored: {r}"
        );
    }

    // And the id itself cannot be spelled with a path in it — the charset is
    // what refuses `/<id>/versions` and a percent-encoded separator.
    for bad in ["a%2fb", "a?b", "a&b=c"] {
        let r = c
            .request("3", &format!("resource/{bad}/read"), json!({}))
            .await;
        assert_ne!(
            r["outcome"]["type"],
            json!("success"),
            "`{bad}` must not resolve: {r}"
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. THE RULING: the application cannot tell
// ═════════════════════════════════════════════════════════════════════════════

/// A door backed by the REAL producer, both halves, recording which was used.
struct RealDoor {
    rt: tokio::runtime::Handle,
    addr: SocketAddr,
    bearer: String,
    used: std::sync::Mutex<Vec<&'static str>>,
}

impl RealDoor {
    fn used(&self) -> Vec<&'static str> {
        self.used.lock().unwrap().clone()
    }
}

impl ResourceDoor for RealDoor {
    fn inline(&self, id: &str) -> Result<Vec<u8>, Refusal> {
        self.used.lock().unwrap().push("ws");
        let (addr, id) = (self.addr, id.to_string());
        let r = futures::executor::block_on(self.rt.spawn(async move {
            let mut c = Client::connect(addr).await;
            c.request("r", &format!("resource/{id}/read"), json!({})).await
        }))
        .expect("the request task");
        if r["outcome"]["type"] != json!("success") {
            return Err(Refusal::new(
                shapewire::envelope::codes::UNKNOWN,
                r["outcome"]["error"].as_str().unwrap_or("refused").to_string(),
            ));
        }
        shapewire::mirror::inline_bytes(&r["outcome"]["payload"])
    }

    fn by_id(&self, id: &str) -> Result<Vec<u8>, Refusal> {
        self.used.lock().unwrap().push("get");
        let (addr, bearer, id) = (self.addr, self.bearer.clone(), id.to_string());
        let res = futures::executor::block_on(self.rt.spawn(async move {
            http_get(addr, &format!("/resource/{id}"), &bearer).await
        }))
        .expect("the request task");
        if res.status != 200 {
            return Err(Refusal::new(
                shapewire::envelope::codes::UNKNOWN,
                format!("status {}", res.status),
            ));
        }
        Ok(res.body)
    }
}

/// **THE WHOLE RULING, END TO END.**
///
/// Two payloads, one small and one over the consuming chassis's inline ceiling,
/// mirrored off a real producer through a real socket. The application makes ONE
/// call — `Mirror::read` — and gets bytes. The two calls provably took different
/// doors, and nothing the application was handed says which.
///
/// Forced red by making `Mirror::choose` ignore the size (`let fits = true`),
/// which sends both payloads down the socket and collapses the two cases into
/// one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_application_reading_a_body_cannot_tell_which_transport_carried_it() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();

    // SMALL: a sentence. LARGE: comfortably over 64 KiB, which is where the
    // consuming chassis falls back — and comfortably under the producer's own
    // inline bound, so the fallback is a CHOICE and not a forced refusal.
    let small_text = "a short message".to_string();
    let large_text = "x".repeat(200 * 1024);
    let small = seed(&up.manager, &cwd, &[user(&small_text)]);
    let large = seed(&up.manager, &cwd, &[user(&large_text)]);

    let mut c = Client::connect(up.addr).await;
    let mut mirror = Mirror::new("threads");
    for key in [&small, &large] {
        let r = c
            .request("1", "shapes/threads/subscribe", json!({"key": key}))
            .await;
        assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
        for f in c.drain().await {
            if f["type"] == "shape" {
                mirror.accept(&f);
            }
        }
    }
    assert!(
        mirror.cell(&small, "body").is_known() && mirror.cell(&large, "body").is_known(),
        "both bodies must be mirrored before anything is read"
    );

    let door = RealDoor {
        rt: tokio::runtime::Handle::current(),
        addr: up.addr,
        bearer: c.bearer.clone(),
        used: std::sync::Mutex::new(Vec::new()),
    };

    // ── THE ONE CALL AN APPLICATION MAKES ─────────────────────────────────
    let got_small = tokio::task::block_in_place(|| mirror.read(&door, &small, "body"))
        .expect("the small body");
    let got_large = tokio::task::block_in_place(|| mirror.read(&door, &large, "body"))
        .expect("the large body");

    // 1. IT ANSWERED THE RIGHT BYTES, BOTH TIMES.
    assert_eq!(String::from_utf8(got_small).unwrap(), small_text);
    assert_eq!(got_large.len(), large_text.len());
    assert_eq!(String::from_utf8(got_large).unwrap(), large_text);

    // 2. AND THE TWO READS GENUINELY TOOK DIFFERENT TRANSPORTS. Without this
    //    the test proves only that one path works twice.
    assert_eq!(
        door.used(),
        vec!["ws", "get"],
        "the small body rode the socket and the large one fell back to HTTP"
    );

    // 3. AND NOTHING THE APPLICATION TOUCHED NAMED EITHER ONE. `read` answers
    //    `Vec<u8>`; the only other thing it could have learned from is the row,
    //    so the row is checked for a transport RANKING — it carries the two as
    //    a set of facts and in the same order for both payloads.
    assert_eq!(
        mirror.row(&small)["body"]["transports"],
        mirror.row(&large)["body"]["transports"],
        "the producer must state the same facts for both sizes; a list that \
         differed by size would be the producer choosing"
    );
    assert_eq!(
        mirror.row(&small)["body"]["transports"],
        json!(["ws", "get"])
    );
}

/// **AND THE PRODUCER'S OWN INLINE BOUND IS A REFUSAL, NOT A SHORT ANSWER** —
/// reached only by asking for it directly, which the chassis never does for a
/// payload this size.
///
/// Forced red by having `read_resource_inline` truncate to `MAX_WS_INLINE_BYTES`
/// instead of refusing, which is deliver-and-warn with the warning left out.
#[tokio::test]
async fn the_inline_door_refuses_an_oversized_body_and_the_other_door_serves_it() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    let text = "y".repeat(shapewire::resource::MAX_WS_INLINE_BYTES as usize + 1);
    let id = seed(&up.manager, &cwd, &[user(&text)]);
    let seq = up.manager.store.max_seq(&id).unwrap();
    let rid = body::body_id(&id, seq).unwrap();

    let mut c = Client::connect(up.addr).await;
    let r = c
        .request("1", &format!("resource/{rid}/read"), json!({}))
        .await;
    assert_eq!(r["outcome"]["code"], json!("too_large"), "{r}");
    assert!(
        r["outcome"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("The id is good"),
        "the refusal must say the id is fine and point at the other door: {r}"
    );

    // And the SAME id serves fine on the door that streams.
    let res = http_get(up.addr, &format!("/resource/{rid}"), &c.bearer).await;
    assert_eq!(res.status, 200);
    assert_eq!(res.body.len(), text.len());

    // Which is what the consuming chassis would have chosen unasked.
    let mut mirror = Mirror::new("threads");
    let r = c
        .request("2", "shapes/threads/subscribe", json!({"key": id}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
    for f in c.drain().await {
        if f["type"] == "shape" {
            mirror.accept(&f);
        }
    }
    assert_eq!(
        mirror.plan(&id, "body").unwrap(),
        Fetch::ById { id: rid },
        "the chassis never asks the inline door for a payload this size"
    );
}

/// The shape's id spelling and the chassis's id CHARSET must agree, and the
/// agreement is proved through the real door rather than by two copies of a
/// predicate.
///
/// Forced red by widening `body::is_id_byte` to accept `/`, which makes
/// `body_id` publish an id the door parses as a sub-path and 404s.
#[tokio::test]
async fn every_id_the_shape_publishes_is_one_the_door_accepts() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    // Thread ids this daemon will accept from a caller, including the awkward
    // ones: a dotted id, a `+`, a `:`, and one with every separator the charset
    // allows.
    for tid in ["plain", "bob.example.com", "a-b_c~d:e@f+g", "0"] {
        let mut s = ThreadSettings::new(cwd.clone(), "fable".to_string());
        s.injection = threads::settings::Injection::NOTHING;
        let id = up
            .manager
            .create_thread(Some(tid.to_string()), s)
            .unwrap();
        assert_eq!(id, tid);
        let base = up.manager.store.max_seq(&id).unwrap();
        up.manager
            .store
            .append_event(&StoredEvent {
                thread_id: id.clone(),
                seq: base + 1,
                message_id: Some(base + 1),
                ts: "2026-10-02T10:00:00Z".into(),
                event: user("hello"),
            })
            .unwrap();

        let mut c = Client::connect(up.addr).await;
        let r = c
            .request("1", "shapes/threads/subscribe", json!({"key": id}))
            .await;
        assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
        let published = c
            .drain()
            .await
            .into_iter()
            .filter(|f| f["type"] == "shape" && f["op"] == "read" && f["key"] == json!(id))
            .last()
            .expect("an export")["row"]["body"]["id"]
            .as_str()
            .expect("a published id")
            .to_string();

        // THE DOOR ACCEPTS WHAT THE SHAPE PUBLISHED. Both of them.
        let r = c
            .request("2", &format!("resource/{published}/read"), json!({}))
            .await;
        assert_eq!(
            r["outcome"]["type"],
            json!("success"),
            "the socket refused an id the shape published for {tid}: {r}"
        );
        let res = http_get(up.addr, &format!("/resource/{published}"), &c.bearer).await;
        assert_eq!(
            res.status, 200,
            "the HTTP door refused an id the shape published for {tid}"
        );
        assert_eq!(res.body, b"hello");
    }
}

/// **THE FOLD CLEARS THE SUMMARY WHEN THE HEAD MOVES PAST THE BODY.**
///
/// The sibling of `an_event_kind_with_no_body_is_a_known_absence`, and it exists
/// because that one came back GREEN under the mutation it was written for. That
/// test subscribes AFTER seeding, so it drives the HYDRATE and the fold's emit
/// never runs: making the fold emit only when there IS a summary left it
/// passing. The fold needs its own witness, and this is it — subscribe first,
/// then fold a body-bearing event and a bodiless one on top of it.
///
/// Forced red by `if let Some(s) = ... { row.body.emit(at, Some(s)) }` in the
/// `Push::Event` arm: `the previous message's summary survived the head moving
/// past it`. That is the live-path form of a summary naming a resource the row
/// is no longer about.
#[tokio::test]
async fn the_fold_clears_the_summary_when_the_head_moves_past_a_body() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    let id = seed(&up.manager, &cwd, &[]);

    let mut c = Client::connect(up.addr).await;
    let r = c
        .request("1", "shapes/threads/subscribe", json!({"key": id}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
    c.drain().await;

    let mut seq = up.manager.store.max_seq(&id).unwrap();
    let mut fold = |ev: ThreadEvent, s: u64| {
        let e = StoredEvent {
            thread_id: id.clone(),
            seq: s,
            message_id: Some(s),
            ts: format!("2026-10-02T13:{:02}:00Z", s % 60),
            event: ev,
        };
        up.manager.store.append_event(&e).unwrap();
        up.manager.hub.emit(threads::event::Push::Event(e));
    };

    // 1. A body arrives, and the fold publishes its summary.
    seq += 1;
    fold(user("a body the fold saw"), seq);
    let body_seq = seq;
    let rows = c.drain().await;
    let with_body = rows
        .iter()
        .filter(|f| f["type"] == "shape" && f["op"] == "live" && f["key"] == json!(id))
        .filter_map(|f| f["event"]["row"]["body"].as_object().cloned())
        .last()
        .unwrap_or_else(|| panic!("no folded summary: {rows:#?}"));
    assert_eq!(with_body["id"], json!(format!("{id}.{body_seq}")));

    // 2. A BODILESS event moves the head. The summary must go with it.
    seq += 1;
    fold(ThreadEvent::TurnEnded { exit_code: Some(0) }, seq);
    let rows = c.drain().await;
    let after = rows
        .iter()
        .filter(|f| f["type"] == "shape" && f["op"] == "live" && f["key"] == json!(id))
        .filter_map(|f| f["event"]["row"].as_object().cloned())
        .last()
        .unwrap_or_else(|| panic!("no row frame after the bodiless event: {rows:#?}"));
    assert_eq!(
        after["newest"]["kind"],
        json!("turn_ended"),
        "the head moved: {after:#?}"
    );
    assert_eq!(
        after["body"],
        Value::Null,
        "the previous message's summary survived the head moving past it"
    );
}

/// **A DEAD FOLD TAKES THE SUMMARY WITH IT, AND NOTHING IS ASKED FOR.**
///
/// The consuming half must not fetch off a cell it cannot vouch for, and this is
/// the reachable form of that: a death export flips every cell to `Dead`, so a
/// summary that was good a frame ago is no longer a statement about the world.
/// The door would still have answered — the resource is immutable and the id is
/// still valid — which is exactly why this has to be refused rather than
/// attempted. Proved against a door that PANICS if touched.
///
/// Forced red by deleting the cell-state gate in `Mirror::summary`: the panic
/// fires, `the chassis asked for a resource the shape could not vouch for`.
#[tokio::test]
async fn a_dead_fold_is_never_fetched_from() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    let id = seed(&up.manager, &cwd, &[user("said")]);

    let mut c = Client::connect(up.addr).await;
    let r = c
        .request("1", "shapes/threads/subscribe", json!({"key": id}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
    let mut mirror = Mirror::new("threads");
    for f in c.drain().await {
        if f["type"] == "shape" {
            mirror.accept(&f);
        }
    }
    assert!(
        mirror.plan(&id, "body").is_ok(),
        "the control: while the fold is alive the summary is fetchable"
    );

    // The death export, as the supervisor emits it: ONE keyless frame.
    mirror.accept(&json!({
        "shape": "threads", "op": "read", "as_of": "2026-10-02T12:00:00Z",
        "epoch": {"reinit": 0, "lost": 0, "state": "dead",
                  "dead_reason": "the shape task panicked"}
    }));
    struct Never;
    impl ResourceDoor for Never {
        fn inline(&self, _: &str) -> Result<Vec<u8>, Refusal> {
            panic!("the chassis asked for a resource the shape could not vouch for")
        }
        fn by_id(&self, _: &str) -> Result<Vec<u8>, Refusal> {
            panic!("the chassis asked for a resource the shape could not vouch for")
        }
    }
    assert!(
        matches!(
            mirror.read(&Never, &id, "body"),
            Err(ContentError::NotKnown { .. })
        ),
        "a dead fold's summary is the absence of a statement"
    );
}

/// The hydrate and the fold must publish the SAME summary for the same event.
/// Two writers behind one number is the drift every rule in `docs/shapes.md` 2
/// is against, and here the two writers are a store read and a live event.
///
/// Forced red by having the fold emit `summary(..)` off the event's `seq + 1`.
#[tokio::test]
async fn the_fold_and_the_hydrate_agree_about_the_same_body() {
    let up = start().await;
    let cwd = up._tmp.path().to_string_lossy().to_string();
    let id = seed(&up.manager, &cwd, &[user("first")]);

    let mut c = Client::connect(up.addr).await;
    let r = c
        .request("1", "shapes/threads/subscribe", json!({"key": id}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
    c.drain().await;

    // **APPENDED AND EMITTED, THE WAY `ThreadManager::emit` DOES BOTH** — the
    // store so the hydrate can read it, the hub so the fold sees it. The two
    // halves of this test must be two READINGS of one row, so there has to be
    // exactly one row.
    //
    // Deliberately NOT `ThreadManager::send`: a real send enqueues the message
    // and the scheduler starts a turn, whose `turn_started`/`turn_ended` move
    // the head past the message within one 100 ms debounce — so the row frame
    // that arrives carries `newest.kind: "turn_ended"` and a null body, which
    // is rule 2.2 coalescing working correctly and is not the thing under test.
    let text = "a message that arrived live";
    let seq = up.manager.store.max_seq(&id).unwrap() + 1;
    let ev = StoredEvent {
        thread_id: id.clone(),
        seq,
        message_id: Some(seq),
        ts: "2026-10-02T11:00:00Z".into(),
        event: user(text),
    };
    up.manager.store.append_event(&ev).unwrap();
    up.manager.hub.emit(threads::event::Push::Event(ev));
    let folded = c.drain().await;
    let live = folded
        .iter()
        .filter(|f| f["type"] == "shape" && f["op"] == "live" && f["key"] == json!(id))
        .filter_map(|f| f["event"]["row"]["body"].as_object().cloned())
        .last()
        .unwrap_or_else(|| panic!("no row frame carrying the body summary: {folded:#?}"));
    assert_eq!(live["id"], json!(format!("{id}.{seq}")));
    assert_eq!(live["bytes"], json!(text.len()));

    // Now a RE-SUBSCRIBE, which re-exports from the HYDRATE's reading of the
    // same row. The two must say the same thing.
    let mut c2 = Client::connect(up.addr).await;
    let r = c2
        .request("1", "shapes/threads/subscribe", json!({"key": id}))
        .await;
    assert_eq!(r["outcome"]["type"], json!("success"), "{r}");
    let hydrated = c2
        .drain()
        .await
        .into_iter()
        .filter(|f| f["type"] == "shape" && f["op"] == "read" && f["key"] == json!(id))
        .last()
        .expect("an export")["row"]["body"]
        .clone();
    assert_eq!(
        hydrated,
        Value::Object(live),
        "the fold and the hydrate must publish one summary for one event"
    );

    // And it fetches.
    let rid = hydrated["id"].as_str().unwrap().to_string();
    let got = c2
        .request("2", &format!("resource/{rid}/read"), json!({}))
        .await;
    assert_eq!(
        shapewire::mirror::inline_bytes(&got["outcome"]["payload"]).unwrap(),
        text.as_bytes()
    );
}

/// `threads` answered `501 unsupported` for every id before this work. It does
/// not any more — and the 501 must not come back by a producer being wired up
/// and then quietly losing its `resources()` override.
#[tokio::test]
async fn the_door_is_no_longer_unsupported_on_this_producer() {
    let up = start().await;
    let c = Client::connect(up.addr).await;
    let res = http_get(up.addr, "/resource/nothing-here", &c.bearer).await;
    assert_ne!(res.status, 501, "the door is implemented now");
    assert_eq!(res.status, 404, "and an unknown id is an unknown id");
    // `tshape` is imported for this: the shape that owns the resource is the
    // one whose name the ledger records, and a test file that named neither
    // would not notice the two coming apart.
    assert_eq!(tshape::Threads::NAME, "threads");
}

use threads::shape::Shape as _;

// R9.F2: fixture scripts are exec'd once, untimed, before any test times them.
#[path = "support/first_exec.rs"]
mod first_exec;
