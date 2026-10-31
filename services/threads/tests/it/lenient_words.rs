//! # Rule 2.6's leniency, in all four positions it can be needed
//!
//! *A word this code does not know is IGNORED and COUNTED, never allowed to
//! destroy the frame around it.* [`ShapeWire`] has FOUR closed, tagged enums, so
//! there are four positions where one unheard-of word can take a whole frame:
//! `event.kind`, `epoch.state`, `members.state` and `op`.
//!
//! **Two were covered and two were not.** `Mirror::accept_lenient` stripped an
//! unknown `event.kind` and substituted an unknown `epoch.state`; an unknown
//! `members.state` or `op` was a hard `serde` failure on the whole `ShapeWire`,
//! so the frame fell through to `reject::UNREADABLE` and was lost — **and with
//! it the frame's `subscribes` and `promises`.**
//!
//! Those two fields are the chassis's own bookkeeping and have nothing to do
//! with the unknown word, and rule 2.5 has no re-emit verb: a lost promise id
//! and a lost subscribe id are never re-offered except by a re-subscribe the
//! CONSUMER has to decide to issue, which means noticing that a promise did not
//! resolve and concluding it has been too long — the timeout-and-retry policy
//! rule 2.5 exists to keep out of clients. So the observable defect was a
//! subscribe that never settles, with nothing to wait on and no refusal to
//! render.
//!
//! ## Why these frames are real
//!
//! A producer at this commit cannot emit an unknown word, so the unknown word
//! has to be injected. Everything ELSE here is real and that is the point: the
//! frames are taken off a real websocket, carrying a real `subscribes` id minted
//! by a real `shapes.subscribe` and a real `promises` id minted by a real
//! command, and they are applied to a real [`Mirror`]. One word is overwritten —
//! which is exactly what a producer one version ahead of this consumer looks
//! like, and the only thing rule 2.6 is about.
//!
//! **No test here asserts that a promise book is empty.** Each asserts that an
//! id the producer minted ARRIVED at a mirror that holds the subject, which is
//! the only thing a consumer can act on.

#[path = "support/keychain.rs"]
mod keychain;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use shapewire::mirror::{Mirror, SubscribeStatus};
use shapewire::wire::reject;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use threads::config::Config;
use threads::{build_manager, ThreadManager};

// ─────────────────────────────────────────────────────────────────────────────
// fixture
// ─────────────────────────────────────────────────────────────────────────────

async fn start() -> (ThreadManager, SocketAddr, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("THREADS_CLAUDE_HOME", tmp.path().join("claude-home"));
    std::env::set_var("THREADS_RESUME_TICK_MS", "60000");
    let mut accounts = BTreeMap::new();
    accounts.insert(
        "first".to_string(),
        tmp.path().join("first").to_string_lossy().to_string(),
    );
    accounts.insert(
        "second".to_string(),
        tmp.path().join("second").to_string_lossy().to_string(),
    );
    let config = Config {
        data_dir: Some(tmp.path().to_string_lossy().to_string()),
        accounts,
        calibrate: false,
        edge_poll: false,
        probe_interval_secs: 0,
        ..Default::default()
    };
    keychain::install(); // CON-28: no real keychain (R9.F2d)
    let m = build_manager(config).unwrap();
    let chassis = threads::chassis::chassis(Arc::new(m.clone()));
    let listener = shapewire::server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let (addr, h) = chassis
        .serve_on_with_shutdown(listener, std::future::pending::<()>())
        .unwrap();
    std::mem::forget(h);
    (m, addr, tmp)
}

struct Client {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    seen: Vec<Value>,
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
            seen: Vec::new(),
        };
        c.next().await;
        c
    }
    async fn next(&mut self) -> Option<Value> {
        use tokio_tungstenite::tungstenite::Message;
        loop {
            let got =
                tokio::time::timeout(std::time::Duration::from_millis(900), self.ws.next()).await;
            match got {
                Err(_) | Ok(None) => return None,
                Ok(Some(Ok(Message::Text(t)))) => {
                    let v: Value = serde_json::from_str(&t).ok()?;
                    self.seen.push(v.clone());
                    return Some(v);
                }
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(_))) => return None,
            }
        }
    }
    async fn drain(&mut self) {
        while self.next().await.is_some() {}
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
}

