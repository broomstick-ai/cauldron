//! **THE HANDSHAKE'S IDENTITY PATHS, END TO END THROUGH THE REAL DOOR**
//! (R4.S14 pass b; R4.S1 spec §2.4 items 3-5, §2.5, §3.7).
//!
//! Every test here stands a chassis up on an EPHEMERAL port (`127.0.0.1:0`),
//! speaks to it over a real websocket, and tears it down. Nothing here dials a
//! fixed port, reads `~/.cauldron` or any environment variable.
//!
//! Two producers, because the gate has two populations and both are owed a
//! test (doctrine 10): `Strict` implements every identity hook the way the
//! soup daemon will (a key, a harness token, a consumer that must state its
//! build, the one fixed refusal of §3.7); `Plain` implements NONE of them and
//! stands in for every producer on the machine today (threads), whose bytes
//! must not move.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use shapewire::envelope::{codes, Identity, Principal, PrincipalClaim, Refusal};
use shapewire::hub::{Hub, Kind};
use shapewire::ledger::ConsumerIdentity;
use shapewire::server::{
    verify_run_token_signature, Caller, Chassis, Identities, NoControl, NoStream, Producer,
    CREDENTIALS_NOT_ACCEPTED,
};
use shapewire::token::{Claims, Key};
use shapewire::ConnId;
use tokio_tungstenite::tungstenite::Message;

type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

const SECRET: &str = "s3cret-for-the-test";
const HARNESS_TOKEN: &str = "harness-token-minted-per-boot";
/// §3.7's one refused-credential frame, byte for byte.
const ONE_ACK: &str = r#"{"type":"auth_ack","ok":false,"error":{"code":"unauthorized","message":"unauthorized: credentials not accepted"}}"#;

#[derive(Clone)]
struct Ev;
impl Kind for Ev {
    fn kind(&self) -> &'static str {
        "ev"
    }
}

fn identity(daemon: &str) -> Identity {
    Identity {
        daemon: daemon.into(),
        version: "0.0.0-test".into(),
        proto: 1,
        boot_id: "boot-test".into(),
    }
}

fn claims(thread: &str) -> Claims {
    Claims {
        thread_id: thread.into(),
        run_id: format!("{thread}/1"),
        root: thread.into(),
        depth: 0,
        turn: "cookie-1".into(),
    }
}

fn unauthorized() -> Refusal {
    Refusal::new(codes::UNAUTHORIZED, CREDENTIALS_NOT_ACCEPTED)
}

// ── The producer that reads the caller ───────────────────────────────────────

struct Strict {
    id: Identity,
    hub: Hub<Ev>,
    key: Key,
    principal_calls: AtomicUsize,
}

impl Strict {
    fn new(key: Key) -> Self {
        Self {
            id: identity("strict"),
            hub: Hub::new(),
            key,
            principal_calls: AtomicUsize::new(0),
        }
    }
}

impl Producer for Strict {
    type Input = Ev;
    type Stream = NoStream;
    type Control = NoControl;
    fn identity(&self) -> &Identity {
        &self.id
    }
    fn hub(&self) -> &Hub<Ev> {
        &self.hub
    }
    fn secret(&self) -> Option<String> {
        Some(SECRET.into())
    }
    fn names(&self) -> &'static [&'static str] {
        &["caller/get"]
    }
    fn handle<'a>(&'a self, _: &'a str, _: Value) -> BoxFut<'a, Result<Value, Refusal>> {
        Box::pin(async { Err(Refusal::untyped("this producer reads the caller")) })
    }
    fn handle_as<'a>(
        &'a self,
        caller: &'a Caller<'a>,
        _method: &'a str,
        _params: Value,
    ) -> BoxFut<'a, Result<Value, Refusal>> {
        let out = json!({
            "conn": caller.conn,
            "thread": caller.claims.map(|c| c.thread_id.clone()),
            "consumer": caller.consumer.map(|c| c.name.clone()),
            "build": caller.consumer.and_then(|c| c.build.clone()),
            "principal": caller.principal.map(|p| serde_json::to_value(p).unwrap()),
            "peer": caller.peer.map(|a| a.to_string()),
        });
        Box::pin(async move { Ok(out) })
    }
    fn connect(&self, _: ConnId) -> (NoStream, NoControl) {
        (NoStream, NoControl)
    }
    fn verify_run_token(&self, token: &str) -> Result<Claims, Refusal> {
        verify_run_token_signature(&self.key, token).map_err(|_| unauthorized())
    }
    fn verify_principal(&self, claim: &PrincipalClaim) -> Result<Principal, Refusal> {
        self.principal_calls.fetch_add(1, SeqCst);
        if claim.expose_token() == HARNESS_TOKEN {
            Ok(claim.principal())
        } else {
            Err(unauthorized())
        }
    }
    fn check_consumer(&self, consumer: Option<&ConsumerIdentity>) -> Result<(), Refusal> {
        match consumer {
            Some(c) if c.build.is_some() => Ok(()),
            _ => Err(Refusal::malformed(
                "`consumer` with `build` is required at `auth`",
            )),
        }
    }
    fn credential_refusal(&self) -> Option<Refusal> {
        Some(unauthorized())
    }
}

