//! **THE 9010 CLIENT — the second socket, and the WRITE side only.**
//!
//! # What this is, and the one sentence that bounds it
//!
//! `0060` ruled the sheet is the OWNER'S CONSOLE: every act is bound, at design
//! time, to the one process that owns the state it changes. Two of the thread
//! sheet's six acts are the daemon's (interrupt, compact) and go out over
//! [`crate::transport`], which already exists. The other four — model, rename,
//! pin/move, closed — are the AUI's, and the only channel the AUI offers for
//! them is its proto-v2 socket on port 9010. This module is that channel.
//!
//! **WRITE-SIDE PLUS A HINT, and the invariant is absolute: nothing the face
//! RENDERS may depend on this connection.** No fact, no count, no state word on
//! any screen is sourced from here. The face draws identically with 9010 absent
//! — which is what keeps `DEC-13`'s disconnected posture and `0023`'s three
//! pieces of state true. The single, deliberate exception is the *availability
//! sentence on an act row*, and that is not a fact about his work: it is the
//! row telling him why it cannot act, which is the opposite of a face that
//! silently needs a socket to draw.
//!
//! # Unreachable is a NORMAL state here, not an error path
//!
//! This face connects over **loopback** (`ws://127.0.0.1:9010/` by default —
//! see [`endpoint_from`]) and has no Tailscale involvement at all, by design:
//! the AUI runs on this same machine, so it was never the TUI's problem to
//! solve that the AUI's listener used to bind **the tailnet address and only
//! the tailnet address**, which left it unbound for DAYS without anyone
//! noticing — `bind 198.51.100.77:9010: Can't assign requested address` on
//! every boot with Tailscale simply stopped (`apps/aui/src/v2/mod.rs`, the
//! 2026-09-11 incident). The AUI now also binds loopback unconditionally
//! (`apps/aui/src/v2/mod.rs`'s module header) for exactly this reason, and
//! this client no longer shells out to `tailscale` to find its door.
//!
//! Absence is still a first-class, named, drawn STATE here ([`Reach`]), with
//! the reason carried in the words a row will print — not because Tailscale
//! is expected to be flaky (it is no longer in this path), but because the
//! AUI process itself can always be down, mid-restart, or bound somewhere
//! unexpected by an explicit override. A client that treats "cannot reach
//! 9010" as an exception would spend most of its life in an exception.
//!
//! # What it never does
//!
//! - **It never writes a file.** Not `.mind/threads.json`, not
//!   `~/.aui/archived-projects.json`, not the settings file. The AUI's index is
//!   a projection of its `AppState` rewritten wholesale every turn
//!   (`crate::index`'s header); a second writer loses the race silently. When
//!   9010 is absent the act does not happen and SAYS so — there is no file
//!   fallback and there must never be one.
//! - **It never sends a daemon-owned act.** `InterruptThread` and
//!   `CompactThread` exist as `Mutation` variants and are deliberately not
//!   reachable from here; [`crate::acts`] is where that binding is made and
//!   machine-checked.
//! - **It never sends service administration.** `AdminRestartServer`,
//!   `AdminShutdownServer` and `SetRingTarget` are cut from this face by the
//!   design doc, and [`crate::acts`]'s guard test enumerates them.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use aui_wire::node::MutationEnvelope;
use aui_wire::proto::{ClientInfo, ClientMsg, MutResult, ServerMsg, DEFAULT_PORT, PROTO_VERSION};
use aui_wire::server::Mutation;

/// The `kind` this face declares in its `Hello`. `proto.rs:85` already names
/// `"tui" (terminal)` as a client kind — the wire anticipated this client.
pub const CLIENT_KIND: &str = "tui";

/// Stable per-DEVICE id, and it is deliberately EMPTY. `proto.rs:94-96`:
/// *"Empty for clients that do not care (the desk TUI); the server then treats
/// each connection as its own anonymous device."* This face never marks
/// anything delivered, so it has nothing to remember across connections.
pub const DEVICE_ID: &str = "";

// ────────────────────────────────────────────────────────────────────────────
//  WHERE THE DOOR IS
// ────────────────────────────────────────────────────────────────────────────

