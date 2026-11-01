//! `link.rs` — **ONE SOCKET.** connect → handshake → learn → subscribe →
//! deliver → reconnect → refuse → teardown.
//!
//! The seven things [`shapewire::mirror`]'s own module doc says a mirror needs
//! from a connection, and nothing else. Specifically **it does not decide
//! recovery**: a socket death resets the mirrors and the next handshake
//! re-subscribes the book. There is no cursor in this file, no `after_seq`, no
//! parked-frame buffer, no lag repair and no `boot_id` branch, because rule 2.5
//! says the re-subscribed mirror and the never-disconnected mirror are the same
//! mirror. **Two things hold the producer and this file to that**, and both are
//! in the chassis beside this reference rather than in a face:
//! `lib/shapewire/tests/resubscribe_identity.rs` drives the property over
//! synthetic frames and states the three things that are NOT identical, and
//! `lib/shapewire/seam/probe/` measures it against the LIVE daemon over two
//! separate connections — 4,000-row windows over tails of 175,858 and 202,707
//! events, identical on every axis, 2026-10-07.
//!
//! ## What this replaces, in both faces
//!
//! In the AUI: `threads_client.rs`'s `connection_task`, `ClientHandle`, `Rpc`,
//! `CLIENT`, `LINK_*`, `await_link`, `ensure_daemon_running`, `daemon_health`,
//! `SUBSCRIBE_FRAME` and `DaemonPush`. In the terminal face: `transport.rs`'s
//! `supervise`, `session`, `handle_frame`, `BootState`, `ResyncKind`,
//! `start_resync`, `Cursors`, `park`, `end_repair`, `request_history`,
//! `Purpose` and `Pending::History`. The legacy keyless firehose subscribe
//! (`{"type":"subscribe","threads":null}`) is **not sent at all** — every fact
//! it carried is a cell now, and the one that was not (`stream_event` and the
//! rest of the 58% of the ledger no shape held) is `threads/log`.
//!
//! ## The door
//!
//! The CHASSIS port (`:3653`, `<PREFIX>_CHASSIS_PORT` to override, where the
//! prefix is the face's own — divergence D2) — the producer's only door since
//! R2.S4 retired the legacy one. That door carried no `shapes/*`, no typed
//! refusal codes and no `resource/{id}/read`, so every branch below that reads
//! a `code` would have had to read PROSE there instead — which is the board's
//! `caps.is_absence` workaround and is not copied.
//!
//! ## What is NOT in this file, and that is the point
//!
//! **No process is started here and no HTTP client is linked.** Whether the
//! daemon may be SPAWNED when nothing answers is a per-face operator ruling
//! (the AUI may, msg #1268; the terminal face's `DIR-7` forbids it), and the
//! design carried it as a fenced body — measured at ~110 lines once the
//! `/health` probe, the cold-start wait, the grace re-probe and the detached
//! spawn are counted, which is nearly twice the whole divergence budget. It is
//! a HOOK instead: [`Supervisor`], one method, implemented by the face above
//! the seam. The ruling is unchanged and lives at the call site; this file's
//! text stops differing. See D1's disposition in `mod.rs`.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use shapewire::envelope::codes;
use shapewire::wire::ShapeInfo;
use std::sync::Mutex;

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use super::held::{EntryState, Held, LinkState};

/// How long one command round trip waits for a reply before giving up.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// **HOW LONG ONE CONNECT MAY TAKE**: the TCP connect AND the WebSocket
/// upgrade, together. Unbounded, a daemon that accepts the connection and
/// never answers the upgrade (wedged, stopped under a debugger, a stranger on
/// the port) left a face `Connecting` with no retry and nothing said. Past
/// this the attempt is `Down` with that reason and the ladder below retries.
/// Loopback answers in milliseconds; five seconds is a wedge, not a slow day.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Backoff floor and ceiling. Reconnect FOREVER — both faces already do, and
/// the operator uses them all day; an `OFFLINE` terminal state is a board
/// affordance for its down page and has no home here.
const BACKOFF_MIN_MS: u64 = 500;
const BACKOFF_MAX_MS: u64 = 30_000;

// ──────────────────────────────────────────────────────────────────────────
//  What the face hears
// ──────────────────────────────────────────────────────────────────────────

