//! # The resource door, over a real socket
//!
//! `GET`/`HEAD`/`PUT /resource/{id}` — the operator's REST ruling, tested where
//! a consumer meets it: raw HTTP/1.1 on an ephemeral port, asserting status
//! lines, headers and bytes rather than calling handlers.
//!
//! ## Why this file is in `threads/tests` and not in `shapewire/tests`
//!
//! `shapewire`'s `server` feature is not in its `default` set — deliberately,
//! so a CONSUMER taking the chassis for a mirror links no axum — and an
//! integration test of the resource door needs it. `threads` is the crate that
//! enables it, so this is where a test of the door RUNS. **The producer under
//! test is NOT `threads`:** it is [`Fake`], defined below, implementing
//! `shapewire::server::Producer` and `shapewire::resource::Resources`
//! over an in-memory map. The chassis gains no dependency on a producer crate
//! from this file, and `threads` declares no resource — a third dispatch owns the
//! shapes that would.
//!
//! ## Every test here was forced red, and the mutation is named in its docstring
//!
//! A green that has never failed is not evidence. This workspace has shipped
//! four tests that passed against broken code because they described the
//! implementation rather than the contract.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use shapewire::envelope::{codes, Identity, Refusal};
use shapewire::hub::{Hub, Kind};
use shapewire::resource::{
    ByteRange, Digest, Found, ResourceId, Resources, Retention, Stored, Uploads,
    MAX_UPLOAD_BYTES, RESOURCE_CHUNK_BYTES,
};
use shapewire::server::{Chassis, NoControl, NoStream, Producer};
use shapewire::wire::ConnId;
use futures::StreamExt;
use serde_json::Value;

// ═════════════════════════════════════════════════════════════════════════════
// THE FAKE PRODUCER
// ═════════════════════════════════════════════════════════════════════════════

#[derive(Clone)]
struct Nothing;
impl Kind for Nothing {
    fn kind(&self) -> &'static str {
        "nothing"
    }
}

/// How a blob's producer behaves when handed a `Range` — the axis the chassis's
/// offset check exists for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seeks {
    /// Ignores the range and hands back a reader at byte zero. Correct, slower.
    Never,
    /// Positions the reader exactly at the first byte asked for, and says so.
    Exactly,
    /// Reports an offset PAST the first byte asked for — a producer defect.
    TooFar,
    /// Declares a length longer than the bytes it has. Truncates mid-stream.
    LiesLong,
}

#[derive(Clone)]
struct Blob {
    bytes: Vec<u8>,
    retention: Retention,
    content_type: String,
    seeks: Seeks,
}

impl Blob {
    fn immutable(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.to_vec(),
            retention: Retention::Fixed,
            content_type: "text/plain".into(),
            seeks: Seeks::Never,
        }
    }
}

/// Counts what the door actually READ, which is how "HEAD reads nothing" and
/// "a GET never buffers the whole resource" become observable.
#[derive(Default)]
struct Reads {
    calls: AtomicUsize,
    bytes: AtomicU64,
    largest_single_read: AtomicUsize,
}

struct CountingReader {
    inner: std::io::Cursor<Vec<u8>>,
    reads: Arc<Reads>,
}

impl Read for CountingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.reads.calls.fetch_add(1, Ordering::AcqRel);
        self.reads.bytes.fetch_add(n as u64, Ordering::AcqRel);
        self.reads
            .largest_single_read
            .fetch_max(n, Ordering::AcqRel);
        Ok(n)
    }
}

/// **WHEN THE LIAR RUNS DRY IS THE TEST'S DECISION, NOT THE SCHEDULER'S.**
///
/// The chassis pumps a producer's reader on `spawn_blocking` into a channel, and
/// hyper polls that channel from the runtime. If the pump has queued BOTH the
/// liar's five bytes AND its short-read error before hyper first polls the body
/// — which is what a loaded machine does to a current-thread runtime — hyper
/// takes the bytes, takes the error on the very next poll, and drops the
/// connection with the head and the bytes still in its own write buffer: the
/// consumer gets an empty socket, and `raw` fails "headers must terminate".
/// That is an honest refusal too (nothing at all is not a lie), but it is not
/// the case this test is about, which is truncation AFTER the first byte.
/// Measured (R9.F2b): red in the full suite at load 5.3 (MERGE-32 cp9), and
/// red every time with the pump forced to win that race.
///
/// So the liar's end of data waits on this gate, and the test opens it only
/// once it has the head and the real bytes in hand. The bound is long (60 s)
/// and only stops a broken test from holding a blocking-pool thread forever.
#[derive(Default)]
struct DryGate {
    open: Mutex<bool>,
    cv: std::sync::Condvar,
}

impl DryGate {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
    fn wait(&self) {
        let g = self.open.lock().unwrap();
        let _ = self
            .cv
            .wait_timeout_while(g, std::time::Duration::from_secs(60), |open| !*open)
            .unwrap();
    }
}

/// A reader whose end of data waits for [`DryGate::open`].
struct HoldsItsEnd {
    inner: CountingReader,
    gate: Arc<DryGate>,
}

impl Read for HoldsItsEnd {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n == 0 && !buf.is_empty() {
            self.gate.wait();
        }
        Ok(n)
    }
}

struct Store {
    blobs: HashMap<String, Blob>,
    reads: Arc<Reads>,
    uploads: Option<Uploaded>,
    dry: Arc<DryGate>,
}