/// **THE DEFAULT IS LOOPBACK, AND THAT IS AN HONEST VALUE, NOT A GUESS.**
///
/// This face and the AUI it talks to run on the SAME machine — it is a write
/// channel to a local process, not a remote one — so there is no address to
/// discover here the way `apps/aui/src/v2/mod.rs`'s PHONE door has to
/// discover a tailnet IP that only exists once Tailscale is up. `127.0.0.1`
/// is a fixed, reserved port on loopback, which is exactly the case
/// `apps/tui/src/main.rs`'s own `DEFAULT_THREADS_URL` comment already rules an
/// honest literal rather than *"a value that PRETENDS TO WORK"*: a loopback
/// address this process is ALWAYS able to dial, whether or not the AUI has
/// bound it yet.
///
/// This face used to resolve this address with `tailscale ip -4` (`DEC-8`'s
/// method), because the AUI's listener used to bind the tailnet address and
/// only the tailnet address. That shelled out to a binary that might not be
/// installed, might not be running, and was answering a question — "where is
/// this machine on the tailnet" — that a same-machine client never needed to
/// ask. The AUI now binds loopback unconditionally for exactly this reason
/// (`apps/aui/src/v2/mod.rs`'s module header), so this face no longer shells
/// out to `tailscale` at all.
///
/// The order is, highest first:
///
/// 1. `AUI_V2_URL` — an explicit whole URL, for a test or an odd machine.
/// 2. The loopback default — always present, never `None`.
pub fn endpoint_from(explicit_url: Option<String>) -> String {
    let clean = |s: String| {
        let t = s.trim().to_string();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    };
    if let Some(url) = explicit_url.and_then(clean) {
        return url;
    }
    format!("ws://127.0.0.1:{DEFAULT_PORT}/")
}

/// [`endpoint_from`], wired to the live machine. Re-run on **every** connect
/// attempt — cheap, and it means `AUI_V2_URL` changing while the face is open
/// is picked up without a restart.
fn resolve_endpoint() -> String {
    endpoint_from(std::env::var("AUI_V2_URL").ok())
}

// ────────────────────────────────────────────────────────────────────────────
//  ABSENCE, IN THE WORDS A ROW PRINTS
// ────────────────────────────────────────────────────────────────────────────

/// **Why the link is down, chosen by what the socket returned.**
///
/// `0060`: *"connection refused / no route to the tailnet address while the
/// process is known to be up is not distinguishable from the TUI's side; the
/// words are chosen by what the socket returned."* This enum is that rule,
/// written down. It has exactly two members because the TUI can honestly tell
/// exactly two things apart:
///
/// - Something answered at the address and **refused** the connection. The
///   route exists; nothing is listening on it. The AUI process is not up (or
///   has not bound yet).
/// - The address could not be reached, or could not be resolved at all. The
///   route itself is the problem — an explicit `AUI_V2_URL` pointing somewhere
///   that does not answer, most likely, now that the default is loopback.
///
/// Guessing harder than this would be inventing a distinction the socket did
/// not give us, which is how a face starts lying politely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownCause {
    /// Refused at the door: the listener is not bound.
    NotRunning,
    /// The door is not reachable — no route, no address, nothing resolved.
    DoorShut,
}

impl DownCause {
    /// The sentence appended to an act row, and repeated once in the notice
    /// line when he activates that row. `0060` fixes both strings verbatim.
    pub fn words(self) -> &'static str {
        match self {
            DownCause::NotRunning => "needs the audio interface, which is not running",
            DownCause::DoorShut => "the audio interface's door is not open",
        }
    }

    /// Classify one connect failure. Kept pure and separate from the socket so
    /// the mapping is provable without one.
    pub fn of_connect_error(text: &str) -> DownCause {
        let t = text.to_ascii_lowercase();
        // "Connection refused" is the one case where the route demonstrably
        // works and the listener demonstrably does not.
        if t.contains("connection refused") || t.contains("os error 61") {
            DownCause::NotRunning
        } else {
            DownCause::DoorShut
        }
    }
}

/// **The link state, and it is DRAWN rather than hidden.**
///
/// The sheet reads this to choose an act row's words. Nothing else in the face
/// may read it — see the module header's invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// No socket. The row is muted and carries [`DownCause::words`].
    Absent { cause: DownCause },
    /// A connect is in flight. Treated as ABSENT for row words — a row that
    /// promised to work because a handshake was halfway done would be a row
    /// that lies for two seconds at a time.
    Connecting { attempt: u32, cause: DownCause },
    /// Handshaken. Acts go.
    Up,
}

impl Reach {
    pub fn is_up(&self) -> bool {
        matches!(self, Reach::Up)
    }