/// **A WAKE-UP, NEVER A FACT.** Every variant means *go and read the mirror*;
/// none of them carries state the mirror does not already hold.
///
/// That is what lets this ride a `broadcast` channel with no lag repair behind
/// it. Under the firehose, a `Lagged` was a hole in the truth and cost a
/// `threads/status` resync plus a `threads/{id}/history` page. Here a dropped
/// wake-up costs nothing at all: the mirror already applied the frame before the
/// wake-up was sent, so the next wake-up — or the next periodic read — sees the
/// same state a delivered one would have. **Rule 2.5 is what deletes the repair,
/// and this channel is where you can see it.**
#[derive(Debug, Clone)]
pub enum SeamEvent {
    /// The link moved. Carries the state so a face can redraw its header
    /// without locking.
    Link(LinkState),
    /// Something a reader can see moved under `(shape, key)`.
    Moved {
        shape: String,
        key: Option<String>,
    },
    /// **A COMMAND'S PROMISE SETTLED — AND HOW.** `outcome` is the same answer
    /// `view::settled` gives, carried on the wake-up so a face cannot take
    /// `Settled` for `Visible`: a promise the daemon's next complete export did
    /// not re-offer, or that a restart swallowed, settles `Lost`, and until
    /// 2026-10-08 that rode this channel as the same bare id a `Visible` did. A
    /// face that treated the event as success had no way to tell. The id alone
    /// was one word for two outcomes.
    Settled { promise: u64, outcome: super::held::Settled },
    /// **A SUPERVISED SERVICE IS GOING DOWN, OR IS BACK.** Passed through from
    /// the daemon's own `lifecycle` frame, routed on the frame's `service`
    /// field rather than on the socket it arrived over, so a second service
    /// relayed through this daemon needs no code here.
    ///
    /// A wake-up like the rest: the seam says WHAT ARRIVED and never what to do
    /// about it. The AUI's announcer speaks it; the terminal face draws a note
    /// row. Neither decision is in this file, which is why this is an event and
    /// not a call into the face.
    Lifecycle {
        service: String,
        phase: String,
        reason: String,
        boot_id: String,
    },
}

// ──────────────────────────────────────────────────────────────────────────
//  Who this face is — divergence D2, and the only fence in this file
// ──────────────────────────────────────────────────────────────────────────

// SEAM-DIVERGE(D2): the face's own two words.
//
// `FACE` is rule 2.1's `consumer.name` — "a stable name for the PROGRAM
// (`threads-app`, `aui`, `tui` — not the library inside it)". It is also the
// prefix on every correlate id this file mints, so a refusal read out of a
// daemon log says which face sent it. `ENV_PREFIX` is how the operator points
// ONE face at a different daemon without moving the other.
//
// The design (`docs/consumer-seam.md` §5) had these as four differences in
// three places — the identity literal, two env names and the correlate prefix.
// They are one fact said four times, so they are one fence said once, and
// everything below reads these two consts.
pub const FACE: &str = "tui";
const ENV_PREFIX: &str = "TUI_THREADS";
// SEAM-END

// ──────────────────────────────────────────────────────────────────────────
//  Supervision — a HOOK, not a body (D1's disposition is in `mod.rs`)
// ──────────────────────────────────────────────────────────────────────────

/// **WHAT RUNS BEFORE THE FIRST BYTE OF A CONNECTION ATTEMPT.**
///
/// Operator ruling (msg #1268): the AUI STARTS the daemon when it is not
/// running — *"that is not spawning claude, and it is how an outage heals
/// itself"*. The terminal face's `DIR-7` makes it a pure subscriber that never
/// spawns. Both are rulings and neither can move.
///
/// So the ruling is a CALL-SITE argument and not a fenced body: a face that may
/// spawn implements this and passes `Some(hook)`; a face that may not passes
/// `None`. An `Err` is published as `LinkState::Down` and retried on the
/// ladder — it never ends the loop, because a daemon that cannot be started now
/// can be started in thirty seconds.
///
/// **Why a trait and not an enum.** An enum had to carry the spawn
/// implementation INTO this file, and with it a `/health` probe, a cold-start
/// wait, a grace re-probe, a detached `Command` and an HTTP client — ~110 lines
/// and one dependency only one face uses, all of it fenced, against a
/// whole-seam budget of sixty. The hook leaves the seam linking no HTTP client
/// at all.
pub trait Supervisor: Send + Sync {
    /// `Ok(())` means *go ahead and connect*. Called before EVERY attempt,
    /// including every reconnect, so it must be cheap when the daemon is up and
    /// idempotent when it is not.
    fn before_connect(&self) -> BoxFuture<'_, Result<(), String>>;
}