#[derive(Default)]
struct Uploaded {
    held: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl Uploads for Uploaded {
    fn digest(&self) -> Digest {
        Digest::Sha256
    }
    fn put(&self, digest: &str, bytes: axum::body::Bytes) -> Result<Stored, Refusal> {
        let mut held = self.held.lock().unwrap();
        if held.contains_key(digest) {
            return Ok(Stored::AlreadyHeld);
        }
        held.insert(digest.to_string(), bytes.to_vec());
        Ok(Stored::Created)
    }
}

impl Resources for Store {
    fn fetch(&self, id: &ResourceId, range: Option<ByteRange>) -> Result<Found, Refusal> {
        // The refusal vocabulary, one id apiece, so every code reaches the wire.
        match id.as_str() {
            "gone1" => return Err(Refusal::new(codes::GONE, "deleted at t")),
            "sup1" => return Err(Refusal::new(codes::SUPERSEDED, "superseded at t")),
            "forb1" => return Err(Refusal::new(codes::FORBIDDEN, "not yours")),
            "corrupt1" => {
                return Err(Refusal::new(
                    codes::INTEGRITY,
                    "bytes on disk do not match the declared digest",
                ))
            }
            "badtype1" => {
                return Ok(Found::whole(
                    Retention::Fixed,
                    "not a\nheader value",
                    3,
                    Box::new(std::io::Cursor::new(b"abc".to_vec())),
                ))
            }
            _ => {}
        }
        let Some(b) = self.blobs.get(id.as_str()) else {
            return Err(Refusal::new(codes::UNKNOWN, "no such resource"));
        };
        let len = match b.seeks {
            Seeks::LiesLong => b.bytes.len() as u64 + 64,
            _ => b.bytes.len() as u64,
        };
        let first = match (b.seeks, range) {
            (Seeks::Exactly, Some(ByteRange::From { first, .. })) => first,
            (Seeks::Exactly, Some(ByteRange::Suffix { len: n })) => len.saturating_sub(n),
            (Seeks::TooFar, Some(ByteRange::From { first, .. })) => first + 1,
            (Seeks::TooFar, None) => 1,
            _ => 0,
        };
        let tail = b.bytes[std::cmp::min(first as usize, b.bytes.len())..].to_vec();
        let reader = CountingReader {
            inner: std::io::Cursor::new(tail),
            reads: Arc::clone(&self.reads),
        };
        let body: Box<dyn Read + Send> = match b.seeks {
            Seeks::LiesLong => Box::new(HoldsItsEnd {
                inner: reader,
                gate: Arc::clone(&self.dry),
            }),
            _ => Box::new(reader),
        };
        Ok(Found {
            retention: b.retention,
            content_type: b.content_type.clone(),
            len,
            offset: first,
            body,
        })
    }

    fn uploads(&self) -> Option<&dyn Uploads> {
        self.uploads.as_ref().map(|u| u as &dyn Uploads)
    }
}

struct Fake {
    identity: Identity,
    hub: Hub<Nothing>,
    store: Option<Store>,
}

impl Producer for Fake {
    type Input = Nothing;
    type Stream = NoStream;
    type Control = NoControl;