    /// The sentence for an AUI-owned act row, or `None` when the act will work.
    pub fn words(&self) -> Option<&'static str> {
        match self {
            Reach::Up => None,
            Reach::Absent { cause } | Reach::Connecting { cause, .. } => Some(cause.words()),
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
//  what the face hears
// ────────────────────────────────────────────────────────────────────────────

/// Everything this client tells the face. Deliberately small: the face does not
/// get to learn about handshakes, promises or projections from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuiEvent {
    /// The link state changed. The ONE event that moves a row's words.
    Reach(Reach),
    /// **"Re-fetch if you care."** A successful mutation — this face's, the
    /// phone's, or the AUI's own — rippled a node
    /// (`apps/aui/src/v2/mod.rs:487-510`). Carries the node KIND only; the
    /// face's answer is to re-read the indexes, which is the same door `Alt-r`
    /// uses.
    Invalidate { node: String },
}

/// The node kinds whose invalidation means **the INDEX changed**, so a re-read
/// is owed.
///
/// `thread_options` and `pending_inputs` also ripple and are deliberately
/// absent: neither changes a byte of `.mind/threads.json`, so re-reading on
/// them would be work with no possible effect. `archived` is present though
/// the `ripple()` helper does not list it, because `ArchiveProject` changes
/// `~/.aui/archived-projects.json`, which `index::load_archived` reads and the
/// root draws.
pub const INDEX_NODES: &[&str] = &[
    "root",
    "recent_threads",
    "pinned",
    "project",
    "thread",
    "archived",
];

/// Whether an `Invalidate` for this node kind should cost an index re-read.
pub fn invalidate_touches_index(node: &str) -> bool {
    INDEX_NODES.contains(&node)
}

// ────────────────────────────────────────────────────────────────────────────
//  the handle
// ────────────────────────────────────────────────────────────────────────────

/// One queued mutation. `resp` is answered exactly once on every path,
/// including every path where the frame is never written at all.
#[derive(Debug)]
pub struct Job {
    env: MutationEnvelope,
    resp: oneshot::Sender<Result<(), String>>,
}

impl Job {
    /// The wire `kind` of the mutation this job would have sent.
    ///
    /// The one thing a test needs off a job it intercepted, exposed as a
    /// reader rather than by widening the envelope: a job is a thing this
    /// module answers, not a thing another module takes apart.
    #[cfg(test)]
    pub fn kind(&self) -> &str {
        &self.env.kind
    }
}

/// The face's handle on the 9010 socket.
#[derive(Clone)]
pub struct Handle {
    out: mpsc::Sender<Job>,
    /// [`Reach`], split so a render pass can read it without a lock contending
    /// with the socket task: the discriminant is an atomic, the cause rides
    /// beside it.
    state: Arc<AtomicU8>,
    cause: Arc<Mutex<DownCause>>,
    attempt: Arc<AtomicU8>,
}

const S_ABSENT: u8 = 0;
const S_CONNECTING: u8 = 1;
const S_UP: u8 = 2;

impl Handle {
    pub fn reach(&self) -> Reach {
        let cause = *self.cause.lock().expect("cause lock");
        match self.state.load(Ordering::SeqCst) {
            S_UP => Reach::Up,
            S_CONNECTING => Reach::Connecting {
                attempt: u32::from(self.attempt.load(Ordering::SeqCst)),
                cause,
            },
            _ => Reach::Absent { cause },
        }
    }

    /// **SEND ONE MUTATION AND WAIT FOR ITS ACK.**
    ///
    /// Refused immediately, and never queued, when the link is down — the same
    /// rule and the same reason as [`crate::transport::Handle::call`]: a queued
    /// act would fire into whatever socket happens to exist minutes later, and
    /// `MovePin` landing minutes late renumbers a pin order he has since
    /// changed by hand.
    ///
    /// **A missing ack is "NOT PERFORMED", said out loud.** `0060` is explicit:
    /// *"treat a missing ack after the reconnect backoff as 'not performed' and
    /// say so — never assume."* The socket dying mid-flight answers every
    /// waiter with that sentence rather than leaving it hanging.
    pub async fn mutate(&self, m: Mutation) -> Result<(), String> {
        let reach = self.reach();
        if !reach.is_up() {
            // The row's own words, so the notice line and the row agree.
            return Err(reach
                .words()
                .unwrap_or(DownCause::DoorShut.words())
                .to_string());
        }
        let (tx, rx) = oneshot::channel();
        let env = m.into_envelope();
        let kind = env.kind.clone();
        if self.out.send(Job { env, resp: tx }).await.is_err() {
            return Err(format!(
                "{kind}: the 9010 client is gone — nothing was sent"
            ));
        }
        rx.await.map_err(|_| {
            format!(
                "{kind}: the socket closed before this was answered — treat it as NOT performed"
            )
        })?
    }
}

// ────────────────────────────────────────────────────────────────────────────
//  configuration and the ladder
// ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Config {
    /// `None` means resolve on every attempt (the live posture). `Some` pins
    /// one URL, which is what a test against a scripted fake does.
    pub endpoint: Option<String>,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// How long one mutation may wait for its `MutAck` before it is reported as
    /// NOT PERFORMED. Generous, because the AUI does real work inside some
    /// handlers (`RemoveProject` touches the filesystem) — but bounded, because
    /// a row that spins forever tells him nothing.
    pub ack_timeout: Duration,
}