/// The hook, or nothing. `None` is a complete answer, and the terminal face's.
pub type Supervision = Option<Arc<dyn Supervisor>>;

/// What [`Supervisor::before_connect`] answers. Spelled out rather than taken
/// from `futures::future::BoxFuture` so this file's dependency list stays the
/// short one the rest of it needs.
pub type BoxFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

// ──────────────────────────────────────────────────────────────────────────
//  Where the daemon is
// ──────────────────────────────────────────────────────────────────────────

const HOST: &str = "127.0.0.1";

/// **THE CHASSIS PORT.** `shapes/*`, typed refusal codes and
/// `resource/{id}/read` live here and on no other door.
pub const DEFAULT_CHASSIS_PORT: u16 = 3653;

/// The `/health` port, which is how supervision decides whether to spawn. It
/// was the LEGACY port (3651), kept for this one thing; R2.S4 (2026-10-10)
/// retired that listener, and the chassis door serves the same `/health` body
/// byte for byte (`threads` `tests/golden_session.rs`), so it is the chassis
/// port. `core/tools/promote` still hardcodes `:3651/health` and must move
/// with this — a deployment dependency, recorded as owed by that step.
pub const DEFAULT_HEALTH_PORT: u16 = DEFAULT_CHASSIS_PORT;

fn env_port(key: &str, default: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|p| *p != 0)
        .unwrap_or(default)
}

pub fn chassis_port() -> u16 {
    env_port(&format!("{ENV_PREFIX}_CHASSIS_PORT"), DEFAULT_CHASSIS_PORT)
}

pub fn health_port() -> u16 {
    env_port(&format!("{ENV_PREFIX}_PORT"), DEFAULT_HEALTH_PORT)
}

pub fn ws_url() -> String {
    format!("ws://{HOST}:{}/ws", chassis_port())
}

pub fn health_url() -> String {
    format!("http://{HOST}:{}/health", health_port())
}