    fn identity(&self) -> &Identity {
        &self.identity
    }
    fn hub(&self) -> &Hub<Nothing> {
        &self.hub
    }
    fn names(&self) -> &'static [&'static str] {
        &[]
    }
    fn handle<'a>(
        &'a self,
        method: &'a str,
        _params: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, Refusal>> + Send + 'a>>
    {
        let m = method.to_string();
        Box::pin(async move { Err(Refusal::untyped(format!("no methods: {m}"))) })
    }
    fn connect(&self, _conn: ConnId) -> (NoStream, NoControl) {
        (NoStream, NoControl)
    }
    fn resources(&self) -> Option<&dyn Resources> {
        self.store.as_ref().map(|s| s as &dyn Resources)
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// THE HARNESS — raw HTTP/1.1, so the assertions are about bytes on a socket
// ═════════════════════════════════════════════════════════════════════════════

struct Up {
    addr: SocketAddr,
    bearer: String,
    reads: Arc<Reads>,
    held: Option<Arc<Fake>>,
}

fn blobs() -> HashMap<String, Blob> {
    let mut m = HashMap::new();
    m.insert("ten".to_string(), Blob::immutable(b"0123456789"));
    m.insert("empty".to_string(), Blob::immutable(b""));
    m.insert(
        "mut1".to_string(),
        Blob {
            retention: Retention::Latest,
            ..Blob::immutable(b"mutable")
        },
    );
    m.insert(
        "ver1".to_string(),
        Blob {
            retention: Retention::All,
            ..Blob::immutable(b"versioned")
        },
    );
    m.insert(
        "seeker".to_string(),
        Blob {
            seeks: Seeks::Exactly,
            ..Blob::immutable(b"0123456789")
        },
    );
    m.insert(
        "overseek".to_string(),
        Blob {
            seeks: Seeks::TooFar,
            ..Blob::immutable(b"0123456789")
        },
    );
    m.insert(
        "liar".to_string(),
        Blob {
            seeks: Seeks::LiesLong,
            ..Blob::immutable(b"short")
        },
    );
    m.insert(
        "big".to_string(),
        Blob::immutable(&big_bytes()),
    );
    m
}

/// Eight chunks plus a remainder, so the streaming path is exercised rather than
/// the one-read case every small blob takes.
fn big_bytes() -> Vec<u8> {
    (0..(RESOURCE_CHUNK_BYTES * 8 + 777))
        .map(|i| (i % 251) as u8)
        .collect()
}

async fn start(with_resources: bool, with_uploads: bool) -> Up {
    let reads = Arc::new(Reads::default());
    let store = with_resources.then(|| Store {
        blobs: blobs(),
        reads: Arc::clone(&reads),
        uploads: with_uploads.then(Uploaded::default),
        dry: Arc::default(),
    });
    let fake = Arc::new(Fake {
        identity: Identity {
            daemon: "fake".into(),
            version: "0.0.1".into(),
            proto: 1,
            boot_id: "b0".into(),
        },
        hub: Hub::new(),
        store,
    });
    let chassis = Chassis::new(Arc::clone(&fake));
    let listener = shapewire::server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let (addr, h) = chassis
        .serve_on_with_shutdown(listener, std::future::pending::<()>())
        .unwrap();
    std::mem::forget(h);

    // The bearer comes from the handshake and from nowhere else — this is the
    // whole of "the id names the thing; the bearer grants it".
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    let welcome: Value = loop {
        match ws.next().await.unwrap().unwrap() {
            tokio_tungstenite::tungstenite::Message::Text(t) => {
                break serde_json::from_str(&t).unwrap()
            }
            _ => continue,
        }
    };
    let bearer = welcome["bearer"].as_str().expect("a bearer").to_string();
    // Hold the socket open for the life of the test: the grant's lifetime IS the
    // socket's.
    std::mem::forget(ws);
    Up {
        addr,
        bearer,
        reads,
        held: Some(fake),
    }
}

struct Res {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Res {
    fn h(&self, k: &str) -> Option<&str> {
        self.headers.get(k).map(String::as_str)
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "body is not JSON ({e}): {:?}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }
    fn code(&self) -> String {
        self.json()["code"].as_str().unwrap_or_default().to_string()
    }
}

/// One request/response over a fresh connection. `Connection: close` so the body
/// is exactly "everything to EOF" — which is what lets a 304 be asserted to have
/// NO body rather than a zero-length one inferred from a header.
async fn raw(
    addr: SocketAddr,
    method: &str,
    target: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> Res {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut head = format!("{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    s.write_all(head.as_bytes()).await.unwrap();
    s.write_all(body).await.unwrap();
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

impl Up {
    async fn get(&self, target: &str) -> Res {
        self.req("GET", target, &[], b"").await
    }
    async fn req(&self, method: &str, target: &str, extra: &[(&str, &str)], body: &[u8]) -> Res {
        let auth = format!("Bearer {}", self.bearer);
        let mut all: Vec<(&str, &str)> = vec![("Authorization", &auth)];
        all.extend_from_slice(extra);
        raw(self.addr, method, target, &all, body).await
    }
    /// No `Authorization` header at all.
    async fn anon(&self, method: &str, target: &str) -> Res {
        raw(self.addr, method, target, &[], b"").await
    }
    fn held(&self) -> BTreeMap<String, Vec<u8>> {
        self.held
            .as_ref()
            .unwrap()
            .store
            .as_ref()
            .unwrap()
            .uploads
            .as_ref()
            .unwrap()
            .held
            .lock()
            .unwrap()
            .clone()
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Digest::Sha256.hex(bytes)
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. THE NO-QUERY RULE, ON THE WIRE
// ═════════════════════════════════════════════════════════════════════════════

/// **A QUERY PARAMETER IS REFUSED, NOT IGNORED.**
///
/// The drift risk was never the transport — it was a parallel NAMESPACE. Every
/// one of `chassis-writes.md` §2.2's six tempting endpoints is a query or a
/// second path segment on this route, and each is refused here.
///
/// **Forced red** by making `opaque_id` ignore its `query` argument
/// (`if !q.is_empty()` -> `if false`): every case below answers 200 with the
/// resource's bytes, which is exactly how `?path=` would be added and observed
/// to "work" --
/// ``?path=/foo/bar` must be refused, not ignored -- body "0123456789"
/// left: 200  right: 400`
#[tokio::test]
async fn a_query_parameter_is_refused_rather_than_ignored() {
    let up = start(true, false).await;
    // The control: the SAME id, with no query, is served.
    let ok = up.get("/resource/ten").await;
    assert_eq!(ok.status, 200, "the id itself must work, or nothing below proves anything");
    assert_eq!(ok.body, b"0123456789");

    for q in [
        "?path=/foo/bar",
        "?thread=x&seq=y",
        "?as=json",
        "?inline=1",
        "?bearer=deadbeef",
    ] {
        let r = up.get(&format!("/resource/ten{q}")).await;
        assert_eq!(
            r.status, 400,
            "`{q}` must be refused, not ignored — body {:?}",
            String::from_utf8_lossy(&r.body)
        );
        assert_eq!(r.code(), codes::MALFORMED, "{q}");
        assert_ne!(r.body, b"0123456789", "a refusal never carries the bytes");
    }
    // A bare `?` is not a query and is not a refusal.
    assert_eq!(up.get("/resource/ten?").await.status, 200);
}

/// **A SECOND PATH SEGMENT IS NOT A ROUTE AND NOT AN ID.**
///
/// `/resource/<id>/versions` and `/resource/<id>/meta` are two of the six, and
/// they fail at the router: one segment is all `/resource/:id` matches.
///
/// **Forced red** by adding a wildcard sub-resource route beside this one (an
/// axum catch-all under `/resource/:id/`, pointed at the same GET handler):
/// ``/resource/ten/versions` must not be a route  left: 500  right: 404`. It is
/// a 500 rather than a 200 because a two-parameter route cannot fill this
/// handler's single-segment `Path<String>` — which is itself worth knowing: the
/// handler's shape will not quietly accept a second segment.
#[tokio::test]
async fn a_subresource_path_is_not_a_route() {
    let up = start(true, false).await;
    for t in [
        "/resource/ten/versions",
        "/resource/ten/meta",
        "/resource/latest/lineage",
    ] {
        let r = up.get(t).await;
        assert_eq!(r.status, 404, "`{t}` must not be a route");
        // axum's own 404, with no envelope in it: there is no handler, which is
        // the point — a sub-resource is not a thing this door has.
        assert!(r.body.is_empty(), "`{t}` reached a handler: {r:?}", r = r.status);
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. THE BEARER
// ═════════════════════════════════════════════════════════════════════════════

/// **A MISSING OR INVALID BEARER IS REFUSED.**
///
/// The id names the thing; the bearer grants it. Cross-consumer access is
/// explicitly not a concern — what the bearer proves is one thing, that you
/// completed the handshake on this daemon.
///
/// **Forced red** by dropping the `bearers().verify(headers)?` line from
/// `admit`: every case below answers 200 and the door is open to anyone who can
/// reach the port.
/// `assertion `left == right` failed: no Authorization header must be refused
/// left: 200  right: 401`
#[tokio::test]
async fn a_missing_or_invalid_bearer_is_refused() {
    let up = start(true, false).await;

    let none = up.anon("GET", "/resource/ten").await;
    assert_eq!(none.status, 401, "no Authorization header must be refused");
    assert_eq!(none.code(), codes::UNAUTHORIZED);
    assert!(none.body != b"0123456789");

    for bad in [
        ("Authorization", "Bearer 00000000000000000000000000000000"),
        ("Authorization", "Basic hunter2"),
        ("Authorization", "Bearer"),
        ("Authorization", ""),
        // A bearer under any other name is not a bearer.
        ("X-Bearer", "whatever"),
    ] {
        let r = raw(up.addr, "GET", "/resource/ten", &[bad], b"").await;
        assert_eq!(r.status, 401, "{bad:?} must not authenticate");
        assert_eq!(r.code(), codes::UNAUTHORIZED, "{bad:?}");
    }

    // A bearer in the QUERY STRING is not reachable at all: it is refused as a
    // query before anything looks at authentication, which is the no-query rule
    // and the header-only signature of `Bearers::verify` meeting.
    let q = raw(
        up.addr,
        "GET",
        &format!("/resource/ten?bearer={}", up.bearer),
        &[],
        b"",
    )
    .await;
    assert_eq!(q.status, 401, "a query-string bearer authenticates nothing");

    // And the real one works, so the refusals above are about the bearer.
    assert_eq!(up.get("/resource/ten").await.status, 200);
}

/// The PUT door is behind the same bearer as the GET door — one policy, two
/// methods.
///
/// **Forced red** by replacing `admit` with a bare `opaque_id` on the PUT path
/// -- the id is still validated, the bearer is not: `left: 201  right: 401`, and
/// an anonymous caller's bytes land in the store. The ORDER matters too and the
/// same test covers it: auth is checked before the `uploads()` gate, so an
/// unauthenticated PUT cannot learn whether this producer takes uploads.
#[tokio::test]
async fn the_upload_door_is_behind_the_same_bearer() {
    let up = start(true, true).await;
    let d = sha256_hex(b"hello");
    let r = raw(up.addr, "PUT", &format!("/resource/{d}"), &[], b"hello").await;
    assert_eq!(r.status, 401);
    assert_eq!(r.code(), codes::UNAUTHORIZED);
    assert!(up.held().is_empty(), "an unauthenticated PUT stored nothing");
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. RANGE
// ═════════════════════════════════════════════════════════════════════════════

/// **A RANGE OUTSIDE THE RESOURCE IS REFUSED, NEVER CLAMPED.**
///
/// A deliberate departure from RFC 9110 §14.1.1's clamp. A 206 carrying fewer
/// bytes than were asked for tells a consumer its summary's `bytes` is wrong by
/// implication, which is a second source for a number the summary states — rule
/// 2.4. 416 sends it back to the row instead.
///
/// **Forced red** by disarming the `Some(l) if l >= len` arm in
/// `ByteRange::resolve` so an over-long end falls through to RFC 9110's clamp:
/// ``bytes=0-10` over 10 bytes must be refused, not shortened -- got body ""
/// left: 206  right: 416`.
#[tokio::test]
async fn a_range_outside_the_resource_is_refused_rather_than_clamped() {
    let up = start(true, false).await;
    for spec in ["bytes=0-10", "bytes=0-99", "bytes=10-", "bytes=99-200", "bytes=-11"] {
        let r = up
            .req("GET", "/resource/ten", &[("Range", spec)], b"")
            .await;
        assert_eq!(
            r.status, 416,
            "`{spec}` over 10 bytes must be refused, not shortened — got body {:?}",
            String::from_utf8_lossy(&r.body)
        );
        assert_eq!(r.code(), codes::RANGE_UNSATISFIABLE, "{spec}");
        assert_ne!(
            r.body, b"0123456789",
            "`{spec}` was clamped: the whole resource came back under a refusal"
        );
        // The summary is the one place a resource's length is stated.
        assert!(
            r.h("content-range").is_none(),
            "a 416 must not restate the resource's length: {:?}",
            r.h("content-range")
        );
    }
}

/// A satisfiable range is served EXACTLY as asked, with the three headers that
/// make it a partial response and no others.
///
/// **Forced red** by serving `found.len` bytes regardless
/// (`let served = found.len`): the whole resource comes back under a 206 --
/// `ten bytes=0-4  left: [48, 49, 50, 51, 52, 53, 54, 55, 56, 57]
/// right: [48, 49, 50, 51, 52]`.
#[tokio::test]
async fn a_satisfiable_range_is_served_exactly() {
    let up = start(true, false).await;
    let cases: &[(&str, &[u8], &str)] = &[
        ("bytes=0-4", b"01234", "bytes 0-4/10"),
        ("bytes=5-9", b"56789", "bytes 5-9/10"),
        ("bytes=3-", b"3456789", "bytes 3-9/10"),
        ("bytes=-4", b"6789", "bytes 6-9/10"),
        ("bytes=7-7", b"7", "bytes 7-7/10"),
    ];
    for (spec, want, cr) in cases {
        for id in ["ten", "seeker"] {
            let r = up
                .req("GET", &format!("/resource/{id}"), &[("Range", spec)], b"")
                .await;
            assert_eq!(r.status, 206, "{id} {spec}");
            assert_eq!(&r.body[..], *want, "{id} {spec}");
            assert_eq!(r.h("content-range"), Some(*cr), "{id} {spec}");
            assert_eq!(
                r.h("content-length"),
                Some(want.len().to_string().as_str()),
                "{id} {spec}"
            );
            assert_eq!(r.h("accept-ranges"), Some("bytes"));
        }
    }
    // A malformed Range is a refusal and never a silent whole-resource answer.
    for bad in ["items=0-9", "bytes=9-0", "bytes=-", "bytes=0-1,4-5"] {
        let r = up.req("GET", "/resource/ten", &[("Range", bad)], b"").await;
        assert_eq!(r.status, 400, "{bad}");
        assert_eq!(r.code(), codes::MALFORMED, "{bad}");
    }
}

/// **A PRODUCER THAT SEEKS PAST THE BYTE ASKED FOR IS A DEFECT, REFUSED.**
///
/// `Found::offset` is how a producer's seek is checked rather than trusted. A
/// producer seeking too far, served under a `Content-Range` that claims
/// otherwise, is bytes plus a false claim about which bytes.
///
/// **Forced red** by disarming the `found.offset > first` branch: the
/// overseeking producer's bytes are served as if they started where the range
/// asked -- `an over-seeked reader is a producer defect  left: 206  right: 500`.
#[tokio::test]
async fn a_producer_that_seeks_past_the_range_is_refused() {
    let up = start(true, false).await;
    let r = up
        .req("GET", "/resource/overseek", &[("Range", "bytes=0-4")], b"")
        .await;
    assert_eq!(r.status, 500, "an over-seeked reader is a producer defect");
    assert_eq!(r.code(), codes::INTEGRITY);
    // And a producer that ignores the range entirely is CORRECT: the chassis
    // skips the difference itself.
    let ok = up
        .req("GET", "/resource/ten", &[("Range", "bytes=4-6")], b"")
        .await;
    assert_eq!(ok.status, 206);
    assert_eq!(ok.body, b"456");
}

// ═════════════════════════════════════════════════════════════════════════════
// 4. ETAG / IF-NONE-MATCH / CACHE-CONTROL
// ═════════════════════════════════════════════════════════════════════════════

/// **`If-None-Match` ON A MATCHING ETAG IS 304 AND SENDS NO BODY.**
///
/// The ETag is the id, tautologically — an id names one blob, so the strong
/// validator for that blob IS the id, and content-addressed caching costs
/// nothing to provide.
///
/// **Forced red** two ways, and both had to be named because either defence
/// alone holds part of the claim up:
///
/// * (a) disarm the `if_none_match_matches` branch --
///   ``"ten"` matches the validator  left: 200  right: 304`.
/// * (b) keep the 304 and hand it a `stream_body` over the reader instead of
///   dropping it. `hyper` strips a 304's body on the way out, so the BODY
///   assertion still passes and the one that catches it is the read counter:
///   `a 304 reads not one byte of the resource  left: 40  right: 10`. That is
///   the assertion worth having -- "no body on the wire" was never in danger;
///   "the producer is not asked to read the resource" was.
#[tokio::test]
async fn if_none_match_on_a_matching_etag_answers_304_with_no_body() {
    let up = start(true, false).await;
    let first = up.get("/resource/ten").await;
    assert_eq!(first.status, 200);
    let etag = first.h("etag").expect("an ETag").to_string();
    assert_eq!(etag, "\"ten\"", "the ETag IS the id");

    let before = up.reads.bytes.load(Ordering::Acquire);
    for inm in [etag.as_str(), "*", "\"zzz\", \"ten\""] {
        let r = up
            .req("GET", "/resource/ten", &[("If-None-Match", inm)], b"")
            .await;
        assert_eq!(r.status, 304, "`{inm}` matches the validator");
        assert!(
            r.body.is_empty(),
            "a 304 carries no body; got {:?}",
            String::from_utf8_lossy(&r.body)
        );
        // A 304 must not describe a body it is forbidden to send.
        assert!(r.h("content-length").is_none(), "{inm}");
        assert_eq!(r.h("etag"), Some(etag.as_str()));
        assert!(r.h("cache-control").is_some());
    }
    assert_eq!(
        up.reads.bytes.load(Ordering::Acquire),
        before,
        "a 304 reads not one byte of the resource"
    );

    // A non-matching validator is a full response, or the 304 above proves
    // nothing.
    let miss = up
        .req("GET", "/resource/ten", &[("If-None-Match", "\"zzz\"")], b"")
        .await;
    assert_eq!(miss.status, 200);
    assert_eq!(miss.body, b"0123456789");

    // A conditional RANGED request whose validator matches is a 304, never a
    // 206 — RFC 9110 §13.2.2's evaluation order.
    let cond = up
        .req(
            "GET",
            "/resource/ten",
            &[("If-None-Match", etag.as_str()), ("Range", "bytes=0-4")],
            b"",
        )
        .await;
    assert_eq!(cond.status, 304);
    assert!(cond.body.is_empty());
}

/// **A MUTABLE ID IS TOLD TO REVALIDATE; AN IMMUTABLE ONE IS NOT.**
///
/// This is the only thing the chassis does with `Retention`, and it is not
/// decorative: a cache told `immutable` about a Mutable lineage's current id
/// keeps serving bytes for an id the world has moved past and never learns it
/// 410s, which makes the cache a second source for the pointer.
///
/// **Forced red** by returning the immutable `Cache-Control` for every arm:
/// `mut1  left: Some("public, max-age=31536000, immutable")
/// right: Some("no-cache")`
#[tokio::test]
async fn cache_control_follows_the_declared_retention() {
    let up = start(true, false).await;
    for (id, want) in [
        ("ten", "public, max-age=31536000, immutable"),
        ("ver1", "public, max-age=31536000, immutable"),
        ("mut1", "no-cache"),
    ] {
        let r = up.get(&format!("/resource/{id}")).await;
        assert_eq!(r.status, 200, "{id}");
        assert_eq!(r.h("cache-control"), Some(want), "{id}");
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 5. CONTENT-LENGTH AND STREAMING
// ═════════════════════════════════════════════════════════════════════════════

/// **A RESOURCE LARGER THAN THE CHUNK ARRIVES WHOLE AND IS NEVER MATERIALISED
/// WHOLE.**
///
/// Both halves matter. The first is "streaming works"; the second is the reason
/// to stream, and it is observable because the producer's reader records the
/// largest single read it was asked for.
///
/// **Forced red** by giving the pump a 64 MiB buffer
/// (`vec![0u8; 64 * 1024 * 1024]`) so it asks for the whole resource in one
/// read: `the door never asks for more than one chunk: largest single read
/// 525065, chunk 65536`. The resource is only 525 KiB, so one read swallowed all
/// of it -- which is exactly the behaviour the chunk bound exists to prevent at
/// 2 GB.
#[tokio::test]
async fn a_large_resource_streams_in_bounded_chunks_and_arrives_whole() {
    let up = start(true, false).await;
    let want = big_bytes();
    let r = up.get("/resource/big").await;
    assert_eq!(r.status, 200);
    assert_eq!(
        r.h("content-length"),
        Some(want.len().to_string().as_str()),
        "Content-Length is the chassis's, computed from the length the range was \
         validated against"
    );
    assert_eq!(r.body.len(), want.len(), "every byte arrived");
    assert_eq!(r.body, want, "and in order");
    let largest = up.reads.largest_single_read.load(Ordering::Acquire);
    assert!(
        largest <= RESOURCE_CHUNK_BYTES && largest > 0,
        "the door never asks for more than one chunk: largest single read {largest}, \
         chunk {RESOURCE_CHUNK_BYTES}"
    );
    assert!(
        up.reads.calls.load(Ordering::Acquire) >= 8,
        "a resource of {} bytes took {} reads",
        want.len(),
        up.reads.calls.load(Ordering::Acquire)
    );
}

/// An empty resource is a 200 with `Content-Length: 0` and no range over it is
/// satisfiable.
///
/// **Forced red** by computing the served length without the empty special case
/// (`last.wrapping_sub(first).wrapping_add(1)`, where `last` is
/// `len.saturating_sub(1)` = 0 and `first` is 0): `left: Some("1")
/// right: Some("0")` -- the door promises a byte it does not have and the
/// response truncates.
#[tokio::test]
async fn an_empty_resource_is_served_and_has_no_satisfiable_range() {
    let up = start(true, false).await;
    let r = up.get("/resource/empty").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.h("content-length"), Some("0"));
    assert!(r.body.is_empty());
    let ranged = up
        .req("GET", "/resource/empty", &[("Range", "bytes=0-0")], b"")
        .await;
    assert_eq!(ranged.status, 416);
    assert_eq!(ranged.code(), codes::RANGE_UNSATISFIABLE);
}

/// **HEAD IS THE HEADERS AND NOT A READ.**
///
/// Registered explicitly rather than left to axum's HEAD-falls-back-to-GET,
/// which would run the whole pump and discard the bytes — a probe costing a full
/// resource read.
///
/// **Forced red** by deleting `.head(head_resource::<P>)` from the route: axum
/// routes HEAD to the GET handler, the pump starts, and
/// `HEAD reads nothing  left: 65536  right: 0` -- one chunk read before the
/// stripped body closed the channel, on a resource whose full read is 525 KiB.
#[tokio::test]
async fn head_answers_the_gets_headers_and_reads_nothing() {
    let up = start(true, false).await;
    let get = up.get("/resource/big").await;
    let before = up.reads.bytes.load(Ordering::Acquire);
    let head = up.req("HEAD", "/resource/big", &[], b"").await;
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty(), "HEAD sends no body");
    for k in ["content-length", "content-type", "etag", "cache-control", "accept-ranges"] {
        assert_eq!(head.h(k), get.h(k), "HEAD and GET must agree on `{k}`");
    }
    assert_eq!(
        up.reads.bytes.load(Ordering::Acquire) - before,
        0,
        "HEAD reads nothing"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// 6. PUT
// ═════════════════════════════════════════════════════════════════════════════

/// **A DIGEST WHOSE BYTES DO NOT HASH TO IT IS REFUSED, AND NOTHING IS STORED.**
///
/// Verified by the CHASSIS before the producer sees a byte, which is what makes
/// each upload atomic: `Uploads::put` cannot be reached with bytes that do not
/// hash to the name they were sent under, so a resume needs no producer session
/// state.
///
/// **Forced red** by deleting the `if got != claimed` branch:
/// `assertion `left == right` failed: a mismatched digest must be refused
/// left: 201  right: 422`, and `assertion failed: nothing was stored` fails on
/// the same mutation — the store ends up holding bytes under a name that is not
/// their digest.
#[tokio::test]
async fn a_put_whose_bytes_do_not_hash_to_its_digest_is_refused() {
    let up = start(true, true).await;
    let wrong = sha256_hex(b"something else entirely");
    let r = up
        .req("PUT", &format!("/resource/{wrong}"), &[], b"hello")
        .await;
    assert_eq!(r.status, 422, "a mismatched digest must be refused");
    assert_eq!(r.code(), codes::DIGEST_MISMATCH);
    let j = r.json();
    assert_eq!(j["expected"].as_str(), Some(wrong.as_str()));
    assert_eq!(j["got"].as_str(), Some(sha256_hex(b"hello").as_str()));
    assert!(up.held().is_empty(), "nothing was stored");

    // And the right digest is accepted, so the refusal is about the mismatch.
    let right = sha256_hex(b"hello");
    let ok = up
        .req("PUT", &format!("/resource/{right}"), &[], b"hello")
        .await;
    assert_eq!(ok.status, 201);
    assert_eq!(up.held().get(&right).map(|v| v.as_slice()), Some(&b"hello"[..]));
}

/// **IDEMPOTENT.** The second PUT of one digest is not an error; it is 200.
///
/// **Forced red** by deleting the `held.contains_key(digest)` branch from the
/// fake store so it always answers `Stored::Created`:
/// `the second PUT is AlreadyHeld  left: 201  right: 200`. That mutation is in
/// the FAKE, which is the honest place: the chassis's job is to carry the two
/// answers to two statuses, and the producer's is to know which it is.
#[tokio::test]
async fn a_put_is_idempotent_and_says_which_it_was() {
    let up = start(true, true).await;
    let d = sha256_hex(b"twice");
    let a = up.req("PUT", &format!("/resource/{d}"), &[], b"twice").await;
    assert_eq!(a.status, 201);
    assert_eq!(a.json()["digest"].as_str(), Some(d.as_str()));
    let b = up.req("PUT", &format!("/resource/{d}"), &[], b"twice").await;
    assert_eq!(b.status, 200, "the second PUT is AlreadyHeld");
    assert_eq!(up.held().len(), 1, "one blob, two PUTs");
}

/// **THE ACK IS `accepted`, AND SAYS SO — rule 2.7.**
///
/// Received and verified against the digest it was sent under. That is the whole
/// claim: not that anything references the bytes, and not that they survive a
/// crash. `durable` is opt-in through a barrier another dispatch builds, so this
/// door fsyncs nothing and puts the stage on the wire rather than leaving a 201
/// to be read as a survival promise.
///
/// **Forced red** by dropping `"stage"` from the ack:
/// `the ack names its stage: verified, not yet promised to survive a crash
/// left: None  right: Some("accepted")`. A consumer reading a bare 201 has
/// nothing to distinguish "verified and held" from "on disk", which is exactly
/// the inference 2.7 exists to stop.
#[tokio::test]
async fn the_put_ack_is_accepted_and_never_claims_durability() {
    let up = start(true, true).await;
    let d = sha256_hex(b"staged");
    for expect in [201u16, 200] {
        let r = up
            .req("PUT", &format!("/resource/{d}"), &[], b"staged")
            .await;
        assert_eq!(r.status, expect);
        assert_eq!(
            r.json()["stage"].as_str(),
            Some("accepted"),
            "the ack names its stage: verified, not yet promised to survive a crash"
        );
        // And nothing that would imply a further stage.
        assert!(
            r.json().get("durable").is_none(),
            "this door promises no durability: {}",
            r.json()
        );
    }
}

/// An id that is not a digest under the declared function is refused before a
/// byte of body is read.
///
/// **Forced red** by disarming the hex/length check: a 63-character id reaches
/// the digest comparison and answers 422 -- the consumer is told its BYTES are
/// wrong when its NAME was.
/// ``2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a488` is not a
/// sha256 digest  left: 422  right: 400`
#[tokio::test]
async fn a_put_id_that_is_not_a_digest_is_malformed_not_a_mismatch() {
    let up = start(true, true).await;
    for id in ["ten", "ABCDEF", &"z".repeat(64), &sha256_hex(b"x")[..63]] {
        let r = up.req("PUT", &format!("/resource/{id}"), &[], b"x").await;
        assert_eq!(r.status, 400, "`{id}` is not a sha256 digest");
        assert_eq!(r.code(), codes::MALFORMED, "`{id}`");
    }
    assert!(up.held().is_empty());
}

/// **A PRODUCER WHOSE IDS ARE ISSUED, NOT COMPUTED, ANSWERS 405.**
///
/// An id is never CHOSEN: it is issued by a shape or computed from the bytes. A
/// producer keyed by `(thread_id, seq)` cannot take an upload, and says so with
/// `Allow`.
///
/// **Forced red** by passing `on_write: false` at this refusal's construction,
/// which is the one thing that separates "this door will not take a write" from
/// "this producer has no door": `left: 501  right: 405`. 501 would tell a
/// consumer the resource endpoint is absent on a daemon that serves reads from
/// it perfectly well.
#[tokio::test]
async fn a_put_to_a_producer_that_issues_its_ids_is_405() {
    let up = start(true, false).await;
    let d = sha256_hex(b"hello");
    let r = up
        .req("PUT", &format!("/resource/{d}"), &[], b"hello")
        .await;
    assert_eq!(r.status, 405);
    assert_eq!(r.code(), codes::UNSUPPORTED);
    assert_eq!(r.h("allow"), Some("GET, HEAD"));
}

/// An upload past the buffer cap is refused with the limit, not truncated and
/// not stored.
///
/// **Forced red** by raising the `to_bytes` limit to `usize::MAX`: the body is
/// buffered whole, the digest matches, and `left: 201  right: 413` -- the cap
/// stops existing and an upload's size becomes the consumer's choice.
#[tokio::test]
async fn an_upload_past_the_cap_is_refused_with_its_limit() {
    let up = start(true, true).await;
    let big = vec![7u8; MAX_UPLOAD_BYTES + 1];
    let d = sha256_hex(&big);
    let r = up.req("PUT", &format!("/resource/{d}"), &[], &big).await;
    assert_eq!(r.status, 413);
    assert_eq!(r.code(), codes::TOO_LARGE);
    assert_eq!(
        r.json()["limit"].as_u64(),
        Some(MAX_UPLOAD_BYTES as u64),
        "the refusal carries what to do next"
    );
    assert!(up.held().is_empty());
}

// ═════════════════════════════════════════════════════════════════════════════
// 7. THE TYPED REFUSALS
// ═════════════════════════════════════════════════════════════════════════════

/// **EVERY REFUSAL IS DISTINGUISHABLE WITHOUT PARSING PROSE.**
///
/// `superseded` and `gone` share 410 because HTTP has one word for both, so the
/// CODE is what distinguishes them — which is the whole reason R1 exists.
///
/// **Forced red** by mapping `SUPERSEDED | GONE` to 404 in the status table:
/// `gone1  left: 404  right: 410`. The distinctness assertion at the end is the
/// other half -- it is what fails if a producer or the door ever collapses two of
/// these codes into one.
#[tokio::test]
async fn the_typed_refusals_reach_the_wire_with_their_own_codes() {
    let up = start(true, false).await;
    let cases: &[(&str, u16, &str)] = &[
        ("nosuchid", 404, codes::UNKNOWN),
        ("gone1", 410, codes::GONE),
        ("sup1", 410, codes::SUPERSEDED),
        ("forb1", 403, codes::FORBIDDEN),
        ("corrupt1", 500, codes::INTEGRITY),
    ];
    let mut seen: Vec<String> = Vec::new();
    for (id, status, code) in cases {
        let r = up.get(&format!("/resource/{id}")).await;
        assert_eq!(r.status, *status, "{id}");
        assert_eq!(r.code(), *code, "{id}");
        assert!(r.body.len() > 0, "a refusal carries its envelope");
        seen.push(r.code());
    }
    let mut uniq = seen.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), seen.len(), "two refusals share a code: {seen:?}");
    // Every code the door can answer is in the closed set.
    for c in seen {
        assert!(codes::ALL.contains(&c.as_str()), "`{c}` is not in the closed set");
    }
}

/// **A PRODUCER WITH NO RESOURCES SAYS SO, AND IT IS NOT A 404.**
///
/// `chassis-writes.md` §10.4 step 1 wanted a route "answering 404 for
/// everything"; a route whose every answer is 404 is indistinguishable from no
/// route. 501 `unsupported` is distinguishable from both a missing route and an
/// unknown id, which is what makes mounting it unconditionally honest.
///
/// **Forced red** by answering `codes::UNKNOWN` when `resources()` is `None`:
/// `left: 404  right: 501` -- a producer with no resources becomes
/// indistinguishable from one that lost a resource, and from a daemon with no
/// route at all.
#[tokio::test]
async fn a_producer_with_no_resources_refuses_honestly_rather_than_404() {
    let up = start(false, false).await;
    let r = up.get("/resource/ten").await;
    assert_eq!(r.status, 501);
    assert_eq!(r.code(), codes::UNSUPPORTED);
    // And it is a REAL route, so the answer is the envelope and not axum's empty
    // 404 — which is the difference a consumer needs.
    assert!(!r.body.is_empty());
    // The other methods answer the same way, from the same place.
    let d = sha256_hex(b"x");
    assert_eq!(
        up.req("PUT", &format!("/resource/{d}"), &[], b"x").await.status,
        501
    );
    assert_eq!(up.req("HEAD", "/resource/ten", &[], b"").await.status, 501);
    // A producer with no resources is still a producer: /health is untouched.
    let h = raw(up.addr, "GET", "/health", &[], b"").await;
    assert_eq!(h.status, 200);
    assert_eq!(h.json()["daemon"].as_str(), Some("fake"));
}

/// A producer that declares an unusable media type is refused rather than
/// silently corrected: a replaced content type is a second claim about the
/// bytes.
///
/// **Forced red** by falling back to `application/octet-stream` when
/// `HeaderValue::from_str` fails: the bytes are served under a type the producer
/// did not declare -- `left: 200  right: 500`.
#[tokio::test]
async fn a_producer_declared_content_type_that_is_not_a_header_is_refused() {
    let up = start(true, false).await;
    let r = up.get("/resource/badtype1").await;
    assert_eq!(r.status, 500);
    assert_eq!(r.code(), codes::INTEGRITY);
    assert_ne!(r.body, b"abc", "the bytes are not served under a guessed type");
}

/// **A RESOURCE SHORTER THAN ITS PRODUCER DECLARED ENDS THE BODY SHORT.**
///
/// There is no way to un-send a response, so the only refusal available after
/// the first byte is truncation against an honest `Content-Length`. A partial
/// body of a refused resource is never valid.
///
/// **Forced red** by having the pump PAD on `Ok(0)` -- send zero bytes to fill
/// the declared length instead of an `UnexpectedEof`:
/// `a truncated body is reported as such, not padded: got 69 bytes`. 69 is the
/// 5 real bytes plus 64 invented ones, delivered under a `Content-Length` that
/// matched. Padding is the worst of the three outcomes: the consumer gets bytes
/// that were never in the resource and no signal at all.
///
/// **The liar runs dry only after the head and its real bytes have arrived**
/// (R9.F2b; see [`DryGate`]). Without that ordering the test was a race between
/// the blocking-pool pump and hyper's first poll, and lost it in the full suite
/// with `headers must terminate` (MERGE-32 cp9). Forced red at aa8832a1 by
/// sleeping 300 ms in `stream_body` after the pump is spawned, so the pump wins
/// every time: `headers must terminate`; green here under the same forcing.
#[tokio::test]
async fn a_resource_shorter_than_declared_truncates_rather_than_lying() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let up = start(true, false).await;
    let dry = Arc::clone(&up.held.as_ref().unwrap().store.as_ref().unwrap().dry);
    let mut s = tokio::net::TcpStream::connect(up.addr).await.unwrap();
    s.write_all(
        format!(
            "GET /resource/liar HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
             Authorization: Bearer {}\r\nContent-Length: 0\r\n\r\n",
            up.bearer
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    // The head and the five real bytes FIRST — the liar cannot run dry until
    // they are on this side of the socket (see `DryGate`).
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let got_head_and_bytes = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                if buf.len() >= i + 4 + b"short".len() {
                    return true;
                }
            }
            match s.read(&mut chunk).await {
                Ok(0) | Err(_) => return false,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    })
    .await
    .expect("the head and the real bytes did not arrive within 30 s");
    assert!(
        got_head_and_bytes,
        "the connection ended before the head and the real bytes: {:?}",
        String::from_utf8_lossy(&buf)
    );
    dry.open();
    tokio::time::timeout(std::time::Duration::from_secs(10), s.read_to_end(&mut buf))
        .await
        .expect("the response did not finish")
        .ok(); // an abort after the first byte may surface as a reset; the bytes are what count
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let mut headers = HashMap::new();
    let mut lines = head.lines();
    let status: u16 = lines.next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let r = Res {
        status,
        headers,
        body: buf[split + 4..].to_vec(),
    };
    assert_eq!(r.status, 200, "the headers were already sent");
    assert_eq!(
        r.h("content-length"),
        Some((b"short".len() + 64).to_string().as_str())
    );
    assert!(
        r.body.len() < b"short".len() + 64,
        "a truncated body is reported as such, not padded: got {} bytes",
        r.body.len()
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// 8. THE ROUTE SURFACE
// ═════════════════════════════════════════════════════════════════════════════

/// **THE WHOLE ROUTE SURFACE, PINNED.** Three routes, and the resource one takes
/// three methods and no more.
///
/// **Forced red** by adding `.route("/resource", get(get_resource::<P>))` -- a
/// collection endpoint, which is the first step of every parallel namespace:
/// `left: 500  right: 404`.
#[tokio::test]
async fn the_router_carries_three_routes_and_the_resource_one_takes_three_methods() {
    let up = start(true, true).await;
    // The two that were there before, unchanged.
    assert_eq!(raw(up.addr, "GET", "/health", &[], b"").await.status, 200);
    // A collection endpoint does not exist. This is the one that will be
    // proposed.
    assert_eq!(up.get("/resource").await.status, 404);
    assert_eq!(up.get("/resource/").await.status, 404);
    // The three methods that do.
    assert_eq!(up.get("/resource/ten").await.status, 200);
    assert_eq!(up.req("HEAD", "/resource/ten", &[], b"").await.status, 200);
    let d = sha256_hex(b"z");
    assert_eq!(
        up.req("PUT", &format!("/resource/{d}"), &[], b"z").await.status,
        201
    );
    // And no others.
    for m in ["DELETE", "PATCH", "POST"] {
        let r = up.req(m, "/resource/ten", &[], b"").await;
        assert_eq!(r.status, 405, "{m} is not a method this door has");
    }
}