impl Config {
    pub fn live() -> Self {
        Self {
            endpoint: None,
            // The same curve as the daemon transport, deliberately: two
            // reconnect policies on one screen is one policy nobody can
            // predict.
            backoff_min: Duration::from_secs(2),
            backoff_max: Duration::from_secs(30),
            ack_timeout: Duration::from_secs(20),
        }
    }

    pub fn pinned(url: impl Into<String>) -> Self {
        Self {
            endpoint: Some(url.into()),
            ..Config::live()
        }
    }
}

/// Backoff for attempt `n` (1-based), doubling from the floor to the ceiling.
pub fn backoff(cfg: &Config, attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(16);
    let ms = (cfg.backoff_min.as_millis() as u64).saturating_mul(1u64 << shift);
    Duration::from_millis(ms.min(cfg.backoff_max.as_millis() as u64))
}

/// Start the 9010 client. Returns the face's handle and the event stream.
/// **A HANDLE THAT REPORTS `Up` WITH THE TEST'S OWN CHANNEL BEHIND IT**, and
/// no socket anywhere.
///
/// Unit tests of the PROJECT sheet need an AUI that is REACHABLE: every
/// AUI-owned row is muted otherwise, and `App::sheet_act` returns on the
/// unavailable branch before it decides anything about arming. A live socket is
/// not an option for those acts — `RemoveProject` de-registers a project on
/// his real machine (it stopped unlinking `~/minds/<name>` at the registry
/// cutover; see `crate::acts::ProjectAct`) — and a fake server is an
/// integration-test shape that a unit test in this crate cannot reach.
///
/// So this hands the caller the OUTBOUND END. The test holds the receiver and
/// asserts on the jobs the app ENQUEUED, which is one step earlier than the
/// bytes and is exactly where "nothing left the process" is decided. That makes
/// the arming proof a POSITIVE witness rather than an absence: the same test
/// sees the second press arrive on the channel, so a first press that enqueues
/// nothing means something.
///
/// Nothing drains this channel, which is deliberate — a job on it has not been
/// sent and never will be.
#[cfg(test)]
pub fn test_handle_up() -> (Handle, mpsc::Receiver<Job>) {
    let (out_tx, out_rx) = mpsc::channel::<Job>(64);
    (
        Handle {
            out: out_tx,
            state: Arc::new(AtomicU8::new(S_UP)),
            cause: Arc::new(Mutex::new(DownCause::DoorShut)),
            attempt: Arc::new(AtomicU8::new(0)),
        },
        out_rx,
    )
}

pub fn spawn(cfg: Config) -> (Handle, mpsc::Receiver<AuiEvent>) {
    // **REL-6a3 F1: THE 9010 CONSTRUCTOR REFUSES UNDER ISOLATION, like every
    // other connection constructor in this crate.** Unpointed, the AUI is
    // 9010 — his live ui-server — so an isolated process (a test, a harness,
    // a drive, a non-interactive launch) that forgot `AUI_V2_URL` is refused
    // here, loudly, before a task or socket exists. The endpoint is re-read
    // on every attempt, so [`session`] refuses again before each dial.
    let endpoint = cfg.endpoint.clone().unwrap_or_else(resolve_endpoint);
    if let Some(refusal) = crate::cauldron::live::refuse_isolated_url("AUI", &endpoint) {
        panic!("{refusal}");
    }
    let (out_tx, out_rx) = mpsc::channel::<Job>(64);
    let (ev_tx, ev_rx) = mpsc::channel::<AuiEvent>(256);
    let handle = Handle {
        out: out_tx,
        state: Arc::new(AtomicU8::new(S_ABSENT)),
        cause: Arc::new(Mutex::new(DownCause::DoorShut)),
        attempt: Arc::new(AtomicU8::new(0)),
    };
    tokio::spawn(supervise(cfg, out_rx, ev_tx, handle.clone()));
    (handle, ev_rx)
}

/// **DRAIN, for the same reason the daemon transport drains.**
///
/// Every dropped job is ANSWERED with a sentence naming what happened, never
/// dropped: a caller unblocked by a closed channel gets channel-shaped noise,
/// and the row's refusal has to be able to say something true.
fn drain(out_rx: &mut mpsc::Receiver<Job>, why: &str) -> usize {
    let mut n = 0;
    while let Ok(job) = out_rx.try_recv() {
        let _ = job.resp.send(Err(format!("{}: {why}", job.env.kind)));
        n += 1;
    }
    n
}