/// Overwrite one word at a JSON path, panicking if the path is not there — a
/// mutation that silently did nothing would make every assertion below vacuous.
fn word(frame: &Value, path: &[&str], to: &str) -> Value {
    let mut f = frame.clone();
    let pointer = format!("/{}", path.join("/"));
    let target = f
        .pointer_mut(&pointer)
        .unwrap_or_else(|| panic!("the real frame must carry `{pointer}` for this \
                                  mutation to mean anything: {frame}"));
    assert!(
        target.is_string(),
        "`{pointer}` is a tagged enum's word, so it must be a string: {frame}"
    );
    *target = Value::String(to.to_string());
    f
}

fn ids(frame: &Value, field: &str) -> Vec<u64> {
    frame[field]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_u64)
        .collect()
}

fn codes(m: &Mirror) -> Vec<String> {
    m.rejections().iter().map(|r| r.reason.clone()).collect()
}

// ═════════════════════════════════════════════════════════════════════════════

/// **A FRAME WITH ONE WORD THIS CONSUMER DOES NOT KNOW STILL DELIVERS ITS IDS,
/// IN ALL FOUR POSITIONS.**
///
/// Four real frames off one real socket, each mutated in one position, each
/// applied to its own fresh mirror:
///
/// | position | the frame | the id it must still deliver |
/// |---|---|---|
/// | `op` | the keyed export | its `subscribes` |
/// | `members.state` | the membership page | its `subscribes` |
/// | `event.kind` | the command's `row` | its `promises` |
/// | `epoch.state` | the command's `row` | its `promises` |
///
/// A membership page carries no `promises` and cannot be made to — the only ids
/// a producer puts on one are the subscribe ids it answers — so that row of the
/// table is the subscribe id, which is also the exact symptom reported: *a
/// subscribe that never settles*.
///
/// Each case also asserts the WORD WAS COUNTED, because the alternative repair
/// — guess a value and apply the body anyway — would pass every id assertion
/// here and be the silent-divergence failure rule 2.6's soft spot 2 is about.
///
/// **THE MUTATION.** In `shapewire/src/mirror.rs::accept_lenient`, delete
/// the `op` detection block (the `if let Some(word) = raw.get("op")` arm).
///
/// Forced red:
/// ```text
/// thread 'one_unknown_word_never_costs_a_frame_its_promise_or_subscribe_ids' panicked:
/// an unknown `op` must not cost the frame its subscribe id: rule 2.5 has no
/// re-emit verb, so subscribe 1 is never offered again and the consumer waits on
/// it for ever with nothing to render. codes seen: ["unreadable"]
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_unknown_word_never_costs_a_frame_its_promise_or_subscribe_ids() {
    let (m, addr, tmp) = start().await;
    let mut c = Client::connect(addr).await;

    // ── A REAL KEYED EXPORT, CARRYING A REAL SUBSCRIBE ID ────────────────────
    let r = c
        .request("1", "shapes/accounts/subscribe", json!({"key":"second"}))
        .await;
    let keyed_sub = r["outcome"]["payload"]["subscribe_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("a subscribe acknowledges with its id: {r}"));
    c.drain().await;
    let export = c
        .seen
        .iter()
        .find(|f| {
            f["type"] == "shape"
                && f["op"] == json!("read")
                && ids(f, "subscribes").contains(&keyed_sub)
        })
        .cloned()
        .unwrap_or_else(|| panic!("the export that names subscribe {keyed_sub}"));

    // ── A REAL MEMBERSHIP PAGE, CARRYING A REAL SUBSCRIBE ID ─────────────────
    let r = c
        .request("2", "shapes/accounts/subscribe", json!({"detail":"keys"}))
        .await;
    let member_sub = r["outcome"]["payload"]["subscribe_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("{r}"));
    c.drain().await;
    let page = c
        .seen
        .iter()
        .find(|f| f["type"] == "shape" && f.get("members").is_some())
        .cloned()
        .unwrap_or_else(|| panic!("a membership page for subscribe {member_sub}"));
    let page_sub = *ids(&page, "subscribes")
        .first()
        .unwrap_or_else(|| panic!("the page must name the subscribe it answers: {page}"));

    // ── A REAL LIVE FRAME, CARRYING A REAL PROMISE ID ────────────────────────
    //
    // `accounts/{a}/disable` is a command (rule 2.4), so it acknowledges with a
    // promise id, and the resolving fact is a `row` for the account it named —
    // which this connection holds, because of the keyed subscribe above.
    let ack = c
        .request("3", "accounts/second/disable", json!({}))
        .await;
    let promise = ack["outcome"]["payload"]["promise_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("a command acknowledges with a promise id: {ack}"));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    c.drain().await;
    let live = c
        .seen
        .iter()
        .find(|f| {
            f["type"] == "shape"
                && f["op"] == json!("live")
                && ids(f, "promises").contains(&promise)
        })
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "a frame carrying promise {promise} must have reached this \
                 connection — without it there is no id for the leniency to lose"
            )
        });

    // ── 1. AN UNKNOWN `op` ───────────────────────────────────────────────────
    let mut mirror = Mirror::new("accounts");
    let rec = mirror.accept(&word(&export, &["op"], "projection"));
    assert_eq!(
        mirror.subscribe(keyed_sub),
        SubscribeStatus::Complete,
        "an unknown `op` must not cost the frame its subscribe id: rule 2.5 has \
         no re-emit verb, so subscribe {keyed_sub} is never offered again and the \
         consumer waits on it for ever with nothing to render. codes seen: {:?}",
        codes(&mirror)
    );
    assert!(
        codes(&mirror).contains(&reject::UNKNOWN_OP.to_string()),
        "and the word is COUNTED — a consumer that silently believed a body it \
         could not read would be worse than one that lost the frame: {:?}",
        codes(&mirror)
    );
    assert!(
        mirror.held("second").is_none(),
        "and NO BODY was applied: `replace one key` and `append an occurrence` \
         are what an `op` chooses between and there is no safe third guess"
    );
    assert!(rec.applied(), "the frame landed in part: {rec:?}");

    // ── 2. AN UNKNOWN `members.state` ────────────────────────────────────────
    let mut mirror = Mirror::new("accounts");
    mirror.accept(&export);
    let roster_before = mirror.members().keys();
    mirror.accept(&word(&page, &["members", "state"], "sampled"));
    assert_eq!(
        mirror.subscribe(page_sub),
        SubscribeStatus::Complete,
        "an unknown `members.state` must not cost the page its subscribe id — \
         this is the reported symptom: a `detail: keys` subscribe that never \
         settles. codes seen: {:?}",
        codes(&mirror)
    );
    assert!(
        codes(&mirror).contains(&reject::UNKNOWN_MEMBERS_STATE.to_string()),
        "{:?}",
        codes(&mirror)
    );
    assert_eq!(
        mirror.members().keys(),
        roster_before,
        "and THE ROSTER THIS CONSUMER HOLDS STANDS: an unheard-of word is not \
         `here are the keys` and not `I cannot enumerate`, so inventing either \
         would be a claim about a pool nobody made"
    );

    // ── 3. AN UNKNOWN `event.kind` ───────────────────────────────────────────
    let mut mirror = Mirror::new("accounts");
    mirror.accept(&export);
    mirror.accept(&word(&live, &["event", "kind"], "sideways"));
    assert!(
        mirror.resolved(promise),
        "an unknown `event.kind` must not cost the frame its promise id — the \
         book on the producing side has already closed it, so no later frame \
         carries it. codes seen: {:?}",
        codes(&mirror)
    );
    assert!(
        codes(&mirror).contains(&reject::UNKNOWN_KIND.to_string()),
        "{:?}",
        codes(&mirror)
    );

    // ── 4. AN UNKNOWN `epoch.state` ──────────────────────────────────────────
    let mut mirror = Mirror::new("accounts");
    mirror.accept(&export);
    mirror.accept(&word(&live, &["epoch", "state"], "degraded"));
    assert!(
        mirror.resolved(promise),
        "an unknown `epoch.state` must not cost the frame its promise id: {:?}",
        codes(&mirror)
    );
    assert!(
        codes(&mirror).contains(&reject::UNKNOWN_SHAPE_STATE.to_string()),
        "{:?}",
        codes(&mirror)
    );

    // ── AND NONE OF THE FOUR IS `unreadable` ─────────────────────────────────
    //
    // `UNREADABLE` is the last resort — *the frame did not parse at all and no
    // single unknown word explains it*. Seeing it in any of the four cases above
    // would mean the position is still uncovered, whatever else passed.
    for (what, f) in [
        ("op", word(&export, &["op"], "projection")),
        ("members.state", word(&page, &["members", "state"], "sampled")),
        ("event.kind", word(&live, &["event", "kind"], "sideways")),
        ("epoch.state", word(&live, &["epoch", "state"], "degraded")),
    ] {
        let mut mirror = Mirror::new("accounts");
        let rec = mirror.accept(&f);
        assert!(
            !codes(&mirror).contains(&reject::UNREADABLE.to_string()),
            "`{what}` is a position rule 2.6 covers, so the frame must not be \
             UNREADABLE: {rec:?}"
        );
    }

    let _ = &m;
    drop(tmp);
}