// ── The producer that implements none of it (threads, today) ─────────────────

struct Plain {
    id: Identity,
    hub: Hub<Ev>,
}

impl Producer for Plain {
    type Input = Ev;
    type Stream = NoStream;
    type Control = NoControl;
    fn identity(&self) -> &Identity {
        &self.id
    }
    fn hub(&self) -> &Hub<Ev> {
        &self.hub
    }
    fn secret(&self) -> Option<String> {
        Some(SECRET.into())
    }
    fn names(&self) -> &'static [&'static str] {
        &["caller/get"]
    }
    fn handle<'a>(&'a self, _: &'a str, _: Value) -> BoxFut<'a, Result<Value, Refusal>> {
        Box::pin(async { Ok(json!({ "via": "handle" })) })
    }
    fn connect(&self, _: ConnId) -> (NoStream, NoControl) {
        (NoStream, NoControl)
    }
}

// ── Harness ──────────────────────────────────────────────────────────────────

struct Served {
    addr: SocketAddr,
    identities: Arc<Identities>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve<P: Producer>(p: Arc<P>) -> Served {
    let chassis = Chassis::new(p);
    let identities = Arc::clone(chassis.identities());
    let listener = shapewire::server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let (addr, task) = chassis
        .serve_on_with_shutdown(listener, std::future::pending())
        .unwrap();
    assert_ne!(addr.port(), 0);
    Served {
        addr,
        identities,
        task,
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Client(Ws);

impl Client {
    async fn connect(s: &Served) -> (Client, Value) {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{}/ws", s.addr))
            .await
            .unwrap();
        let mut c = Client(ws);
        let welcome = c.json().await.expect("a welcome");
        assert_eq!(welcome["type"], "welcome");
        (c, welcome)
    }

    async fn send(&mut self, v: Value) {
        self.0.send(Message::Text(v.to_string())).await.unwrap();
    }

    /// The next TEXT frame as it arrived, or `None` when the socket closed.
    async fn text(&mut self) -> Option<String> {
        loop {
            match tokio::time::timeout(Duration::from_secs(10), self.0.next()).await {
                Err(_) => panic!("no frame within 10s"),
                Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_)))) => return None,
                Ok(Some(Ok(Message::Text(t)))) => return Some(t),
                Ok(Some(Ok(_))) => continue,
            }
        }
    }

    async fn json(&mut self) -> Option<Value> {
        self.text().await.map(|t| serde_json::from_str(&t).unwrap())
    }

    /// Assert the server closed this socket (no further text frame).
    async fn closed(&mut self) {
        let next = self.text().await;
        assert!(next.is_none(), "the socket must be closed, got {next:?}");
    }

    async fn call(&mut self, id: &str) -> Value {
        self.send(json!({"type":"request","id":id,"method":"caller/get"}))
            .await;
        let r = self.json().await.expect("a response");
        assert_eq!(r["correlate"], id, "{r}");
        r["outcome"].clone()
    }
}