async fn supervise(
    cfg: Config,
    mut out_rx: mpsc::Receiver<Job>,
    ev_tx: mpsc::Sender<AuiEvent>,
    h: Handle,
) {
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let cause = *h.cause.lock().expect("cause lock");
        h.attempt
            .store(u8::try_from(attempt).unwrap_or(u8::MAX), Ordering::SeqCst);
        h.state.store(S_CONNECTING, Ordering::SeqCst);
        if ev_tx
            .send(AuiEvent::Reach(Reach::Connecting { attempt, cause }))
            .await
            .is_err()
        {
            return; // the face is gone
        }

        // **ALWAYS SOME**, now that the default is loopback rather than a
        // discovered tailnet address — see `endpoint_from`. There is no more
        // "no route could even be located" state to fall into here; a closed
        // door is now always something `session`'s connect attempt discovers.
        let endpoint = cfg.endpoint.clone().unwrap_or_else(resolve_endpoint);

        let cause = match session(&cfg, &endpoint, &mut out_rx, &ev_tx, &h).await {
            Ok(()) => return, // the face went away
            Err(c) => c,
        };
        // **READ IT BEFORE `down` CLEARS IT.** The ladder resets only on a
        // session that got all the way up, and that fact has to be captured
        // here because the very next line destroys it.
        let was_up = h.state.load(Ordering::SeqCst) == S_UP;
        down(&h, &ev_tx, cause).await;
        let dropped = drain(&mut out_rx, "the 9010 socket closed before this was sent");
        if dropped > 0 {
            tracing::warn!(
                dropped,
                "9010 outbound drained — not one of these fired into a new socket"
            );
        }
        if wait(&cfg, attempt, &ev_tx).await.is_err() {
            return;
        }
        if was_up {
            attempt = 0;
        }
    }
}

/// **THE LINK IS DOWN THE INSTANT THE SESSION ENDS — NOT WHEN THE BACKOFF
/// EXPIRES.**
///
/// The state is stored HERE, before the ladder's sleep, and that ordering is
/// load-bearing rather than tidy. It used to be stored after the sleep, and the
/// window in between was up to THIRTY SECONDS during which
/// [`Handle::reach`] still answered `Up`: every AUI act row drew itself as
/// available with no socket behind it, and [`Handle::mutate`] passed its guard
/// and ENQUEUED — so the job outlived the drain and fired into the next
/// session. That is precisely the failure the drain exists to prevent, arriving
/// through the front door: a `MovePin` landing a minute late renumbers an order
/// he has since fixed by hand.
///
/// Closing it at the SOURCE — the link is down, so the guard refuses — is worth
/// more than a second drain, which would only have narrowed the window.
async fn down(h: &Handle, ev_tx: &mpsc::Sender<AuiEvent>, cause: DownCause) {
    *h.cause.lock().expect("cause lock") = cause;
    h.state.store(S_ABSENT, Ordering::SeqCst);
    let _ = ev_tx.send(AuiEvent::Reach(Reach::Absent { cause })).await;
}

async fn wait(cfg: &Config, attempt: u32, ev_tx: &mpsc::Sender<AuiEvent>) -> Result<(), ()> {
    tokio::time::sleep(backoff(cfg, attempt)).await;
    if ev_tx.is_closed() {
        return Err(());
    }
    Ok(())
}