fn secret() -> Option<String> {
    for k in [format!("{ENV_PREFIX}_SECRET"), "THREADS_SECRET".to_string()] {
        if let Ok(v) = std::env::var(&k) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    None
}

// ──────────────────────────────────────────────────────────────────────────
//  The handle
// ──────────────────────────────────────────────────────────────────────────

/// One request the connection must send. **Commands only** — there is no read
/// method left for this to carry, and `commands::methods::TEMPLATES` is the
/// whole list, routed against the producer's own inventory by
/// `seamdiff --inventory`. A face that wants a guard against a read smuggled
/// in at a call site above the seam writes it in the face; the seam's own
/// surface is checked here.
pub(crate) struct Request {
    pub method: String,
    pub params: Value,
    pub reply: oneshot::Sender<Result<Value, shapewire::envelope::Refusal>>,
}

/// What a face holds. Cheap to clone; everything behind it is one task.
#[derive(Clone)]
pub struct Link {
    tx: mpsc::Sender<Request>,
    /// Keyed (un)subscribes raised by `Book::hold`/`Book::release` after the
    /// initial walk. A plain channel rather than a request, because a
    /// subscribe's ANSWER is the export and not the reply.
    subs: mpsc::Sender<SubOp>,
}

pub(crate) enum SubOp {
    Subscribe {
        shape: &'static str,
        key: Option<String>,
    },
    Unsubscribe {
        shape: &'static str,
        key: Option<String>,
    },
}

impl Link {
    /// One correlated request/response round trip, for a COMMAND.
    ///
    /// **IT TAKES A `Dialed`, NOT A STRING, AND THAT IS THE WHOLE OF HOW A DEAD
    /// VERB STOPS BEING POSSIBLE.** `shapewire::surface::Dialed` is sealed: the
    /// only thing that mints one is `Method::at`, and the only `Method`s that
    /// exist are the consts `seam/inventory.rs` generates from the producer's own
    /// method list. So the set of strings this function can ever put on the wire
    /// is the set of substitutions of templates the producer dispatches — checked
    /// by the compiler, in the face's own build, at the line that names the verb.
    ///
    /// `thread.replace` and `thread.unpend` were sent through this function with
    /// string literals, from ABOVE the seam, so the inventory guard never saw
    /// them. That arrangement is now a type error rather than a convention: there
    /// is no `request_raw`, because an escape hatch is the hole.
    ///
    /// A refusal comes back TYPED (`Refusal{code, message}`) and the caller
    /// branches on `code`, never on prose. `busy` is returned rather than
    /// retried: rule 2.5's reasoning is that a retry policy in the consumer half
    /// is a second opinion about the producer's load, and the SKIN is what
    /// decides whether to ask again.
    /// The bound is on `Dialable` rather than on `Dialed` itself for ONE reason,
    /// and it is the error message: a parameter typed `Dialed` refuses a string
    /// with `expected `Dialed`, found `&str``, which names a type equation. The
    /// trait carries a `#[diagnostic::on_unimplemented]` that names the method's
    /// const and says there is no raw door — and a compile error that cannot say
    /// what to add is a riddle with a build break attached.
    pub async fn request<M: shapewire::surface::Dialable>(
        &self,
        method: M,
        params: Value,
    ) -> Result<Value, shapewire::envelope::Refusal> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Request {
                method: method.dialed().path().to_string(),
                params,
                reply,
            })
            .await
            .map_err(|_| {
                shapewire::envelope::Refusal::new(codes::ERROR, "the seam's link task is gone")
            })?;
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(shapewire::envelope::Refusal::new(
                codes::ERROR,
                "the socket dropped before the daemon answered",
            )),
            Err(_) => Err(shapewire::envelope::Refusal::new(
                codes::ERROR,
                "the daemon did not answer within 30s",
            )),
        }
    }

    pub async fn subscribe(&self, shape: &'static str, key: Option<String>) {
        let _ = self.subs.send(SubOp::Subscribe { shape, key }).await;
    }

    pub async fn unsubscribe(&self, shape: &'static str, key: Option<String>) {
        let _ = self.subs.send(SubOp::Unsubscribe { shape, key }).await;
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  The connection
// ──────────────────────────────────────────────────────────────────────────

/// Start the one connection. Returns the handle; the task runs forever.
pub fn start(
    held: Arc<Mutex<Held>>,
    events: broadcast::Sender<SeamEvent>,
    supervision: Supervision,
) -> Link {
    let (tx, rx) = mpsc::channel::<Request>(64);
    let (stx, srx) = mpsc::channel::<SubOp>(256);
    let link = Link {
        tx,
        subs: stx,
    };
    tokio::spawn(run(held, events, supervision, rx, srx));
    link
}

/// **ONE SUBSCRIBE PARAMETER SET, BUILT IN ONE PLACE**, so an entry's `detail`,
/// `retain` and `beat` can never be spelled two ways.
fn subscribe_params(e: &super::held::Entry) -> Value {
    let mut p = json!({
        // Rule 2.1's consumer identity. `FACE` is the one word that differs
        // by face and it is fenced ONCE, at the top of this file, rather than
        // here: the design had the literal in this object AND the two env
        // names AND the correlate prefix as separate divergences, which is the
        // same fact in four places.
        "consumer": {
            "name": FACE,
            "shape_version": super::SHAPE_VERSION,
            "pid": std::process::id(),
            "build": env!("CARGO_PKG_VERSION"),
        },
        "detail": e.detail.as_str(),
        "retain": e.retain.describe(),
        // ── RULE 2.6, AND IT IS A HANDSHAKE RATHER THAN A CLAIM ────────────
        //
        // `consumer.shape_version` above is free text for a human; THIS is the
        // integer the producer's declared contract is checked against, once, at
        // the link, PER SHAPE (`mod.rs`'s `SHAPE_CONTRACT_VERSIONS`): the
        // version of THIS shape the seam's reads were written against. A
        // producer that does not serve it refuses the subscribe
        // `version_refused` — which is the rule working, and is why this is
        // sent rather than read back off `shapes/list`. Echoing the producer's
        // own number would make the handshake a tautology.
        "version": super::contract_version(e.shape),
    });
    if let Some(k) = &e.key {
        p["key"] = Value::String(k.clone());
    }
    match e.beat {
        shapewire::wire::Beat::Max => {}
        shapewire::wire::Beat::Every(ms) => p["beat"] = json!(ms),
    }
    p
}

async fn run(
    held: Arc<Mutex<Held>>,
    events: broadcast::Sender<SeamEvent>,
    supervision: Supervision,
    mut rx: mpsc::Receiver<Request>,
    mut srx: mpsc::Receiver<SubOp>,
) {
    let mut backoff = BACKOFF_MIN_MS;
    let mut attempt: u32 = 0;
    let mut last_boot: Option<String> = None;

    loop {
        attempt = attempt.saturating_add(1);
        publish(&held, &events, LinkState::Connecting { attempt });

        if let Err(e) = supervise(&supervision).await {
            publish(&held, &events, LinkState::Down { reason: e.clone() });
            warn!("threads daemon unavailable: {e}; retrying in {backoff}ms");
            tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
            backoff = (backoff * 2).min(BACKOFF_MAX_MS);
            continue;
        }

        // BOUNDED (CONNECT_TIMEOUT): a daemon that takes the TCP connection
        // and never answers the upgrade is `Down` with that said, then
        // retried, never `Connecting` for ever.
        let ws = match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(ws_url())).await {
            Ok(Ok((ws, _))) => ws,
            failed => {
                let reason = match failed {
                    Ok(Err(e)) => format!("could not open the chassis socket: {e}"),
                    _ => format!("the chassis socket did not answer its handshake within {} ms", CONNECT_TIMEOUT.as_millis()),
                };
                warn!("threads daemon unavailable: {reason}; retrying in {backoff}ms");
                publish(&held, &events, LinkState::Down { reason });
                tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                backoff = (backoff * 2).min(BACKOFF_MAX_MS);
                continue;
            }
        };
        let (mut sink, mut stream) = ws.split();

        let mut pending: HashMap<String, oneshot::Sender<Result<Value, shapewire::envelope::Refusal>>> =
            HashMap::new();
        let mut next_id: u64 = 1;
        // `shapes/list`'s own request id, so its answer is routed to `learn`
        // rather than to a caller that is not there.
        let mut learn_id: Option<String> = None;
        let mut usable = false;

        'session: loop {
            tokio::select! {
                req = rx.recv() => {
                    let Some(r) = req else { return };  // the process is going down
                    if !usable {
                        let _ = r.reply.send(Err(shapewire::envelope::Refusal::new(
                            codes::ERROR,
                            "the threads daemon link is not up",
                        )));
                        continue;
                    }
                    let id = format!("{FACE}-{next_id}");
                    next_id += 1;
                    let frame = json!({"type":"request","id":id,"method":r.method,"params":r.params});
                    if sink.send(Message::Text(frame.to_string())).await.is_err() {
                        let _ = r.reply.send(Err(shapewire::envelope::Refusal::new(
                            codes::ERROR, "the socket write failed",
                        )));
                        break 'session;
                    }
                    pending.insert(id, r.reply);
                }
                op = srx.recv() => {
                    let Some(op) = op else { return };
                    if !usable { continue; }
                    let frame = match op {
                        SubOp::Subscribe { shape, key } => {
                            let params = {
                                let g = lock(&held);
                                match g.book.get(shape, key.as_deref()) {
                                    Some(e) => subscribe_params(e),
                                    None => continue,
                                }
                            };
                            next_id += 1;
                            json!({
                                "type":"request",
                                "id": sub_correlate(shape, key.as_deref(), next_id),
                                "method": format!("shapes/{shape}/subscribe"),
                                "params": params,
                            })
                        }
                        SubOp::Unsubscribe { shape, key } => {
                            next_id += 1;
                            let mut params = json!({});
                            if let Some(k) = key { params["key"] = Value::String(k); }
                            json!({
                                "type":"request",
                                "id": format!("{FACE}-unsub-{next_id}"),
                                "method": format!("shapes/{shape}/unsubscribe"),
                                "params": params,
                            })
                        }
                    };
                    if sink.send(Message::Text(frame.to_string())).await.is_err() {
                        break 'session;
                    }
                }
                frame = stream.next() => {
                    let Some(Ok(msg)) = frame else { break 'session };
                    let text = match msg {
                        Message::Text(t) => t,
                        Message::Close(_) => break 'session,
                        _ => continue,
                    };
                    let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                    match v.get("type").and_then(Value::as_str) {
                        Some("welcome") => {
                            // Parsed TYPED, into the chassis's own envelope: a
                            // welcome with no `boot_id` is a frame this build
                            // cannot act on, not a boot id of `""` — which would
                            // compare equal to itself across every restart and
                            // never settle a promise `Lost`. Both fields are
                            // required on `ChassisServerMsg::Welcome`.
                            let (boot_id, auth_required) = match serde_json::from_value::<
                                shapewire::envelope::ChassisServerMsg,
                            >(v.clone())
                            {
                                Ok(shapewire::envelope::ChassisServerMsg::Welcome {
                                    boot_id, auth_required, ..
                                }) => (boot_id, auth_required),
                                other => {
                                    error!(
                                        "the daemon's welcome did not parse as a chassis welcome \
                                         (no boot_id, or an unknown form): {other:?}; dropping the socket"
                                    );
                                    publish(&held, &events, LinkState::Down {
                                        reason: "the daemon's welcome frame did not parse".into(),
                                    });
                                    break 'session;
                                }
                            };
                            info!(%boot_id, auth_required, "threads chassis welcome");
                            // **THE ONLY CONSUMER OF THE `boot_id` COMPARISON
                            // INSIDE THE SEAM** (`docs/consumer-seam.md` §2.2)
                            // is the promise book. It decides NOTHING about the
                            // mirrors — a restart killed the socket, and socket
                            // death already reset them. The SENTENCE a face says
                            // about a new boot is the face's own: it reads the
                            // `boot_id` off `LinkState::Up`, which the publish
                            // below carries.
                            if last_boot.as_deref().is_some_and(|b| b != boot_id) {
                                let lost = lock(&held).promises.boot_changed(&boot_id);
                                for id in lost {
                                    let _ = events.send(SeamEvent::Settled {
                                        promise: id,
                                        outcome: super::held::Settled::Lost,
                                    });
                                }
                            }
                            last_boot = Some(boot_id.clone());

                            if auth_required {
                                let Some(s) = secret() else {
                                    error!(
                                        "the threads daemon requires a secret but neither \
                                         {ENV_PREFIX}_SECRET nor THREADS_SECRET is set"
                                    );
                                    break 'session;
                                };
                                if sink.send(Message::Text(
                                    json!({"type":"auth","secret":s}).to_string()
                                )).await.is_err() { break 'session; }
                            } else {
                                usable = true;
                                backoff = BACKOFF_MIN_MS;
                                attempt = 0;
                                publish(&held, &events, LinkState::Up {
                                    boot_id: boot_id.clone(),
                                    since: chrono::Utc::now(),
                                });
                                next_id += 1;
                                let id = format!("{FACE}-learn-{next_id}");
                                learn_id = Some(id.clone());
                                if sink.send(Message::Text(json!({
                                    "type":"request","id":id,"method":"shapes/list","params":{}
                                }).to_string())).await.is_err() { break 'session; }
                            }
                        }
                        Some("auth_ack") => {
                            if v.get("ok").and_then(Value::as_bool) != Some(true) {
                                error!("the threads daemon rejected our secret");
                                publish(&held, &events, LinkState::Down {
                                    reason: "the daemon rejected the secret".into(),
                                });
                                break 'session;
                            }
                            usable = true;
                            backoff = BACKOFF_MIN_MS;
                            attempt = 0;
                            publish(&held, &events, LinkState::Up {
                                boot_id: last_boot.clone().unwrap_or_default(),
                                since: chrono::Utc::now(),
                            });
                            next_id += 1;
                            let id = format!("{FACE}-learn-{next_id}");
                            learn_id = Some(id.clone());
                            if sink.send(Message::Text(json!({
                                "type":"request","id":id,"method":"shapes/list","params":{}
                            }).to_string())).await.is_err() { break 'session; }
                        }
                        Some("response") => {
                            let correlate = v.get("correlate").and_then(Value::as_str)
                                .unwrap_or_default().to_string();
                            let outcome = v.get("outcome").cloned().unwrap_or(Value::Null);
                            let res = parse_outcome(&outcome);
                            if learn_id.as_deref() == Some(correlate.as_str()) {
                                learn_id = None;
                                match res {
                                    Ok(payload) => {
                                        let walk = learn(&held, &payload);
                                        for frame in walk {
                                            if sink.send(Message::Text(frame)).await.is_err() {
                                                break 'session;
                                            }
                                        }
                                    }
                                    Err(r) => {
                                        error!(code = r.code, "`shapes/list` refused: {}", r.message);
                                        break 'session;
                                    }
                                }
                                continue;
                            }
                            // A subscribe's refusal lands on its ENTRY. It is
                            // per-KEY: a `threads` subscribe to a thread that
                            // does not exist must not mark the whole pool
                            // refused, which `Mirror::refused` (shape-wide)
                            // would do. Only `dead` is shape-wide.
                            if let Some(reply) = pending.remove(&correlate) {
                                let _ = reply.send(res);
                            } else if let Err(r) = res {
                                note_entry_refusal(&held, &correlate, &r);
                            }
                        }
                        Some("shape") => {
                            // **THE FOLD, AND IT RUNS HERE.** Rule 2.3: an ack
                            // means received into the mirror, and a slow
                            // consumer may only hurt itself. So frames are
                            // applied in arrival order in the reader task, under
                            // the one lock, and never on a face's loop.
                            let (promises, moved, lost) = {
                                let mut g = lock(&held);
                                let receipt = g.fold(&v);
                                let promises = receipt.promises.clone();
                                let moved = receipt.moved;
                                // A COMPLETE export is the one thing that lets
                                // an absence be inferred (§4.4): ids this
                                // process opened for that subject which the
                                // export did not re-offer are now Lost.
                                let lost = if receipt.complete == Some(true) {
                                    let key = v.get("key").and_then(Value::as_str);
                                    g.promises.lose_unoffered(key)
                                } else {
                                    Vec::new()
                                };
                                (promises, moved, lost)
                            };
                            for id in promises {
                                let _ = events.send(SeamEvent::Settled {
                                    promise: id,
                                    outcome: super::held::Settled::Visible,
                                });
                            }
                            for id in lost {
                                let _ = events.send(SeamEvent::Settled {
                                    promise: id,
                                    outcome: super::held::Settled::Lost,
                                });
                            }
                            if moved {
                                let _ = events.send(SeamEvent::Moved {
                                    shape: v.get("shape").and_then(Value::as_str)
                                        .unwrap_or_default().to_string(),
                                    key: v.get("key").and_then(Value::as_str).map(str::to_string),
                                });
                            }
                        }
                        // Passed up as an event. The seam does not know what a
                        // face does about a service going down, and must not.
                        Some("lifecycle") => {
                            if let Some(e) = lifecycle_event(&v) {
                                let _ = events.send(e);
                            }
                        }
                        other => debug!(?other, "seam: a frame type this face does not read"),
                    }
                }
            }
        }

        // ── the socket died ────────────────────────────────────────────────
        for (_, tx) in pending.drain() {
            let _ = tx.send(Err(shapewire::envelope::Refusal::new(
                codes::ERROR,
                "the socket dropped before the daemon answered",
            )));
        }
        lock(&held).reset("the threads daemon connection was lost");
        let _ = events.send(SeamEvent::Link(LinkState::Down {
            reason: "the threads daemon connection was lost".into(),
        }));
        warn!("threads chassis connection lost — reconnecting");
        tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
        backoff = (backoff * 2).min(BACKOFF_MAX_MS);
    }
}

fn parse_outcome(outcome: &Value) -> Result<Value, shapewire::envelope::Refusal> {
    match outcome.get("type").and_then(Value::as_str) {
        Some("success") => Ok(outcome.get("payload").cloned().unwrap_or(Value::Null)),
        _ => {
            // The chassis door types every refusal. The code is the branch; the
            // message is for a human and is never matched on.
            let code = outcome
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or(codes::ERROR);
            let code = codes::ALL
                .iter()
                .find(|c| **c == code)
                .copied()
                .unwrap_or(codes::ERROR);
            let message = outcome
                .get("error")
                .or_else(|| outcome.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("the daemon refused and said nothing")
                .to_string();
            Err(shapewire::envelope::Refusal::new(code, message))
        }
    }
}

/// **THE CORRELATE CARRIES THE SUBJECT**, so a typed refusal can be put back on
/// the ENTRY that caused it rather than on the shape.
///
/// `\u{1}` as the separator, not `:` or `/`, because a key is a thread id or an
/// account label and a separator that could occur inside one is a bug waiting
/// for the first account named `a:b`.
fn sub_correlate(shape: &str, key: Option<&str>, n: u64) -> String {
    format!("{FACE}-sub-{shape}\u{1}{}\u{1}{n}", key.unwrap_or(""))
}

/// A refusal that answers a subscribe this face sent.
fn note_entry_refusal(
    held: &Arc<Mutex<Held>>,
    correlate: &str,
    r: &shapewire::envelope::Refusal,
) {
    let prefix = format!("{FACE}-sub-");
    let Some(rest) = correlate.strip_prefix(prefix.as_str()) else {
        // The refusal's TEXT is the producer's prose and may quote a key —
        // an account's key is its address (V1-ACCT, V1-CLEAN2-I N2). The
        // seam is face-neutral and holds no masker, so the text never goes
        // to the log at all: its code and length do.
        debug!(correlate, code = r.code, message_chars = r.message.chars().count(), "seam: an unrouted refusal");
        return;
    };
    let mut parts = rest.splitn(3, '\u{1}');
    let Some(shape) = parts.next() else { return };
    let key = parts.next().filter(|k| !k.is_empty());
    lock(held).refuse(shape, key, r);
}

/// `shapes/list` arrived: teach every mirror, then build the subscribe walk.
///
/// **A `shapes/list` THIS BUILD CANNOT PARSE IS NOT A DAEMON THAT SERVES
/// NOTHING.** It used to be read as exactly that: a parse failure became an
/// empty list, every book entry went `Unsupported { "this daemon does not serve
/// `threads`" }` — a positive, false claim — and the mirrors learned no ceiling,
/// no contract and no manifest. A typed `ShapeInfo` that gains a required key
/// (`contract` did; `cells` did) is the ordinary way this happens, and the
/// honest answer is *this build cannot read what the producer declares*, named
/// as such on every entry, with nothing subscribed.
fn learn(held: &Arc<Mutex<Held>>, payload: &Value) -> Vec<String> {
    let infos: Vec<ShapeInfo> = match payload
        .get("shapes")
        .map(|s| serde_json::from_value(s.clone()))
    {
        Some(Ok(infos)) => infos,
        Some(Err(e)) => {
            error!("`shapes/list` did not parse into this build's `ShapeInfo`: {e}");
            lock(held).unreadable(&format!(
                "this build cannot read the daemon's `shapes/list` ({e}); it is behind or \
                 ahead of what the producer declares and must be re-vendored"
            ));
            return Vec::new();
        }
        None => {
            error!("`shapes/list` answered with no `shapes` key");
            lock(held).unreadable("the daemon's `shapes/list` carried no `shapes` key");
            return Vec::new();
        }
    };
    let mut g = lock(held);
    g.learn(&infos);
    let mut out = Vec::new();
    let mut n = 0u64;
    for e in g.book.entries() {
        if matches!(e.state, EntryState::Unsupported { .. }) {
            continue;
        }
        n += 1;
        let id = sub_correlate(e.shape, e.key.as_deref(), n);
        out.push(
            json!({
                "type": "request",
                "id": id,
                "method": format!("shapes/{}/subscribe", e.shape),
                "params": subscribe_params(e),
            })
            .to_string(),
        );
    }
    out
}

fn publish(held: &Arc<Mutex<Held>>, events: &broadcast::Sender<SeamEvent>, s: LinkState) {
    lock(held).link = s.clone();
    let _ = events.send(SeamEvent::Link(s));
}

/// Poison is recovered, not propagated: a panic in one fold must not take the
/// mirror away from the operator's interface for the rest of the process's life.
fn lock(held: &Arc<Mutex<Held>>) -> std::sync::MutexGuard<'_, Held> {
    held.lock().unwrap_or_else(|e| e.into_inner())
}

/// A `lifecycle` frame, as an event. `None` when the frame names no service:
/// there is nothing to say about a service with no name, and inventing one is
/// how a narration line ends up lying.
fn lifecycle_event(v: &Value) -> Option<SeamEvent> {
    let f = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let service = f("service");
    if service.is_empty() {
        return None;
    }
    Some(SeamEvent::Lifecycle {
        service,
        phase: f("phase"),
        reason: f("reason"),
        boot_id: f("boot_id"),
    })
}

// ──────────────────────────────────────────────────────────────────────────
//  The hook, called
// ──────────────────────────────────────────────────────────────────────────

/// Run the face's hook, if it supplied one. **Before every attempt**, including
/// every reconnect: a face that spawns must be allowed to notice the daemon has
/// gone away for good and start it again.
async fn supervise(s: &Supervision) -> Result<(), String> {
    match s {
        None => Ok(()),
        Some(hook) => hook.before_connect().await,
    }
}