fn consumer() -> Value {
    json!({"name":"pen","shape_version":"v6","build":"build-abc"})
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// **AN AGENT LOGS IN WITH ITS RUN TOKEN AND NO SECRET, AND THE CALLER IS THE
/// THREAD THE TOKEN WAS MINTED FOR.** Two tokens for two DIFFERENT threads
/// give two different answers, so the thread cannot be coming from anywhere
/// but the verified claims (a constant would pass one and fail the other).
/// Paired with a token minted under another key: the one ack, then close.
#[tokio::test]
async fn an_agent_logs_in_by_run_token_and_handle_as_sees_its_thread() {
    let key = Key::from_bytes([3u8; 32]);
    let s = serve(Arc::new(Strict::new(key.clone()))).await;

    for thread in ["thread-A", "thread-B"] {
        let (mut c, welcome) = Client::connect(&s).await;
        assert_eq!(welcome["auth_required"], true);
        c.send(json!({"type":"auth","consumer":consumer(),"run_token":key.mint(&claims(thread))}))
            .await;
        let ack = c.json().await.expect("an ack");
        assert_eq!(ack["ok"], true, "{ack}");
        assert!(
            ack["bearer"].is_string() && ack.get("error").is_none(),
            "{ack}"
        );
        let out = c.call("1").await;
        assert_eq!(out["type"], "success", "{out}");
        assert_eq!(out["payload"]["thread"], thread, "{out}");
        assert_eq!(out["payload"]["build"], "build-abc", "{out}");
        assert_eq!(out["payload"]["principal"], Value::Null, "{out}");
    }

    let forged = Key::from_bytes([4u8; 32]).mint(&claims("thread-A"));
    let (mut c, _) = Client::connect(&s).await;
    c.send(json!({"type":"auth","consumer":consumer(),"run_token":forged}))
        .await;
    assert_eq!(c.text().await.as_deref(), Some(ONE_ACK));
    c.closed().await;
}

/// **ONE ACK FOR EVERY REFUSED CREDENTIAL** (§3.7), byte for byte, then
/// close: a wrong secret, a malformed token, a forged token, a right secret
/// beside a forged token, a harness principal without the secret, and a
/// harness principal whose token is wrong. The principal-without-secret case
/// must never reach `verify_principal`.
#[tokio::test]
async fn every_refused_credential_is_the_one_ack_and_a_close() {
    let key = Key::from_bytes([3u8; 32]);
    let p = Arc::new(Strict::new(key.clone()));
    let s = serve(Arc::clone(&p)).await;
    let forged = Key::from_bytes([9u8; 32]).mint(&claims("thread-A"));
    let harness = |tok: &str| json!({"kind":"harness","operation":"migrate","token":tok});

    let frames = [
        (
            "wrong secret",
            json!({"type":"auth","secret":"wrong","consumer":consumer()}),
        ),
        (
            "malformed token",
            json!({"type":"auth","consumer":consumer(),"run_token":"not-a-token"}),
        ),
        (
            "forged token",
            json!({"type":"auth","consumer":consumer(),"run_token":forged}),
        ),
        (
            "wrong secret beside a GENUINE token",
            json!({"type":"auth","secret":"wrong","consumer":consumer(),"run_token":key.mint(&claims("t"))}),
        ),
        (
            "right secret beside a forged token",
            json!({"type":"auth","secret":SECRET,"consumer":consumer(),"run_token":forged}),
        ),
        (
            "harness principal, no secret",
            json!({"type":"auth","consumer":consumer(),"principal":harness(HARNESS_TOKEN)}),
        ),
    ];
    for (why, frame) in frames {
        let (mut c, _) = Client::connect(&s).await;
        c.send(frame).await;
        assert_eq!(c.text().await.as_deref(), Some(ONE_ACK), "{why}");
        c.closed().await;
    }
    assert_eq!(
        p.principal_calls.load(SeqCst),
        0,
        "a principal without the secret must be refused BEFORE verify_principal"
    );

    let (mut c, _) = Client::connect(&s).await;
    c.send(
        json!({"type":"auth","secret":SECRET,"consumer":consumer(),"principal":harness("not-it")}),
    )
    .await;
    assert_eq!(
        c.text().await.as_deref(),
        Some(ONE_ACK),
        "wrong harness token"
    );
    c.closed().await;
    assert_eq!(p.principal_calls.load(SeqCst), 1);
}

/// **THE NON-AGENT PATHS ADMIT WHAT THEY PROVE AND NOTHING MORE.** A reader
/// (secret + consumer) has no thread and no principal; a harness principal
/// with the secret is visible to `handle_as` with its token gone. A consumer
/// without `build` is refused `malformed` and closed — the producer's rule,
/// asked after the secret.
#[tokio::test]
async fn readers_and_harness_principals_and_the_consumer_rule() {
    let s = serve(Arc::new(Strict::new(Key::from_bytes([3u8; 32])))).await;

    let (mut c, _) = Client::connect(&s).await;
    c.send(json!({"type":"auth","secret":SECRET,"consumer":consumer()}))
        .await;
    assert_eq!(c.json().await.unwrap()["ok"], true);
    let out = c.call("r").await;
    assert_eq!(out["payload"]["thread"], Value::Null, "{out}");
    assert_eq!(out["payload"]["principal"], Value::Null, "{out}");
    assert_eq!(out["payload"]["consumer"], "pen", "{out}");

    let (mut c, _) = Client::connect(&s).await;
    c.send(json!({"type":"auth","secret":SECRET,"consumer":consumer(),
        "principal":{"kind":"harness","operation":"checkpoint","token":HARNESS_TOKEN}}))
        .await;
    assert_eq!(c.json().await.unwrap()["ok"], true);
    let out = c.call("h").await;
    assert_eq!(
        out["payload"]["principal"],
        json!({"kind":"harness","operation":"checkpoint"}),
        "{out}"
    );
    assert!(!out.to_string().contains(HARNESS_TOKEN), "{out}");

    // A supervised service (the ripple runner) is
    // a principal of its own kind, and carries no ripple step: nothing about
    // a step is a connection property any more.
    let (mut c, _) = Client::connect(&s).await;
    c.send(json!({"type":"auth","secret":SECRET,"consumer":consumer(),
        "principal":{"kind":"service","service":"ripple-runner","token":HARNESS_TOKEN}}))
        .await;
    assert_eq!(c.json().await.unwrap()["ok"], true);
    let out = c.call("sv").await;
    assert_eq!(
        out["payload"]["principal"],
        json!({"kind":"service","service":"ripple-runner"}),
        "{out}"
    );
    assert!(!out.to_string().contains(HARNESS_TOKEN), "{out}");

    let (mut c, _) = Client::connect(&s).await;
    c.send(json!({"type":"auth","secret":SECRET,"consumer":{"name":"pen","shape_version":"v6"}}))
        .await;
    let ack = c.json().await.unwrap();
    assert_eq!(ack["ok"], false, "{ack}");
    assert_eq!(ack["error"]["code"], "malformed", "{ack}");
    c.closed().await;
}

/// **A PRODUCER THAT IMPLEMENTS NONE OF THE HOOKS IS UNCHANGED ON EVERY PATH
/// IT HAD, AND REFUSES THE NEW ONES.** The wrong-secret ack is still exactly
/// `{"type":"auth_ack","ok":false}`; the right one carries a bearer and no
/// `error` key; a request reaches `handle` through `handle_as`'s default. A
/// run token or a principal is refused `unsupported` and closed — the hooks'
/// refusing defaults, carried into the ack.
#[tokio::test]
async fn a_producer_with_no_identity_hooks_keeps_its_bytes() {
    let s = serve(Arc::new(Plain {
        id: identity("plain"),
        hub: Hub::new(),
    }))
    .await;

    let (mut c, _) = Client::connect(&s).await;
    c.send(json!({"type":"auth","secret":"wrong"})).await;
    assert_eq!(
        c.text().await.as_deref(),
        Some(r#"{"type":"auth_ack","ok":false}"#)
    );
    c.closed().await;

    let (mut c, _) = Client::connect(&s).await;
    c.send(json!({"type":"auth","secret":SECRET})).await;
    let ack = c.text().await.unwrap();
    assert!(
        ack.starts_with(r#"{"type":"auth_ack","ok":true,"bearer":""#),
        "{ack}"
    );
    assert!(!ack.contains("error"), "{ack}");
    let out = c.call("1").await;
    assert_eq!(out, json!({"type":"success","payload":{"via":"handle"}}));

    for (why, frame) in [
        (
            "run token",
            json!({"type":"auth","run_token":Key::from_bytes([3u8; 32]).mint(&claims("t"))}),
        ),
        (
            "principal with the secret",
            json!({"type":"auth","secret":SECRET,"principal":{"kind":"harness","operation":"o","token":"t"}}),
        ),
    ] {
        let (mut c, _) = Client::connect(&s).await;
        c.send(frame).await;
        let ack = c.json().await.unwrap();
        assert_eq!(ack["ok"], false, "{why}: {ack}");
        assert_eq!(ack["error"]["code"], "unsupported", "{why}: {ack}");
        c.closed().await;
    }
}

/// **A FRAME WITH NO CREDENTIAL IS `Invalid` AND THE SOCKET STAYS OPEN**, as
/// before — so the same socket can then authenticate. Before `auth` succeeds
/// the door answers ANY non-`auth` frame, an `Invalid` one included, with the
/// pre-auth `unauthorized` RESPONSE (not an `auth_ack`, not a close): that is
/// the gate's existing arm order and it is unchanged. This is what R4.S1 §3.7
/// now says (corrected gen 20, harness sv1/R4.S15 bb269c7d: an authentication
/// failure is `unauthorized`, the not-authenticated arm runs before parse,
/// socket left open).
#[tokio::test]
async fn a_credentialless_auth_frame_is_invalid_and_leaves_the_socket_open() {
    let key = Key::from_bytes([3u8; 32]);
    let s = serve(Arc::new(Strict::new(key.clone()))).await;
    let (mut c, _) = Client::connect(&s).await;
    c.send(json!({"type":"auth","consumer":consumer()})).await;
    let r = c.json().await.expect("an Invalid response, not a close");
    assert_eq!(r["type"], "response", "{r}");
    assert_eq!(r["outcome"]["code"], "unauthorized", "{r}");
    c.send(json!({"type":"auth","consumer":consumer(),"run_token":key.mint(&claims("thread-C"))}))
        .await;
    assert_eq!(c.json().await.unwrap()["ok"], true);
    assert_eq!(c.call("1").await["payload"]["thread"], "thread-C");
}

/// **THE IDENTITY IS THE CONNECTION'S, FOR ITS LIFE.** It is in the registry
/// while the socket is open (the sentinel: the count is genuinely 1 and names
/// the thread) and gone after the client drops it. A second `auth` proving the
/// same thing gets the same bearer; one proving a DIFFERENT thread is refused
/// `malformed` and closed.
#[tokio::test]
async fn the_identity_is_registered_for_the_connection_and_revoked_with_it() {
    let key = Key::from_bytes([3u8; 32]);
    let s = serve(Arc::new(Strict::new(key.clone()))).await;
    let a = key.mint(&claims("thread-A"));
    let auth = |t: &str| json!({"type":"auth","consumer":consumer(),"run_token":t});

    let (mut c, _) = Client::connect(&s).await;
    c.send(auth(&a)).await;
    let first = c.json().await.unwrap();
    assert_eq!(first["ok"], true);
    let conn = c.call("1").await["payload"]["conn"].as_u64().unwrap();
    assert_eq!(s.identities.live(), 1);
    assert_eq!(
        s.identities
            .get(conn)
            .and_then(|i| i.claims)
            .map(|c| c.thread_id),
        Some("thread-A".to_string())
    );

    c.send(auth(&a)).await;
    let again = c.json().await.unwrap();
    assert_eq!(
        (again["ok"].clone(), again["bearer"].clone()),
        (json!(true), first["bearer"].clone())
    );

    c.send(auth(&key.mint(&claims("thread-B")))).await;
    let other = c.json().await.unwrap();
    assert_eq!(other["ok"], false, "{other}");
    assert_eq!(other["error"]["code"], "malformed", "{other}");
    c.closed().await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while s.identities.live() != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the identity outlived its connection"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(s.identities.get(conn).is_none());
}

/// **`Caller.peer` IS THE ADDRESS THE LISTENER ACCEPTED** (R6.S0 open item 13,
/// decided gen 20). Two clients dial from two sockets whose LOCAL addresses
/// the test chose and read back before connecting; each must see exactly its
/// own, so a constant (or the server's own address, or the first client's)
/// fails one of them. The control: a router mounted WITHOUT connect info
/// (`into_router` on a bare `axum::serve`, as threads mounts it) still serves,
/// with `peer` absent rather than a refused upgrade.
#[tokio::test]
async fn the_caller_peer_is_the_address_the_listener_accepted() {
    let key = Key::from_bytes([3u8; 32]);
    let s = serve(Arc::new(Strict::new(key.clone()))).await;

    let mut seen = Vec::new();
    for thread in ["thread-P", "thread-Q"] {
        let sock = tokio::net::TcpSocket::new_v4().unwrap();
        sock.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let local = sock.local_addr().unwrap();
        let tcp = sock.connect(s.addr).await.unwrap();
        let (ws, _) = tokio_tungstenite::client_async(
            format!("ws://{}/ws", s.addr),
            tokio_tungstenite::MaybeTlsStream::Plain(tcp),
        )
        .await
        .unwrap();
        let mut c = Client(ws);
        assert_eq!(c.json().await.unwrap()["type"], "welcome");
        c.send(json!({"type":"auth","consumer":consumer(),"run_token":key.mint(&claims(thread))}))
            .await;
        assert_eq!(c.json().await.unwrap()["ok"], true);
        let out = c.call("p").await;
        assert_eq!(out["payload"]["peer"], local.to_string(), "{out}");
        seen.push(local);
    }
    assert_ne!(
        seen[0], seen[1],
        "the two dials must be two different peers"
    );
    assert_ne!(
        seen[0], s.addr,
        "a peer is never the listener's own address"
    );

    // The control: no connect info, so no peer — and the door still works.
    let chassis = Chassis::new(Arc::new(Strict::new(key.clone())));
    let app = chassis.into_router();
    let listener = shapewire::server::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let bare_addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum_serve(listener, app).await;
    });
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{bare_addr}/ws"))
        .await
        .unwrap();
    let mut c = Client(ws);
    assert_eq!(c.json().await.unwrap()["type"], "welcome");
    c.send(json!({"type":"auth","consumer":consumer(),"run_token":key.mint(&claims("thread-R"))}))
        .await;
    assert_eq!(c.json().await.unwrap()["ok"], true);
    let out = c.call("b").await;
    assert_eq!(out["type"], "success", "{out}");
    assert_eq!(out["payload"]["peer"], Value::Null, "{out}");
    task.abort();
}

/// A bare `axum::serve` of a router, without connect info.
async fn axum_serve(listener: tokio::net::TcpListener, app: axum::Router) -> std::io::Result<()> {
    axum::serve(listener, app).await
}

/// **ONE CONNECTION, ONE CONSUMER** (R4.S1 spec §2.4 item 4). A connection
/// that stated a consumer at `auth` and then subscribes stating a DIFFERENT
/// one is refused `malformed`; stating the SAME one passes the rule (and is
/// then answered by the hub — `unknown`, since this producer registers no
/// shape — so the two answers differ only by the rule). A connection that
/// stated NO consumer at `auth` (every client on the machine today) is
/// untouched by the rule whatever its subscribe states.
#[tokio::test]
async fn a_subscribe_stating_a_different_consumer_than_auth_is_malformed() {
    let key = Key::from_bytes([3u8; 32]);
    let s = serve(Arc::new(Strict::new(key.clone()))).await;
    let sub = |id: &str, consumer: Value| json!({"type":"request","id":id,"method":"shapes/none/subscribe","params":{"consumer":consumer}});
    let other = json!({"name":"someone-else","shape_version":"v6","build":"build-abc"});

    let (mut c, _) = Client::connect(&s).await;
    c.send(json!({"type":"auth","consumer":consumer(),"run_token":key.mint(&claims("thread-S"))}))
        .await;
    assert_eq!(c.json().await.unwrap()["ok"], true);
    c.send(sub("same", consumer())).await;
    let same = c.json().await.unwrap();
    assert_eq!(same["outcome"]["code"], "unknown", "{same}");
    c.send(sub("diff", other.clone())).await;
    let diff = c.json().await.unwrap();
    assert_eq!(diff["outcome"]["code"], "malformed", "{diff}");
    assert!(
        diff["outcome"]["error"]
            .as_str()
            .unwrap()
            .contains("different `consumer`"),
        "{diff}"
    );

    let p = serve(Arc::new(Plain {
        id: identity("plain"),
        hub: Hub::new(),
    }))
    .await;
    let (mut c, _) = Client::connect(&p).await;
    c.send(json!({"type":"auth","secret":SECRET})).await;
    assert_eq!(c.json().await.unwrap()["ok"], true);
    c.send(sub("legacy", other)).await;
    let legacy = c.json().await.unwrap();
    assert_eq!(legacy["outcome"]["code"], "unknown", "{legacy}");
}

// ── RV1.S1e: the door's request round trip, timed (`#[ignore]`d bench) ───────
#[path = "auth_gate_latency/mod.rs"]
mod latency;