/// One connection. `Ok(())` only when the FACE went away; every other end is a
/// [`DownCause`] to draw.
async fn session(
    cfg: &Config,
    endpoint: &str,
    out_rx: &mut mpsc::Receiver<Job>,
    ev_tx: &mpsc::Sender<AuiEvent>,
    h: &Handle,
) -> Result<(), DownCause> {
    // REL-6a3 F1: the per-attempt half of `spawn`'s refusal (AUI_V2_URL may
    // change between attempts). Never dialled; drawn as a shut door.
    if let Some(refusal) = crate::cauldron::live::refuse_isolated_url("AUI", endpoint) {
        tracing::warn!(%endpoint, "{refusal}");
        return Err(DownCause::DoorShut);
    }
    let ws = match crate::cauldron::live::dial_ws("AUI", endpoint).await {
        Ok(ws) => ws,
        Err(crate::cauldron::live::DialError::Refused(refusal)) => {
            tracing::warn!(%endpoint, "{refusal}");
            return Err(DownCause::DoorShut);
        }
        Err(e) => {
            let cause = DownCause::of_connect_error(&e.to_string());
            tracing::info!(%endpoint, error = %e, cause = ?cause, "9010 is not reachable — this is a normal state");
            return Err(cause);
        }
    };
    let (mut sink, mut stream) = ws.split();

    // The handshake is one frame and there is no auth: this face dials
    // loopback, so "a process on this machine" IS the auth boundary — the same
    // story `apps/aui/src/v2/mod.rs`'s module header tells for this door (the
    // tailnet plays that role for the AUI's OTHER door, the phone's).
    let hello = ClientMsg::Hello {
        client: ClientInfo {
            kind: CLIENT_KIND.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            device_id: DEVICE_ID.into(),
        },
        proto_version: PROTO_VERSION,
    };
    let frame = serde_json::to_string(&hello).map_err(|_| DownCause::DoorShut)?;
    if sink.send(Message::Text(frame)).await.is_err() {
        return Err(DownCause::NotRunning);
    }

    let mut pending: HashMap<Uuid, oneshot::Sender<Result<(), String>>> = HashMap::new();
    let mut up = false;

    let end = loop {
        tokio::select! {
            // OUTBOUND, enabled only once `ServerInfo` has landed. Until then
            // a job SITS in the channel — which is exactly the state the drain
            // exists to clean up, and it is why `mutate` checks `reach` first.
            job = out_rx.recv(), if up => {
                let Some(job) = job else { break None };  // the face is gone
                let id = Uuid::new_v4();
                let msg = ClientMsg::Mutate { id, m: job.env.clone() };
                let text = match serde_json::to_string(&msg) {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = job.resp.send(Err(format!("{}: could not be encoded: {e}", job.env.kind)));
                        continue;
                    }
                };
                tracing::info!(kind = %job.env.kind, %id, "9010 mutate");
                if sink.send(Message::Text(text)).await.is_err() {
                    let _ = job.resp.send(Err(format!(
                        "{}: the socket write failed — treat it as NOT performed", job.env.kind
                    )));
                    break Some(DownCause::NotRunning);
                }
                pending.insert(id, job.resp);
            }

            incoming = stream.next() => {
                let text = match incoming {
                    None | Some(Err(_)) | Some(Ok(Message::Close(_))) =>
                        break Some(DownCause::NotRunning),
                    Some(Ok(Message::Text(t))) => t,
                    Some(Ok(_)) => continue,
                };
                // TOLERANT: this crate does not fail a session because the AUI
                // grew a frame it has not heard of.
                let Ok(msg) = serde_json::from_str::<ServerMsg>(&text) else {
                    tracing::debug!(%text, "9010: unrecognized frame — ignored");
                    continue;
                };
                match msg {
                    ServerMsg::ServerInfo { version, proto_version, .. } => {
                        tracing::info!(%version, proto_version, "9010 is up");
                        up = true;
                        h.state.store(S_UP, Ordering::SeqCst);
                        if ev_tx.send(AuiEvent::Reach(Reach::Up)).await.is_err() {
                            break None;
                        }
                    }
                    ServerMsg::MutAck { id, result } => {
                        match pending.remove(&id) {
                            Some(resp) => {
                                let _ = resp.send(match result {
                                    MutResult::Ok => Ok(()),
                                    // **THE AUI'S OWN SENTENCE, VERBATIM.** Its
                                    // refusals are already written for a human
                                    // ("is a real directory here, not a link —
                                    // removing it is not something a menu row
                                    // may do"); re-wording one here would make
                                    // the face paraphrase the only party that
                                    // knows what went wrong.
                                    MutResult::Err { message } => Err(message),
                                });
                            }
                            None => tracing::warn!(%id, "9010: MutAck for an id we do not hold"),
                        }
                    }
                    ServerMsg::Invalidate { node, .. } => {
                        // The KIND only. The face never learns a node's params
                        // or its version from here — it re-reads the files it
                        // already reads, and 9010 is a hint about WHEN, never a
                        // source of what.
                        let node = node.kind.clone();
                        if ev_tx.send(AuiEvent::Invalidate { node }).await.is_err() {
                            break None;
                        }
                    }
                    // Everything else on this socket belongs to the phone:
                    // promises, projections, turn events, alert counts. This
                    // face asked for none of it and renders none of it.
                    _ => {}
                }
            }
        }
    };

    // Every in-flight mutation is ANSWERED. A mutation whose ack never came is
    // NOT PERFORMED as far as this face is concerned, and it says so.
    for (_, resp) in pending.drain() {
        let _ = resp.send(Err(
            "the 9010 socket closed before this was acked — treat it as NOT performed".into(),
        ));
    }
    let _ = cfg;
    match end {
        None => Ok(()),
        Some(cause) => Err(cause),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **THE ADDRESS IS RESOLVED IN ONE ORDER, AND THE DEFAULT IS LOOPBACK —
    /// EXACTLY, NOT "SOMETHING LOCAL-LOOKING."**
    ///
    /// Positive assertions throughout: this binds to EXACTLY
    /// `ws://127.0.0.1:9010/`, not merely "does not contain tailscale" — a
    /// negative like that goes silent the moment the fallback is renamed to
    /// something else that is just as broken.
    ///
    /// Forced red (pre-fix tree, `git show HEAD~1`-equivalent of this
    /// function): the old third branch shelled out to `tailscale ip -4` and
    /// returned `None` when that failed, so
    /// `endpoint_from(None, None, None) == None` was the asserted case — call
    /// the OLD three-argument form with no tailscale output and it asserts
    /// `None`; call this one and it must assert the loopback literal instead,
    /// which is the behavior change this test now locks in.
    #[test]
    fn the_door_is_resolved_highest_first_and_defaults_to_loopback() {
        assert_eq!(
            endpoint_from(Some("ws://pinned:1/".into())),
            "ws://pinned:1/",
            "an explicit URL outranks the default"
        );
        // **THE ONE THAT MATTERS.** No explicit URL binds to EXACTLY the
        // loopback default — no tailscale subprocess, no `None`, no guess.
        assert_eq!(
            endpoint_from(None),
            "ws://127.0.0.1:9010/",
            "with nothing explicit, the door is loopback, exactly"
        );
        assert_eq!(
            endpoint_from(Some("  ".into())),
            "ws://127.0.0.1:9010/",
            "blank is absent, not a value — falls through to loopback"
        );
    }

    /// The port comes from the crate the SERVER reads it from, so the two
    /// cannot disagree about where the door is.
    #[test]
    fn the_port_is_the_servers_own_constant() {
        assert_eq!(DEFAULT_PORT, 9010);
        assert!(endpoint_from(None).ends_with(":9010/"));
    }

    /// **NO SUBPROCESS, OF ANY NAME, IS SPAWNED TO FIND THIS FACE'S DOOR.**
    ///
    /// Positive form of the ban: `endpoint_from` and `resolve_endpoint` take
    /// no fallible, external-process-shaped input at all any more —
    /// `endpoint_from` is `fn(Option<String>) -> String`, a pure function of
    /// one optional string, which makes "it shells out to find its address"
    /// a type error, not a behavior someone could quietly reintroduce behind
    /// this signature.
    #[test]
    fn endpoint_from_takes_no_process_output_and_always_returns_a_door() {
        fn _type_check(f: fn(Option<String>) -> String) {
            let _ = f;
        }
        _type_check(endpoint_from);
    }

    /// **THE BAN ITSELF: NO `tailscale` SUBPROCESS IN THIS FILE'S SOURCE —
    /// AND THE BAN DOES NOT WITNESS ITSELF.**
    ///
    /// This scans THIS crate's own `aui.rs` for the exact call that used to
    /// shell out to find the door: a `std::process::Command` built from the
    /// VPN binary's name. Proved red against the pre-fix tree, by hand, this
    /// round — the old `resolve_endpoint` in the committed pre-fix revision
    /// of this file contains exactly that call — so this is a ban on a
    /// spelling that was actually there, not a sentence that can never fail.
    ///
    /// **The self-witness bug, avoided on purpose, TWICE.** First: the needle
    /// below is assembled from two halves rather than one contiguous literal.
    /// Second — and this is the one that actually bit this test on its first
    /// draft — EVERY MENTION IN THIS VERY COMMENT BLOCK must avoid spelling
    /// the call out contiguously too, because `include_str!` reads this
    /// file's doc comments right along with its code: the first draft of this
    /// doc wrote the banned call out in full, right here, to explain what it
    /// bans, and the test failed by finding ITSELF. That exact mistake is on
    /// record at `apps/aui/src/dispatch/root.rs` and has now recurred THREE
    /// times in this operator's codebases, plus once more inside the very fix
    /// for it just now.
    #[test]
    fn no_subprocess_is_shelled_out_to_find_this_faces_door() {
        let src = include_str!("aui.rs");
        let needle = format!("Command::new(\"{}{}\")", "tail", "scale");
        assert!(
            !src.contains(&needle),
            "this file shells out to `tailscale` to resolve its door — the exact \
             thing the loopback default exists to make unnecessary"
        );
    }

    /// **THE TWO SENTENCES ARE FIXED BY THE DESIGN AND CHOSEN BY THE SOCKET.**
    #[test]
    fn a_refusal_and_an_unreachable_route_say_different_true_things() {
        assert_eq!(
            DownCause::of_connect_error("IO error: Connection refused (os error 61)"),
            DownCause::NotRunning
        );
        for unreachable in [
            "IO error: No route to host (os error 65)",
            "IO error: Can't assign requested address (os error 49)",
            "failed to lookup address information",
            "deadline has elapsed",
        ] {
            assert_eq!(
                DownCause::of_connect_error(unreachable),
                DownCause::DoorShut,
                "{unreachable} was read as 'the process is down'"
            );
        }
        assert_eq!(
            DownCause::NotRunning.words(),
            "needs the audio interface, which is not running"
        );
        assert_eq!(
            DownCause::DoorShut.words(),
            "the audio interface's door is not open"
        );
    }

    /// A row's words follow the link state, and **`Connecting` counts as
    /// ABSENT**. A row that promised to work because a handshake was halfway
    /// done would lie for two seconds at a time, on a ladder that reaches
    /// thirty.
    #[test]
    fn connecting_is_drawn_as_unavailable_and_only_up_is_available() {
        assert_eq!(Reach::Up.words(), None);
        assert!(Reach::Up.is_up());
        let c = DownCause::NotRunning;
        assert_eq!(Reach::Absent { cause: c }.words(), Some(c.words()));
        assert_eq!(
            Reach::Connecting {
                attempt: 3,
                cause: c
            }
            .words(),
            Some(c.words())
        );
        assert!(!Reach::Connecting {
            attempt: 3,
            cause: c
        }
        .is_up());
    }

    #[test]
    fn the_ladder_matches_the_daemon_transports_curve() {
        let c = Config::live();
        assert_eq!(backoff(&c, 1), Duration::from_secs(2));
        assert_eq!(backoff(&c, 2), Duration::from_secs(4));
        assert_eq!(backoff(&c, 5), Duration::from_secs(30));
        assert_eq!(backoff(&c, 99), Duration::from_secs(30));
    }

    /// Only the node kinds that change an index file cost a re-read.
    #[test]
    fn an_invalidate_for_a_node_no_index_holds_costs_no_reread() {
        for yes in INDEX_NODES {
            assert!(invalidate_touches_index(yes), "{yes}");
        }
        for no in ["thread_options", "pending_inputs", "project_options", ""] {
            assert!(
                !invalidate_touches_index(no),
                "{no} triggered an index re-read it cannot possibly affect"
            );
        }
    }

    /// **A CALL WITH NO LINK IS REFUSED IN THE ROW'S OWN WORDS, AND NEVER
    /// QUEUED.** Queueing it would fire his `MovePin` into whatever socket
    /// exists minutes later, renumbering an order he has since fixed by hand.
    #[tokio::test]
    async fn a_mutation_with_no_link_is_refused_in_words_and_never_queued() {
        let (tx, mut rx) = mpsc::channel::<Job>(8);
        let h = Handle {
            out: tx,
            state: Arc::new(AtomicU8::new(S_ABSENT)),
            cause: Arc::new(Mutex::new(DownCause::NotRunning)),
            attempt: Arc::new(AtomicU8::new(0)),
        };
        let err = h
            .mutate(Mutation::SetThreadPinned {
                project: "harness".into(),
                thread_id: Uuid::nil(),
                pinned: true,
            })
            .await
            .expect_err("a mutation went out with no socket");
        assert_eq!(err, DownCause::NotRunning.words());
        assert!(
            rx.try_recv().is_err(),
            "a refused mutation sat in the queue waiting for a fresh socket"
        );
    }

    /// The drain answers every waiter rather than dropping it. N >= 2, because
    /// one item cannot show the loop keeps going.
    #[tokio::test]
    async fn the_drain_answers_every_waiting_mutation() {
        let (tx, mut rx) = mpsc::channel::<Job>(8);
        let mut waiters = Vec::new();
        for m in [
            Mutation::SetThreadClosed {
                project: "p".into(),
                thread_id: Uuid::nil(),
                closed: true,
            },
            Mutation::SetThreadSummary {
                project: "p".into(),
                thread_id: Uuid::nil(),
                summary: "x".into(),
            },
        ] {
            let (rtx, rrx) = oneshot::channel();
            let env = m.into_envelope();
            let kind = env.kind.clone();
            tx.send(Job { env, resp: rtx }).await.expect("queued");
            waiters.push((kind, rrx));
        }
        assert_eq!(
            drain(&mut rx, "the 9010 socket closed before this was sent"),
            2
        );
        for (kind, rrx) in waiters {
            let err = rrx
                .await
                .expect("a drained caller must be ANSWERED, not dropped")
                .expect_err("a drained mutation did not reach the AUI");
            assert!(
                err.starts_with(&kind),
                "the refusal must name the act: {err}"
            );
            assert!(err.contains("closed before this was sent"), "{err}");
        }
        assert_eq!(drain(&mut rx, "again"), 0);
    }
}
