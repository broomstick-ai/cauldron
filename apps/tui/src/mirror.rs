//! # `mirror.rs` — **THE FACE'S HALF OF THE SEAM.**
//!
//! Everything the terminal face supplies to [`crate::seam`], or does with what
//! the seam hands back. The seam is a COPY — byte-identical to
//! `libs/shapewire/seam/*.rs` outside ONE fence, which
//! `tests/it/seam_reference.rs` checks — so anything face-specific has to live
//! here, above it. That principle is the whole reason this file exists, and it
//! is the same sentence `apps/aui/src/mirror.rs` opens with:
//!
//! > *Anything a face supplies to the seam, or does with its output, lives above
//! > the seam; only what the seam must **say** can differ.*
//!
//! ## The three divergences that landed here, and they are the AUI's three
//!
//! * **D1 — who may start the daemon.** [`spawn`] passes
//!   [`seam::link::Supervision`] [`EgressGate`] — a hook that STARTS NOTHING
//!   and only refuses (REL-6a5). **The terminal face does not start the
//!   threads daemon and never did** — an unreachable daemon is a DRAWN state
//!   here, not an error path, and `Handle::call` refused loudly rather than
//!   spawning anything. That is why this manifest has no `reqwest`: the
//!   `/health` probe, the ~25 ms `execv` grace and the detached spawn are the
//!   AUI's hook (`apps/aui/src/mirror.rs::DaemonSupervisor`) and they stay
//!   there.
//! * **D5 — where the handle lives.** A field on `App`, reached through
//!   [`Handle`], because this face has ONE loop that owns everything. The AUI
//!   holds a `static OnceCell` because it has many tasks and no central owner.
//!   Neither needs a line of the seam to change.
//! * **D6 — the event adapter.** [`forward`], which turns a
//!   [`seam::SeamEvent`] into the [`TransportEvent`] vocabulary this face
//!   already speaks. The AUI's answer to the same event is a spoken sentence;
//!   this face's is a note row and a repaint. Neither belongs in a file both
//!   copy.
//!
//! ## What this module is NOT: a translation of questions into other questions
//!
//! > *"We should not have new paths to request data. That's adding features to
//! > the producer that we didn't want to support. **We should remove the ability
//! > to request data.**"*
//!
//! `src/methods.rs` named nine methods; six of them were READS and every one is
//! GONE, not renamed and not re-homed. What holds each fact now:
//!
//! | the read that was there | what holds that fact now |
//! |---|---|
//! | `threads/list` | nothing asks. The window is `threads/recent-updates` (the producer's own 32 retained, newest first); the executing set is `accounts.*.threads` |
//! | `threads/status` | the `threads` shape's `status` cell — `{state, executing, detail}` — per held key, current between repaints rather than at the moment of asking |
//! | `threads/{id}/get` | `threads.{id}.newest.seq`, through [`seam::view::positions`]. It was asked for ONE number, `last_seq`, and the shape carries it |
//! | `threads/{id}/history` | `threads/log` — the producer's 4,000-row window, every kind, unfiltered, read through [`seam::ledger::rows_after`] |
//! | `threads/{id}/queue` | the `threads` shape's `queue` cell — `list<struct:QueueEntry>`, declared at `seam::inventory::threads::QUEUE` — read through [`seam::view::queue`]. See [`THE_ENVELOPED_QUEUE_READS`] for why the LEAF NAME survives the read's retirement |
//! | `threads/{id}/queue_failed` | the SAME `queue` cell, filtered on `state == "failed"` by [`seam::view::queue_failed`]. There is no second cell and there is no second call |
//!
//! ## THE EIGHTH SHAPE, `warnings`, AND WHY THIS FACE DOES NOT RENDER IT YET
//!
//! The producer serves it. The vendor bump at `9235e8e` brought its whole
//! COMPILE-TIME half into this repo: `inventory.json` lists it as the eighth
//! shape, and `seam::inventory::warnings` mints a `Carried` token for every cell
//! it declares — `warn`, `error`, `kinds`, `kinds_total`, `sink_errors`. So the
//! count the operator asked for (*"display the number of warnings coming from
//! threads so it annoys me"*) is `warnings.daemon.warn`, with `error` carried
//! apart and never summed into it by the producer.
//!
//! **What is missing is in the SEAM, not here, and it cannot be added here.**
//! `seam::held` declares no `WARNINGS` const, `Held::new` builds no mirror for it,
//! `Book::standing` holds no entry for it, and `seam::view` has no accessor. A
//! `type: shape` frame for `warnings` therefore reaches `Held::fold`, finds no
//! mirror, and is counted as `reject::NOT_MINE` — so subscribing from above the
//! seam would hold a shape whose every frame is discarded. `Held`'s `mirrors` map
//! is private and there is no `add_mirror`, so there is no door.
//!
//! Adding one to this face's `src/seam/held.rs` would be a divergence OUTSIDE a
//! fence, which `tests/it/seam_reference.rs` reddens by design, and §7.1 names the
//! correct direction of pressure: the fix belongs UPSTREAM in
//! `substrate-interface-layer:lib/shapewire/seam/`, followed by a second vendor
//! bump. That is a chassis release, not a face release, and it is deliberately
//! not in this one. Nothing else blocks it.
//!
//! ## How a three-valued `Fact` becomes the store's `Known`, for free
//!
//! The projection below emits a field **only when the mirror's answer is
//! `Fact::Known`**, and that is not tidiness — it is the one line that makes
//! this a mirror rather than a rewrite. `store.rs` already reads a fleet row as
//! `if let Some(v) = s.get("state")` for the sticky fields and
//! `f.queued = known_u32(s.get("queued"))` for the counted ones, so an OMITTED
//! field is already spelled *"I have no current answer"* and lands as
//! `Known::Unknown`. A `NotHydrated` cell therefore reaches the screen as the
//! `?` it always was, by arithmetic rather than by a new code path, and
//! **`store.rs` has zero lines changed by this migration.**
//!
//! Rendering a stale value as current is the defect the whole shape contract
//! exists to prevent; the shape of the `Fact` enum and the shape of the store's
//! `Known` happen to agree about it exactly.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::event::Ev;
use crate::proto::{ServerMsg, ThreadEvent};
use crate::seam::{self, held, ledger, link, view, Seam};
use crate::store::LedgerRow;
use crate::transport::{NoteLevel, ResyncKind, TransportEvent};

#[cfg(test)]
pub mod fixtures;

// ──────────────────────────────────────────────────────────────────────────
//  The two reads no shape answers, named rather than smuggled
// ──────────────────────────────────────────────────────────────────────────

/// **THE TWO LEAF NAMES THAT ARE STILL SPOKEN, AND NOT BECAUSE ANYTHING IS
/// UNSHAPED.**
///
/// **A declared shape answers both of these, and this list is a REPLY ENVELOPE,
/// not a carve-out from *only the shapes*.** `THE_UNSHAPED_READS` is what this
/// constant used to be called and the name was false by the time it was read:
/// the ruling it was waiting for had already landed. The `threads` shape
/// carries a `queue` cell — `list<struct:QueueEntry>`, declared at
/// `seam::inventory::threads::QUEUE` and listed in `libs/shapewire/inventory.json`
/// — and [`Handle::enveloped_queue_read`] answers both leaves off the held
/// mirror. **No request leaves this face for either of them.** The producer
/// does not dispatch `threads/{id}/queue` or `threads/{id}/queue_failed` any
/// more, so there is nothing left for either name to ask.
///
/// **What the two entries ARE, then.** They are the two keys
/// [`crate::queue_reads`] still calls with, and each one selects the legacy
/// reply key its caller destructures: `queue` → `{"waiting": [...]}`,
/// `queue_failed` → `{"failed": [...]}`. One cell, one state, two envelopes
/// over it. The envelope is kept so the queue panel's call shape did not have
/// to change when the cell landed underneath it.
///
/// **They are still not in the seam**, and that part was always about
/// placement rather than about shapes: `commands::methods::TEMPLATES` is
/// commands only and `seamdiff --inventory` routes it, so neither name can
/// appear below the seam. They sit ABOVE it, in [`crate::queue_reads`].
///
/// **And it is still pinned at TWO.**
/// `the_only_enveloped_queue_reads_are_these_two` fails if a third appears, so
/// a later worker cannot widen the envelope quietly — which is now a narrower
/// claim than it was, and an honest one: a third entry here would be a third
/// legacy reply key, not a third read escaping the shapes.
pub const THE_ENVELOPED_QUEUE_READS: &[&str] = &["queue", "queue_failed"];

// ──────────────────────────────────────────────────────────────────────────
//  The handle — D5
// ──────────────────────────────────────────────────────────────────────────

/// **WHAT THE FACE HOLDS**, and a drop-in for the `transport::Handle` it
/// replaces: `is_live`, `queued`, and one door for each thing the face can say.
///
/// Cheap to clone; everything behind it is one task and one mutex.
#[derive(Clone)]
pub struct Handle {
    seam: Arc<Seam>,
    /// The elided bodies this face has already fetched, shared by the live
    /// rows and the transcript load. See [`Bodies`].
    bodies: Bodies,
}

impl Handle {
    /// Where the one socket stands now (REL-6a5's reconnect test reads it).
    pub fn link_state(&self) -> held::LinkState {
        self.seam.lock().link.clone()
    }

    /// Whether frames are reaching us right now. **Never used to decide what to
    /// RENDER** — that follows [`TransportEvent`], because that is ordered with
    /// respect to the frames and this is not.
    pub fn is_live(&self) -> bool {
        self.read(|h| view::link(h).is_up())
    }

    /// **ZERO, ALWAYS, AND THE NUMBER IS HONEST.**
    ///
    /// The old transport queued RPCs against a handshake and this counted them,
    /// so the face could put *"3 calls waiting"* in a banner. There is no
    /// request queue left to count: the seam's commands take the link or refuse
    /// at once, and there are no reads at all. Kept rather than deleted because
    /// `app.rs` reads it for that banner segment, and a segment that always says
    /// nothing is the correct rendering of a queue that cannot exist.
    pub fn queued(&self) -> usize {
        0
    }

    /// Read the mirror under the one lock.
    pub fn read<T>(&self, f: impl FnOnce(&held::Held) -> T) -> T {
        self.seam.read(f)
    }

    /// Hold one thread's row, so its cells arrive. Idempotent.
    pub async fn hold_thread(&self, id: &str) {
        crate::diag!("shape", "HOLD shape={} key={id}", held::THREADS);
        self.seam.hold(held::THREADS, id).await;
    }

    /// Open a `threads/log` window on `id`, evicting the least recently opened
    /// one if the producer's ceiling is already reached. The ceiling is read off
    /// `shapes/list` and never hardcoded — see [`seam::ledger::capacity`].
    pub async fn watch_log(&self, id: &str) {
        crate::diag!("shape", "WATCH shape={} key={id} (subscribe if not held)", held::LOG);
        self.seam.watch_log(id).await;
        crate::diag!(
            "shape",
            "WATCHED shape={} key={id} open_windows={:?} state={}",
            held::LOG,
            self.read(ledger::open),
            self.read(|h| log_state(h, id))
        );
    }

    pub async fn unwatch_log(&self, id: &str) {
        crate::diag!("shape", "UNWATCH shape={} key={id}", held::LOG);
        self.seam.unwatch_log(id).await;
    }

    // ── THE VERBS. Three, and they acknowledge rather than answer. ────────

    /// **THE WHOLE OF WHAT THIS FACE CAN SAY TO THE DAEMON, AS THREE CALLS.**
    ///
    /// Not a `call(&method, params)` door, deliberately: a generic door is how
    /// a read gets smuggled back in at a call site, and this face had exactly
    /// that — `acts::Wire::Daemon { method: String, params: Value }` dispatched
    /// through one `Handle::call`, so the set of things it could say was a grep
    /// and not a type. [`crate::acts::Verb`] is a three-variant enum now and
    /// these are its three landings.
    ///
    /// `seam::commands` carries SIX verbs — `update`, `seen` and `event` too —
    /// and that is vocabulary rather than policy (divergence D4, dissolved):
    /// which ones a face CALLS is above the seam, and this face calls three.
    pub async fn send(
        &self,
        thread: &str,
        text: &str,
        intent: Option<&str>,
        bare: bool,
        headers: &[Value],
        metadata: Value,
    ) -> Result<seam::commands::Ack, String> {
        seam::commands::send(&self.seam, thread, text, intent, bare, headers, metadata)
            .await
            .map_err(refusal)
    }

    /// **INTERRUPT THE RUNNING TURN — AND SAY SO IN THE FLIGHT RECORDER, BOTH
    /// WAYS** (TC-46, TUI-P1f).
    ///
    /// Every cancel this face sends lands here (`verb` is the only caller and
    /// `acts::route` is the only maker of `Verb::Cancel`), so this is the one
    /// place the two lines are written: `SEND` before the request leaves —
    /// UTC time and pid from the recorder, the thread, the verb, the endpoint
    /// and `via`, the key or path that asked for it — and `REPLY` when the
    /// answer comes back, ok or error, with the body or refusal summarised and
    /// the latency in ms. A refusal, a dropped socket and the seam's own 30 s
    /// timeout all come back as `Err` and are logged as `result=error`. The
    /// two lines share one `rid=`, so an incident can be rebuilt from
    /// `~/.tui/diag.log` alone. Neither line carries message text or
    /// credentials: a cancel has no text, and the body is the daemon's ack.
    ///
    /// What is SENT is unchanged: `threads/{thread_id}/cancel`, params `{}`.
    pub async fn cancel(&self, thread: &str, via: &str) -> Result<seam::commands::Ack, String> {
        let rid = cancel_rid();
        crate::diag!(
            "cancel",
            "SEND rid={rid} thread={thread} verb=cancel endpoint={} via={via:?}",
            seam::commands::methods::cancel(thread).path()
        );
        let t0 = std::time::Instant::now();
        let result = seam::commands::cancel(&self.seam, thread)
            .await
            .map_err(refusal);
        let ms = t0.elapsed().as_millis();
        match &result {
            Ok(ack) => crate::diag!(
                "cancel",
                "REPLY rid={rid} thread={thread} result=ok latency_ms={ms} visible={} changed={:?} promise_id={:?} body={}",
                ack.visible,
                ack.changed,
                ack.promise_id,
                crate::diag::preview(&ack.payload.to_string(), 160)
            ),
            Err(why) => crate::diag!(
                "cancel",
                "REPLY rid={rid} thread={thread} result=error latency_ms={ms} error={}",
                crate::diag::preview(why, 200)
            ),
        }
        result
    }

    pub async fn create(
        &self,
        thread: &str,
        settings: Value,
        name: Option<&str>,
    ) -> Result<seam::commands::Ack, String> {
        seam::commands::create(&self.seam, thread, settings, name)
            .await
            .map_err(refusal)
    }

    /// **NAME A THREAD** (TUI-P1a) — the thread's newest `summary_updated`,
    /// through the seam's `event` verb, marked `manual` because a person typed
    /// it. Non-addressable: a name is not a message and takes no message id.
    pub async fn name(&self, thread: &str, name: &str) -> Result<seam::commands::Ack, String> {
        seam::commands::event(&self.seam, thread, SUMMARY_EVENT_TYPE, name_payload(name, Utc::now()), false)
            .await
            .map_err(refusal)
    }

    /// **SAY ONE VERB.** The whole of what `acts::Wire::Daemon` dispatches,
    /// and the reason it is one function rather than three call sites: the
    /// exhaustive `match` is what makes "this face says three things" a
    /// compile-time fact instead of a grep over `app.rs`.
    pub async fn verb(&self, v: crate::acts::Verb) -> Result<seam::commands::Ack, String> {
        self.verb_via(v, "unnamed").await
    }

    /// [`Handle::verb`], naming the key or path that asked for it. Only a
    /// cancel records `via` (in its `SEND` diag line); the other verbs ignore
    /// it, and nothing about what is sent depends on it.
    pub async fn verb_via(
        &self,
        v: crate::acts::Verb,
        via: &str,
    ) -> Result<seam::commands::Ack, String> {
        use crate::acts::Verb;
        match v {
            Verb::Send {
                thread,
                text,
                intent,
                bare,
                headers,
                metadata,
            } => {
                self.send(&thread, &text, intent.as_deref(), bare, &headers, metadata)
                    .await
            }
            Verb::Cancel { thread } => self.cancel(&thread, via).await,
            Verb::Create {
                thread,
                settings,
                name,
            } => self.create(&thread, settings, name.as_deref()).await,
            Verb::Name { thread, name } => self.name(&thread, &name).await,
        }
    }

    /// **THE `threads.queue` CELL, UNDER ONE OF ITS TWO LEGACY REPLY KEYS.**
    /// `leaf` must be in [`THE_ENVELOPED_QUEUE_READS`] or this panics, which is
    /// deliberate: the envelope is a list of two and this is the only door
    /// through it, so a third key is a crash in a test rather than a reply
    /// shape nobody declared.
    ///
    /// **This asks the producer nothing.** It holds the thread's `threads` row
    /// and reads one declared cell off the mirror; the `{"waiting": […]}` /
    /// `{"failed": […]}` wrapper is the only thing the leaf name still selects.
    /// See [`crate::queue_reads`] for who calls it and why the wrapper is kept.
    pub async fn enveloped_queue_read(&self, leaf: &str, thread: &str) -> Result<Value, String> {
        assert!(
            THE_ENVELOPED_QUEUE_READS.contains(&leaf),
            "`{leaf}` is not one of the two enveloped queue reads — the \
             envelope is {THE_ENVELOPED_QUEUE_READS:?} and widening it needs a ruling"
        );
        // **THE RULING LANDED: IT IS A CELL NOW, AND THIS IS NOT A REQUEST ANY
        // MORE.** The two reads were `threads/{id}/queue` and
        // `threads/{id}/queue_failed`, and the ruling owed was "a `queue` cell
        // on the `threads` shape, or the panel goes". The cell landed and the
        // producer stopped dispatching both methods — which is exactly what the
        // comment that stood here promised:
        // `seam::inventory::methods::THREADS_QUEUE` stopped existing and this
        // stopped compiling. So both leaves answer from the mirror: hold the
        // thread's row and read `view::queue`, one cell, one state, never a
        // second call. The `{"waiting": [..]}` / `{"failed": [..]}` reply shape
        // is kept so `queue_reads.rs` keeps its call shape.
        //
        // **R2.S2b: the rename this comment used to say was owed IS DONE.** The
        // constant and this function are named for the envelope they are,
        // rather than for a scarcity that had already ended. The two shas that
        // stood here as the ruling's provenance (`eeb2e33`, `fa34569`) are
        // REMOVED: neither resolves as an object in this repository
        // (`git cat-file -t` fails on both, and harness worktrees share one
        // object database, so that covers the whole of this repo's history).
        // A sha that does not resolve is not provenance.
        let field = match leaf {
            "queue" => "waiting",
            "queue_failed" => "failed",
            _ => unreachable!(
                "the assert above pins the envelope at {THE_ENVELOPED_QUEUE_READS:?}"
            ),
        };
        self.seam.hold(held::THREADS, thread).await;
        // One beat of patience for the export a FIRST hold asks for — bounded,
        // and an answer that never hydrates is an honest refusal, never `[]`.
        let mut last = String::new();
        for _ in 0..40 {
            let fact = self.seam.read(|h| {
                if leaf == "queue" {
                    view::queue(h, thread)
                } else {
                    view::queue_failed(h, thread)
                }
            });
            match fact {
                held::Fact::Known(rows) => {
                    let rows: Vec<Value> = rows
                        .into_iter()
                        .filter(|m| leaf == "queue_failed" || m.state != "failed")
                        .map(|m| {
                            json!({
                                "message_id": m.message_id,
                                "text": m.text,
                                "intent": m.intent,
                                "created_at": m.created_at,
                                "state": m.state,
                            })
                        })
                        .collect();
                    return Ok(json!({ field: rows }));
                }
                held::Fact::NotHydrated { reason } => last = reason,
                held::Fact::Dead { reason } | held::Fact::Undeclared { reason } => {
                    return Err(reason)
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Err(format!("the queue of `{thread}` has not arrived: {last}"))
    }

    /// **THE RESOURCE DOOR, AND IT IS A READ THAT IS NOT A QUESTION.**
    ///
    /// `resource/{id}/read` takes no filter, no cursor and no range: the shape
    /// said WHERE the bytes are and this carries them, so the id IS the whole of
    /// the ask and the export that carried the row minted it. Used only for a
    /// row the producer elided (`Payload::Split` — a message body over 1 KiB).
    pub async fn resource(&self, id: &str) -> Result<Vec<u8>, String> {
        let payload = self
            .seam
            .link
            .request(seam::inventory::methods::RESOURCE_READ.at(id), json!({}))
            .await
            .map_err(refusal)?;
        shapewire::mirror::inline_bytes(&payload).map_err(|e| e.message)
    }
}

/// One request id per cancel, unique within the process (the diag line
/// already carries the pid), joining a `SEND` line to its `REPLY`.
fn cancel_rid() -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!("cancel-{}", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1)
}

fn refusal(r: shapewire::envelope::Refusal) -> String {
    format!("{}: {}", r.code, r.message)
}

// ──────────────────────────────────────────────────────────────────────────
//  Starting it — D1
// ──────────────────────────────────────────────────────────────────────────

/// **START THE ONE SOCKET AND THE ONE FORWARDER.** A drop-in for
/// `transport::spawn`: the same pair, so `app::run`'s loop, `store.rs` and
/// every `TransportEvent` arm are unchanged.
///
/// Supervision is [`EgressGate`] (refuses, starts nothing) — see this module's header, D1.
pub fn spawn() -> (Handle, mpsc::Receiver<TransportEvent>) {
    spawn_with(EgressGate::default())
}

/// [`spawn`] with its gate given: production's is `EgressGate::default()`.
fn spawn_with(gate: EgressGate) -> (Handle, mpsc::Receiver<TransportEvent>) {
    // **THE LAST DOOR** (REL-6a2): every threads connection in this crate is
    // made here. An isolated process (a test, a harness, a drive, a dump —
    // `cauldron::live::isolation`) never opens one to his live ports, whatever
    // pointed it there or left it unpointed (unset is 3653). Loud, because a
    // caller that reached this is a defect to fix, not a state to draw.
    let port = link::chassis_port();
    if let Some(why) = crate::cauldron::live::isolation(false) {
        assert!(
            !crate::cauldron::live::PROD_PORTS.contains(&port),
            "refused: a threads connection to port {port}, a live daemon of his, from an isolated process ({why})"
        );
    }
    let seam = Arc::new(Seam::start(Some(Arc::new(gate))));
    let (tx, rx) = mpsc::channel(256);
    let bodies = Bodies::default();
    tokio::spawn(forward(seam.clone(), tx, bodies.clone()));
    (Handle { seam, bodies }, rx)
}

/// **THE DIAL-TIME GATE** (REL-6a5; was REL-6a4 item 1). The seam calls
/// [`link::Supervisor::before_connect`] before EVERY connection attempt,
/// reconnects included, and re-reads `ws_url()` for each — so a port that
/// changes mid-life is re-checked here, at the attempt that would dial it,
/// through the one choke point (`env::egress::admit_url` via
/// `cauldron::live::refuse_isolated_url`). An `Err` is drawn as
/// `LinkState::Down` and retried on the seam's ladder; nothing is dialled.
/// It starts nothing, so DIR-7 (the face never spawns the daemon) holds.
///
/// Residual, by construction: the seam reads `ws_url()` once here and once
/// for `connect_async`, on the same task a few instructions apart; a process
/// that rewrote `TUI_THREADS_CHASSIS_PORT` from another thread inside that
/// window would dial the new value unchecked. Closing it means a fenced edit
/// of the vendored `link.rs` (seamdiff), which this step did not make.
#[derive(Debug, Clone, Copy, Default)]
pub struct EgressGate {
    /// TEST ONLY ([`spawn_gated_at`]): one more port to refuse, so the
    /// reconnect re-check is proven against a SCRATCH listener standing in
    /// for a live port — no test ever lets a real one be dialled. `None` in
    /// production.
    also_refuse: Option<u16>,
}

impl link::Supervisor for EgressGate {
    fn before_connect(&self) -> link::BoxFuture<'_, Result<(), String>> {
        let url = link::ws_url();
        let r = match (self.also_refuse, crate::cauldron::live::url_port(&url)) {
            (Some(p), Some(q)) if p == q => Err(format!("refused: a threads chassis connection to port {p}, which this test declared live")),
            _ => gate(&url),
        };
        Box::pin(async move { r })
    }
}

/// [`EgressGate`]'s decision for one attempt's url.
pub fn gate(url: &str) -> Result<(), String> {
    match crate::cauldron::live::refuse_isolated_url("threads chassis", url) {
        Some(refusal) => Err(refusal),
        None => Ok(()),
    }
}

/// **A TEST SEAM POINTED AT `chassis`, WHOSE GATE ALSO REFUSES
/// `also_refuse`** — a scratch port the test treats as live (REL-6a5's
/// reconnect table). Hold the guard; re-point with [`TestPorts::repoint`].
pub fn spawn_gated_at(chassis: u16, also_refuse: u16) -> (Handle, mpsc::Receiver<TransportEvent>, TestPorts) {
    let guard = TEST_PORTS.lock().unwrap_or_else(|e| e.into_inner());
    point(chassis);
    let (h, rx) = spawn_with(EgressGate { also_refuse: Some(also_refuse) });
    (h, rx, TestPorts(guard))
}

impl TestPorts {
    /// Re-point the lever while this guard is held — the mid-life change the
    /// seam's next attempt re-reads.
    pub fn repoint(&self, chassis: u16) {
        point(chassis);
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  Starting it IN A TEST — and why this is not just `spawn` with a comment
// ──────────────────────────────────────────────────────────────────────────

/// **THE DEFAULT CHASSIS PORT IS 3653 AND 3653 IS THE OPERATOR'S LIVE DAEMON.**
///
/// That is the whole reason this type exists. [`spawn`] reads its port out of
/// the environment through the seam, with `DEFAULT_CHASSIS_PORT` as the
/// fallback, so a test process that calls [`spawn`] with nothing set opens a
/// socket to PRODUCTION — and env vars are per-PROCESS while Rust tests are
/// per-THREAD, so two tests pointing the same process at two different fakes
/// race, and the loser dials whatever the winner left behind or the default.
///
/// An agent has already paid this bill in this repo: it set the AUI's
/// `_PORT` to point a scratch run somewhere safe, forgot `_CHASSIS_PORT`, and
/// its shape socket went to the live daemon, where it created a real thread and
/// spent real money.
///
/// So a test does not get to call [`spawn`]. It calls [`spawn_at`], which:
///
/// * takes a PROCESS-WIDE lock, so only one seam in this binary is pointed
///   anywhere at a time and the race cannot happen;
/// * sets BOTH halves of the lever, because setting one is the mistake;
/// * and hands back a guard the test must hold, so the window in which the
///   ports are pointed is the lifetime of a value a reader can see.
///
/// `no_test_starts_a_seam_without_pointing_it` is the guard that makes this
/// more than a convention.
pub struct TestPorts(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

/// THE ONE LOCK over the process-wide port lever (`TUI_THREADS_CHASSIS_PORT`
/// and `TUI_THREADS_PORT`). Every test that writes or reads the lever takes it.
pub(crate) static TEST_PORTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// **START A SEAM POINTED AT `chassis`, AND NOWHERE ELSE.** See [`TestPorts`].
///
/// Hold the returned guard for as long as the seam is alive. `1` is the
/// conventional port for *nothing is listening*, which is how a test asks for
/// the state this face has to draw when the daemon is away.
pub fn spawn_at(chassis: u16) -> (Handle, mpsc::Receiver<TransportEvent>, TestPorts) {
    let guard = TEST_PORTS.lock().unwrap_or_else(|e| e.into_inner());
    point(chassis);
    let (h, rx) = spawn();
    (h, rx, TestPorts(guard))
}

/// **A SEAM WITH NOTHING BEHIND IT, AND NO GUARD TO HOLD.**
///
/// Port 1 is the conventional *nothing is listening*, so the link is `Down` and
/// every command is refused exactly as a real one is when the daemon is away.
/// That is the state this face has to draw correctly anyway, which is why so
/// many tests want it.
///
/// **It needs no [`TestPorts`] and that is an argument, not a shortcut.** The
/// guard exists to stop two tests in one process pointing the seam at two
/// different fakes; this function only ever writes the SAME two values, so two
/// concurrent callers agree and there is nothing to serialise. It still takes
/// the lock for the two writes, so it cannot interleave with a [`spawn_at`]
/// halfway through pointing somewhere else.
///
/// What it does NOT do is leave the environment unset — which is the whole
/// point, because unset means `DEFAULT_CHASSIS_PORT`, which is the operator's
/// live daemon. A test helper that returns a bare `App` can call this and have
/// nothing to keep alive.
pub fn spawn_dead() -> (Handle, mpsc::Receiver<TransportEvent>) {
    // The lock is held across `spawn()` as well (TUI-P1a, inspect F-1), so the
    // lever cannot be re-pointed between the write and the seam's first read.
    let _g = TEST_PORTS.lock().unwrap_or_else(|e| e.into_inner());
    point(1);
    spawn()
}

/// **THE LEVER HELD UNSET** (REL-6a): the lock, with BOTH halves of the
/// lever removed, for as long as the guard lives. For a test proving what a
/// face does when nothing points it — which must be NOTHING: the face is
/// refused before it resolves or dials anything. Nothing in this binary can
/// point the lever while it is held.
pub fn ports_unset() -> TestPorts {
    let guard = TEST_PORTS.lock().unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("TUI_THREADS_CHASSIS_PORT");
    std::env::remove_var("TUI_THREADS_PORT");
    TestPorts(guard)
}

/// BOTH halves of the lever, always together. `_PORT` is the legacy `/health`
/// door this face never probes, and leaving it pointed at the real daemon while
/// the chassis port moves is the exact confusion [`check_ports`] refuses in
/// production.
fn point(chassis: u16) {
    std::env::set_var("TUI_THREADS_CHASSIS_PORT", chassis.to_string());
    std::env::set_var("TUI_THREADS_PORT", chassis.to_string());
}

#[cfg(test)]
mod spawn_guard {
    /// **NO TEST IN THIS CRATE STARTS A SEAM IT HAS NOT POINTED SOMEWHERE.**
    ///
    /// `mirror::spawn()` reads its port out of the environment and falls back to
    /// `link::DEFAULT_CHASSIS_PORT`, which is 3653, which is the operator's LIVE
    /// daemon. A test that calls it with nothing set opens a socket to
    /// production. This is the guard that says so, and it is a source scan
    /// because there is no type-level way to say *not in a test* from a library
    /// that integration tests link as an ordinary dependency.
    ///
    /// **THE SENTINEL IS FIRST.** A scan that matched nothing would pass on an
    /// empty set, which is the shape of about twenty-five probes this arc has
    /// already found. So it must first FIND the pointed spawns it expects before
    /// it is believed about the bare ones it does not.
    #[test]
    fn no_test_starts_a_seam_without_pointing_it() {
        // Every file in this crate that starts a seam in a test, with the
        // in-crate test blocks and the integration tests side by side. A new
        // one added without a line here is caught by the last assertion, which
        // counts what the whole set found.
        let files: &[(&str, &str)] = &[
            ("src/app.rs", include_str!("app.rs")),
            ("src/mirror.rs", include_str!("mirror.rs")),
            ("tests/it/compose.rs", include_str!("../tests/it/compose.rs")),
            ("tests/mic.rs", include_str!("../tests/mic.rs")),
            (
                "tests/it/rows_registry_and_rename.rs",
                include_str!("../tests/it/rows_registry_and_rename.rs"),
            ),
            (
                "tests/it/lib_surface.rs",
                include_str!("../tests/it/lib_surface.rs"),
            ),
        ];
        // **THE ONE FILE THIS SCAN MUST SKIP, AND IT IS EXACTLY ONE.** This
        // file's own test half carries the banned needle as a string LITERAL —
        // it is the ban — so a scan that read it would fail on the existence of
        // the check rather than on anything shipped. That is the
        // self-witnessing case, and `seamdiff`'s engine has the identical
        // problem and the identical answer written down as `SELF_EXCLUDED`. The
        // exclusion is proved NON-VACUOUS below rather than asserted: an
        // excluded path that needs no excluding has stopped being a
        // measurement.
        const SELF_EXCLUDED: &str = "src/mirror.rs";
        let me = files
            .iter()
            .find(|(n, _)| *n == SELF_EXCLUDED)
            .expect("the excluded file is in the list");
        assert!(
            me.1.split_once("#[cfg(test)]")
                .is_some_and(|(_, t)| t.contains("mirror::spawn()")),
            "`{SELF_EXCLUDED}` is excluded from this scan and contains no \
             `mirror::spawn()` needle, so the exclusion protects nothing"
        );

        let mut pointed = 0usize;
        for (name, whole) in files {
            if *name == SELF_EXCLUDED {
                continue;
            }
            // Only the TEST half of a file in `src/`; `spawn()` itself lives in
            // the production half of this very file and is not the subject.
            let body = if name.starts_with("src/") {
                whole.split_once("#[cfg(test)]").map_or("", |(_, t)| t)
            } else {
                whole
            };
            let code: String = body
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            pointed += code.matches("spawn_at(").count() + code.matches("spawn_dead(").count();
            // `spawn()` with nothing in the parentheses — the bare call. A
            // `spawn_at(` or `spawn_dead(` cannot match this needle.
            for bare in ["mirror::spawn()", "mirror::spawn ()"] {
                assert!(
                    !code.contains(bare),
                    "`{name}` calls `{bare}` in a test. With no port set that \
                     dials {}, which is the operator's live daemon — use \
                     `mirror::spawn_at(port)` or `mirror::spawn_dead()`",
                    super::link::DEFAULT_CHASSIS_PORT
                );
            }
        }
        assert!(
            pointed >= 4,
            "the scan found only {pointed} pointed spawn(s) across {} files, so \
             it is looking at the wrong text and the bans above prove nothing",
            files.len()
        );
    }

    /// **THE CAULDRON FACE'S DOOR IS SCANNED TOO, PRODUCTION HALF INCLUDED**
    /// (REL-6a, REL-6-I F1). `cauldron/live.rs` hands `mirror::spawn` over as
    /// a function VALUE (`connect(mirror::spawn)`), which the needle above —
    /// a call with empty parentheses — can never match, and its unpointed
    /// door is production code, which the scan above skips by design. So the
    /// rule for that file is structural: every unpointed reference to
    /// `mirror::spawn` sits inside `fn start`, that function takes an
    /// already-resolved `Endpoint` (the only place an isolated face is refused
    /// his live ports), and it is not `pub` — no integration test can call it.
    #[test]
    fn the_cauldron_face_dials_only_through_a_resolved_endpoint() {
        let live = include_str!("cauldron/live.rs");
        let code: String = live
            .lines()
            .map(|l| if l.trim_start().starts_with("//") { "" } else { l })
            .collect::<Vec<_>>()
            .join("\n");
        // SENTINEL: the pointed door is found, so the scan reads the right text.
        assert!(code.contains("mirror::spawn_at("), "live.rs has no `mirror::spawn_at(` — the scan is reading the wrong text");
        // Every reference to `mirror::spawn` that is not `spawn_at`/`spawn_dead`.
        let needle = "mirror::spawn";
        let bare: Vec<usize> = code
            .match_indices(needle)
            .map(|(i, _)| i)
            .filter(|&i| !code[i + needle.len()..].starts_with(|c: char| c == '_' || c.is_alphanumeric()))
            .collect();
        assert!(!bare.is_empty(), "live.rs no longer references `mirror::spawn` unpointed; re-decide this guard");
        // The one function allowed to hold it, and its signature.
        let start = code.find("fn start(").expect("live.rs has a `fn start(`");
        let sig_end = start + code[start..].find('{').expect("`fn start` has a body");
        let sig = &code[start..sig_end];
        assert!(sig.contains("&Endpoint"), "`fn start` must take a resolved `&Endpoint`, got `{sig}`");
        let vis = code[..start].rsplit('\n').next().unwrap_or("");
        assert!(
            vis.trim() == "pub(crate)",
            "`fn start` in live.rs is reachable from integration tests (`{}`): it must be `pub(crate)`",
            vis.trim()
        );
        let body_end = sig_end + code[sig_end..].find("\n    }\n").expect("`fn start` closes");
        for i in bare {
            let line = code[..i].matches('\n').count() + 1;
            assert!(
                (sig_end..body_end).contains(&i),
                "live.rs references `mirror::spawn` unpointed at line {line}, outside `fn start(&Endpoint)`: \
                 that is a door to the environment's daemon (3653 for prod) that no isolation check guards"
            );
        }
    }
}

/// **THE LEVER'S TWO HALVES, AND WHY THIS REFUSES RATHER THAN GUESSES.**
///
/// The seam reads two ports, both under this face's own `TUI_THREADS` prefix:
/// `TUI_THREADS_CHASSIS_PORT` (3653) is the shape socket and the only door this
/// face opens; `TUI_THREADS_PORT` (3651) is the LEGACY door, kept for `/health`,
/// which this face does not probe at all.
///
/// So setting `TUI_THREADS_PORT` alone moves nothing, and that is a trap with a
/// bill attached: an agent pointed a face at a scratch daemon by setting the
/// AUI's `_PORT`, forgot `_CHASSIS_PORT`, and its shape socket went to the
/// OPERATOR'S LIVE DAEMON — where it created a real thread and spent real money.
/// A default that silently means *production* is a default worth refusing over.
///
/// Called from `main.rs` before the loop starts. It never guesses the other
/// half: deriving a chassis port from a health port is exactly the reconciliation
/// that makes two values look like one.
pub fn check_ports() -> Result<(), String> {
    let set = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    match (set("TUI_THREADS_PORT"), set("TUI_THREADS_CHASSIS_PORT")) {
        (Some(health), None) => Err(format!(
            "TUI_THREADS_PORT is set to {health} and TUI_THREADS_CHASSIS_PORT is not.\n\
             \n\
             TUI_THREADS_PORT is the LEGACY /health door, which this face never\n\
             probes. Every frame this face reads goes over the chassis socket,\n\
             which is still {} — the operator's live daemon.\n\
             \n\
             Set TUI_THREADS_CHASSIS_PORT too, or unset TUI_THREADS_PORT. This is\n\
             refused rather than guessed: an agent made exactly this mistake and\n\
             its shape socket reached production.",
            link::DEFAULT_CHASSIS_PORT
        )),
        _ => Ok(()),
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  D6 — the event adapter, and the whole of the projection
// ──────────────────────────────────────────────────────────────────────────

/// **THE SEAM SAYS WHAT ARRIVED; THIS DECIDES WHAT THE FACE IS TOLD.**
///
/// One task, one `broadcast` receiver, and a [`Projection`] that remembers what
/// it last said so a wake-up with no news costs nothing. The seam deliberately
/// carries no state on its events — every subscriber reads the mirror when it
/// wakes — which is why a `Lagged` here needs no handler: a missed wake-up is a
/// missed wake-up, and the next one reads the same mirror.
async fn forward(seam: Arc<Seam>, tx: mpsc::Sender<TransportEvent>, bodies: Bodies) {
    let mut rx = seam.events();
    let mut p = Projection::default();
    let tried: Tried = Tried::default();
    loop {
        let e = match rx.recv().await {
            Ok(e) => Some(e),
            // Not a hole. A wake-up carries no fact, so the only cost of a
            // dropped one is latency, and the re-projection below repairs it.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::debug!(missed = n, "mirror: wake-ups coalesced");
                None
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        };
        // **HOLD BEFORE PROJECTING.** A key the window named for the first
        // time is subscribed here and projected on the next wake-up, which is
        // the right order: `Holding::NotAsked` is not a fact about a thread and
        // must never reach the screen as one.
        hold_window(&seam).await;
        let fetch = |id: String| {
            let seam = seam.clone();
            async move { fetch_resource(&seam, &id).await }
        };
        for ev in deliver(&mut p, &seam, e, Utc::now(), &bodies, fetch).await {
            if tx.send(ev).await.is_err() {
                return;
            }
        }
        // **WHAT AN UNNAMED THREAD IS CALLED** (TUI-P1a): the first line of
        // its first message, read once off its first message page. Off the
        // loop, so a slow page never holds up a frame.
        for (thread, page) in seam.read(|h| first_pages_owed(h, &tried)) {
            tried.lock().expect("first-line book").insert(page.clone());
            let (seam, tx) = (seam.clone(), tx.clone());
            tokio::spawn(async move {
                match fetch_resource(&seam, &page).await {
                    Ok(bytes) => {
                        if let Some(line) = first_line_from_page(&bytes) {
                            let _ = tx.send(TransportEvent::FirstLine { thread_id: thread, line }).await;
                        }
                    }
                    Err(why) => crate::diag!("body", "FIRST-LINE page={page} failed: {why}"),
                }
            });
        }
    }
}

/// The first-message pages already asked for, by page id. A page id changes
/// while the page is still filling (`m0-3` becomes `m0-4`), so an unnamed
/// thread whose first page held no message of his yet is asked again when it
/// grows — and never otherwise.
type Tried = Arc<std::sync::Mutex<std::collections::HashSet<String>>>;

/// **THE FIRST PAGES OWED A READ** — `(thread, page id)` for every thread the
/// window holds that has NO name (no `summary_updated`, no summary title) and
/// whose first message page has not been asked for. A named thread is never
/// read: its name wins, so its first line could never be drawn.
fn first_pages_owed(h: &held::Held, tried: &Tried) -> Vec<(String, String)> {
    let tried = tried.lock().expect("first-line book");
    window(h)
        .into_iter()
        .filter(|id| {
            let named = view::app_payload(h, id, SUMMARY_EVENT_TYPE)
                .known()
                .is_some_and(|p| p.get("summary").and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty()));
            let titled = matches!(view::title(h, id).known(), Some(Some(t)) if !t.trim().is_empty());
            !named && !titled
        })
        .filter_map(|id| {
            let first = view::message_pages(h, &id).known()?.into_iter().min_by_key(|p| p.from)?;
            (!tried.contains(&first.id)).then_some((id, first.id))
        })
        .collect()
}

/// **THE FIRST LINE OF THE FIRST MESSAGE HE (or whoever started it) WROTE**,
/// out of a message page's bytes — `{thread_id, from, to, messages: [...]}`,
/// each message spelled as `threads/{id}/query` spelled one (`kind`, `text`).
/// The first `user_message` with any text; its first non-blank line, bounded.
pub fn first_line_from_page(bytes: &[u8]) -> Option<String> {
    let page: Value = serde_json::from_slice(bytes).ok()?;
    page.get("messages")?
        .as_array()?
        .iter()
        .filter(|m| m.get("kind").and_then(Value::as_str) == Some("user_message"))
        .find_map(|m| m.get("text").and_then(Value::as_str).and_then(crate::store::first_line_of))
}

// ──────────────────────────────────────────────────────────────────────────
//  Clone-free reads of an open window (THREAD-LOAD-LATENCY F5)
// ──────────────────────────────────────────────────────────────────────────
//
// `view::log_rows` answers with an owned `Vec<Value>`: every call deep-copies
// the whole window (up to 4,000 rows). Every wake-up read every open window
// that way, twice, to find the zero new rows on an ordinary wake — 33 ms a
// wake with eight windows open, and most of his live face's CPU while nothing
// on screen changed. These read the SAME cell, under the same conditions
// `view::items` answers `Known` for, without copying it. They live in the face
// and not in `seam/`, which is a checked copy of the shapewire reference
// (`tests/it/seam_reference.rs`): a face that needs a different answer reads a
// different function.

/// `f` handed one thread's `threads/log` window — the exported items and the
/// transitions appended since — in place. `None` unless the cell is `Known`
/// (not hydrated, dead, refused or undeclared all answer `None`).
pub fn with_log_rows<T>(h: &held::Held, id: &str, f: impl FnOnce(&[Value], &[Value]) -> T) -> Option<T> {
    let c = seam::inventory::threads_log::EVENTS;
    let (shape, name) = (c.shape(), c.name());
    let m = h.mirror(shape)?;
    if m.refusal().is_some() {
        return None;
    }
    if matches!(m.declared(name), shapewire::mirror::Declaration::Undeclared { .. }) {
        return None;
    }
    if !matches!(m.cell(id, name), shapewire::wire::CellState::Known { .. }) {
        return None;
    }
    let empty: &[Value] = &[];
    match m.held(id) {
        Some(row) => {
            let exported = match row.field(name) {
                Value::Array(a) => a.as_slice(),
                _ => empty,
            };
            Some(f(exported, row.transitions(name)))
        }
        None => Some(f(empty, empty)),
    }
}

/// The number of rows in a Known window, for the flight recorder.
fn window_len(h: &held::Held, id: &str) -> Option<usize> {
    with_log_rows(h, id, |a, b| a.len() + b.len())
}

/// `ledger::row_of`'s reading of one item, for the rows this face builds
/// itself. Same rules: no `seq`, no `payload` or no `kind` is not a row.
fn ledger_row(item: &Value) -> Option<ledger::Row> {
    let seq = item.get("seq").and_then(Value::as_u64)?;
    let payload = item.get("payload")?;
    let (event, elided) = if let Some(v) = payload.get("here") {
        (v.clone(), false)
    } else if let Some(s) = payload.get("split") {
        (s.get("event").cloned().unwrap_or(Value::Null), true)
    } else {
        (Value::Null, true)
    };
    Some(ledger::Row {
        seq,
        message_id: item.get("message_id").and_then(Value::as_u64),
        kind: item.get("kind").and_then(Value::as_str)?.to_string(),
        ts: item.get("ts").and_then(Value::as_str).map(str::to_string),
        event,
        elided,
    })
}

/// **THE ROWS OF `thread`'s WINDOW NOT YET CLAIMED**, each claimed as it is
/// returned: what `ledger::rows_after(h, thread, 0)` filtered through
/// `ledger::claim` returned, without copying the window. The window is in
/// `seq` order (exported oldest-first, transitions appended), so the unclaimed
/// rows are a SUFFIX: the walk goes back from the newest row and stops at the
/// first at or below the claim mark, and an empty wake reads one row. Only the
/// rows that win their claim are built. A row with no `seq` (or `seq` 0, which
/// the producer never stamps) is skipped rather than handed over on every wake.
fn rows_unclaimed(h: &mut held::Held, thread: &str) -> Vec<ledger::Row> {
    let readable = |item: &Value| -> Option<u64> {
        item.get("payload")?;
        item.get("kind").and_then(Value::as_str)?;
        item.get("seq").and_then(Value::as_u64)
    };
    let mark = ledger::emitted(h)
        .get(thread)
        .copied()
        .unwrap_or_else(|| ledger::floor(h, thread).unwrap_or(0));
    let seqs: Vec<u64> = with_log_rows(h, thread, |a, b| {
        let mut out = Vec::new();
        for item in a.iter().chain(b).rev() {
            match item.get("seq").and_then(Value::as_u64) {
                Some(seq) if seq > mark => {
                    if readable(item).is_some() {
                        out.push(seq);
                    }
                }
                Some(_) => break,
                None => {}
            }
        }
        out.reverse();
        out
    })
    .unwrap_or_default();
    let won: std::collections::HashSet<u64> =
        seqs.into_iter().filter(|seq| ledger::claim(h, thread, *seq)).collect();
    if won.is_empty() {
        return Vec::new();
    }
    let lowest = won.iter().copied().min().unwrap_or(0);
    with_log_rows(h, thread, |a, b| {
        let mut tail: Vec<&Value> = a
            .iter()
            .chain(b)
            .rev()
            .take_while(|item| item.get("seq").and_then(Value::as_u64).map_or(true, |s| s >= lowest))
            .collect();
        tail.reverse();
        tail.into_iter()
            .filter(|item| readable(item).is_some_and(|seq| won.contains(&seq)))
            .filter_map(ledger_row)
            .collect()
    })
    .unwrap_or_default()
}

/// **THE BODIES THE PRODUCER PUT BEHIND THE RESOURCE DOOR, ONCE FETCHED.**
/// Keyed `(thread, seq)`; the value is the WHOLE event, completed.
#[derive(Clone, Default)]
pub struct Bodies(Arc<std::sync::Mutex<std::collections::HashMap<(String, u64), Value>>>);

/// `resource/{id}/read`, through the seam's own link — the one door
/// [`Handle::resource`] also uses.
async fn fetch_resource(seam: &Seam, id: &str) -> Result<Vec<u8>, String> {
    let payload = seam
        .link
        .request(seam::inventory::methods::RESOURCE_READ.at(id), json!({}))
        .await
        .map_err(refusal)?;
    shapewire::mirror::inline_bytes(&payload).map_err(|e| e.message)
}

// ──────────────────────────────────────────────────────────────────────────
//  Elided bodies — the empty echo and the vanishing reply
// ──────────────────────────────────────────────────────────────────────────

/// **WHERE AN ELIDED ROW'S BYTES ARE.** The producer puts a row's bytes
/// behind the resource door when the event serialises over 1 KiB
/// (`threads/log`'s `MAX_INLINE_PAYLOAD`): a MESSAGE as `split` — the event
/// with its `text` key REMOVED, plus the id of the text — and anything else as
/// `there`, the id of the whole event and nothing more.
///
/// **This face used to render the row as it arrived.** A `split`
/// `user_message` became a user box with no words (the EMPTY ECHO: every
/// message he sends over 1 KiB), and a `split` `assistant_text` — which
/// retires the box the reply's deltas were assembling live — replaced a box
/// full of words with an empty one (the VANISHING REPLY: on screen while it
/// streamed, gone the moment it finished). [`Handle::resource`] existed and
/// nothing called it.
#[derive(Debug, Clone)]
pub struct Elided {
    pub id: String,
    pub kind: String,
    /// `Some` for a `split`: the event as it arrived, without its `text`.
    pub skeleton: Option<Value>,
}

/// Every elided row the face's own mirror holds for `thread`, by seq.
pub fn elisions(h: &held::Held, thread: &str) -> std::collections::HashMap<u64, Elided> {
    let mut out = std::collections::HashMap::new();
    // Borrowed, not copied (F5): only the elided rows are cloned out.
    let items: Vec<Value> = with_log_rows(h, thread, |a, b| {
        a.iter()
            .chain(b)
            .filter(|item| {
                item.get("payload")
                    .is_some_and(|p| p.get("split").is_some() || p.get("there").is_some())
            })
            .cloned()
            .collect()
    })
    .unwrap_or_default();
    for item in items {
        let (Some(seq), Some(payload)) = (item.get("seq").and_then(Value::as_u64), item.get("payload"))
        else {
            continue;
        };
        let kind = item.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
        let (id, skeleton) = if let Some(sp) = payload.get("split") {
            (sp.pointer("/text/id"), Some(sp.get("event").cloned().unwrap_or(json!({}))))
        } else if let Some(t) = payload.get("there") {
            (t.get("id"), None)
        } else {
            continue;
        };
        if let Some(id) = id.and_then(Value::as_str) {
            out.insert(seq, Elided { id: id.to_string(), kind, skeleton });
        }
    }
    out
}

/// **THE WHOLE EVENT, from the bytes the door served for it.** A `split`
/// gets its text put back under `text`; a `there` IS the event's JSON.
pub fn complete(e: &Elided, bytes: &[u8]) -> Result<Value, String> {
    let mut ev = match &e.skeleton {
        Some(sk) => {
            let text = String::from_utf8(bytes.to_vec())
                .map_err(|err| format!("the body is not UTF-8: {err}"))?;
            let mut ev = if sk.is_object() { sk.clone() } else { json!({}) };
            ev["text"] = json!(text);
            ev
        }
        None => serde_json::from_slice::<Value>(bytes)
            .map_err(|err| format!("the event is not JSON: {err}"))?,
    };
    if ev.is_object() && ev.get("kind").is_none() {
        ev["kind"] = json!(e.kind);
    }
    Ok(ev)
}

/// **WHAT A ROW SAYS WHEN ITS BODY COULD NOT BE FETCHED** — never an empty
/// box. The kind survives so the row lands in the right arm, and a message
/// says in its own text that its words are missing and why.
fn unavailable(e: &Elided, why: &str) -> Value {
    let mut ev = match &e.skeleton {
        Some(sk) if sk.is_object() => sk.clone(),
        _ => json!({}),
    };
    ev["kind"] = json!(e.kind);
    if matches!(e.kind.as_str(), "user_message" | "assistant_text" | "thinking") {
        ev["text"] = json!(format!(
            "[the daemon holds this message's text behind resource {}, and fetching it failed: {why}]",
            e.id
        ));
    }
    ev
}

/// One elided row, completed: from the cache, else through `fetch`.
async fn resolve<F, Fut>(thread: &str, seq: u64, e: &Elided, bodies: &Bodies, fetch: &F) -> Value
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    let key = (thread.to_string(), seq);
    if let Some(v) = bodies.0.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return v.clone();
    }
    match fetch(e.id.clone()).await.and_then(|b| complete(e, &b)) {
        Ok(v) => {
            crate::diag!(
                "body",
                "FETCHED thread={thread} seq={seq} kind={} resource={} text_len={}",
                e.kind,
                e.id,
                v.get("text").and_then(Value::as_str).map_or("-".into(), |t| t.len().to_string())
            );
            bodies
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, v.clone());
            v
        }
        Err(why) => {
            crate::diag!(
                "body",
                "FAILED thread={thread} seq={seq} kind={} resource={} why={why}",
                e.kind,
                e.id
            );
            tracing::warn!(%thread, seq, resource = %e.id, %why, "an elided body could not be fetched");
            unavailable(e, &why)
        }
    }
}

/// How many elided bodies one face asks the resource door for at once
/// (THREAD-LOAD-LATENCY F4). They used to be asked for one at a time, each
/// awaiting its full round trip before the next was sent, so a window with
/// 400 elided rows cost 400 serial round trips before the transcript could be
/// drawn. Bounded so a huge window cannot flood the link's request table.
pub const BODY_FETCH_CONCURRENCY: usize = 16;

/// Every `(seq, elided)` completed, at most [`BODY_FETCH_CONCURRENCY`] in
/// flight, through the same cache and door as [`resolve`].
async fn resolve_many<F, Fut>(
    thread: &str,
    wanted: Vec<(u64, Elided)>,
    bodies: &Bodies,
    fetch: &F,
) -> std::collections::HashMap<u64, Value>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    use futures_util::StreamExt;
    futures_util::stream::iter(wanted)
        .map(|(seq, el)| async move { (seq, resolve(thread, seq, &el, bodies, fetch).await) })
        .buffer_unordered(BODY_FETCH_CONCURRENCY)
        .collect()
        .await
}

/// **ONE WAKE-UP, AS THE EVENTS THE STORE IS HANDED.** What [`forward`]
/// sends, as a function a test can drive with a fake resource door: the
/// projection's events, with every elided ledger row COMPLETED before the
/// store sees it — so no row is ever drawn without the words the daemon
/// stored for it.
pub async fn deliver<F, Fut>(
    p: &mut Projection,
    seam: &Seam,
    ev: Option<seam::SeamEvent>,
    now: DateTime<Utc>,
    bodies: &Bodies,
    fetch: F,
) -> Vec<TransportEvent>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    let mut out = p.step(seam, ev, now);
    let mut by_thread: BTreeMap<String, std::collections::HashMap<u64, Elided>> = BTreeMap::new();
    // Which rows of this wake-up are elided, per thread — then every body
    // fetched CONCURRENTLY (F4), and only then written back in frame order.
    let mut wanted: BTreeMap<String, Vec<(u64, Elided)>> = BTreeMap::new();
    for e in out.iter() {
        let TransportEvent::Frame {
            msg: ServerMsg::Event { thread_id, seq, .. },
            ..
        } = e
        else {
            continue;
        };
        let map = by_thread
            .entry(thread_id.clone())
            .or_insert_with(|| seam.read(|h| elisions(h, thread_id)));
        if let Some(el) = map.get(seq) {
            wanted.entry(thread_id.clone()).or_default().push((*seq, el.clone()));
        }
    }
    let mut done: BTreeMap<String, std::collections::HashMap<u64, Value>> = BTreeMap::new();
    for (thread, items) in wanted {
        let got = resolve_many(&thread, items, bodies, &fetch).await;
        done.insert(thread, got);
    }
    for e in out.iter_mut() {
        let TransportEvent::Frame {
            msg: ServerMsg::Event { thread_id, seq, event, .. },
            ..
        } = e
        else {
            continue;
        };
        if let Some(v) = done.get_mut(thread_id.as_str()).and_then(|m| m.remove(seq)) {
            *event = ThreadEvent::parse(v);
        }
    }
    out
}

/// **WHAT THE FACE HAS ALREADY BEEN TOLD**, so a wake-up with no news costs
/// nothing and a fact is never restated as if it had just happened.
///
/// It is a plain struct with no socket in it, which is the point: [`Projection::step`]
/// is a pure function of a `Held` and this state, so every test below drives it
/// by FOLDING FRAMES through the real `Held::fold` and reads what the face would
/// have been told. There is no transport in any of them.
#[derive(Default)]
pub struct Projection {
    /// The last link state reported, as the word the face uses for it.
    link: Option<Link>,
    /// The `boot_id` of the last link that came UP. What the three-way resync
    /// label is read off, and the only thing left of the ladder.
    boot: Option<String>,
    /// Per-thread, the last status row emitted. Diffed so an unchanged fleet
    /// raises no activity.
    statuses: BTreeMap<String, Value>,
    headers: BTreeMap<String, Value>,
    capped: Option<bool>,
    /// Per narration channel, the newest `at` already told, plus the raw lines
    /// at exactly that stamp — because two lines may share a timestamp and
    /// `since` is strictly-greater.
    narrated: BTreeMap<String, (String, Vec<Value>)>,
    /// Per held `threads/log` key, the last state the flight recorder wrote
    /// down, so a transition is logged once and not on every wake-up.
    log_states: BTreeMap<String, String>,
}

/// **WHAT THE FACE'S OWN MIRROR SAYS ABOUT ONE `threads/log` KEY**, in words.
/// Read off local state only; it names the condition, never a cause.
pub fn log_state(h: &held::Held, id: &str) -> String {
    // The common case, answered without copying the window (F5).
    if let Some(n) = window_len(h, id) {
        return format!("known rows={n}");
    }
    match view::log_rows(h, id) {
        held::Fact::Known(rows) => format!("known rows={}", rows.len()),
        held::Fact::NotHydrated { reason } => format!("not-hydrated: {reason}"),
        held::Fact::Dead { reason } => format!("dead: {reason}"),
        held::Fact::Undeclared { reason } => format!("undeclared: {reason}"),
    }
}

/// The three words this face has for a link, which are the three
/// `TransportEvent` arms. Not [`held::LinkState`] itself: that carries a
/// `since` and an `attempt` that must not make a transition look new.
#[derive(PartialEq, Eq, Clone, Debug)]
enum Link {
    Connecting(u32),
    Up(String),
    Down(String),
}

/// The three narration channels, and the SAME three the firehose carried as
/// its own frame types. Named in one place because the note text for each is
/// byte-for-byte what `store.rs`'s own `Progress`/`Lifecycle`/`Info` arms
/// produced, and that equality is the evidence this is a migration and not a
/// redesign.
const CHANNELS: &[&str] = held::CHANNELS;

impl Projection {
    /// **ONE WAKE-UP, TURNED INTO EVERYTHING THE FACE IS OWED.**
    ///
    /// Order matters and is the old transport's: the link's own transition
    /// first, then `Invalidate` BEFORE any repair lands, then the fleet, then
    /// the open thread's rows. A face that learned a new row before it learned
    /// the link came back would paint the row into a header that still said
    /// *down*.
    pub fn step(
        &mut self,
        seam: &Seam,
        ev: Option<seam::SeamEvent>,
        now: DateTime<Utc>,
    ) -> Vec<TransportEvent> {
        let mut out = Vec::new();
        if crate::diag::on() {
            match &ev {
                Some(seam::SeamEvent::Moved { shape, key }) => crate::diag!(
                    "wake",
                    "moved shape={shape} key={}",
                    key.as_deref().unwrap_or("-")
                ),
                Some(other) => crate::diag!("wake", "{other:?}"),
                None => crate::diag!("wake", "coalesced"),
            }
            for id in seam.read(ledger::open) {
                let st = seam.read(|h| log_state(h, &id));
                // Compared on the CONDITION, not the row count, so a growing
                // window is one line and not one per wake-up.
                let coarse = |s: &str| s.split(" rows=").next().unwrap_or("").to_string();
                if self.log_states.get(&id).map(|s| coarse(s)) != Some(coarse(&st)) {
                    crate::diag!(
                        "shape",
                        "STATE shape={} key={id} {} -> {st}",
                        held::LOG,
                        self.log_states.get(&id).map_or("(new)", String::as_str)
                    );
                    self.log_states.insert(id, st);
                }
            }
        }
        // ── the link ────────────────────────────────────────────────────────
        let link = seam.read(|h| match view::link(h) {
            held::LinkState::Connecting { attempt } => Link::Connecting(*attempt),
            held::LinkState::Up { boot_id, .. } => Link::Up(boot_id.clone()),
            held::LinkState::Down { reason } => Link::Down(reason.clone()),
        });
        if self.link.as_ref() != Some(&link) {
            match &link {
                Link::Connecting(attempt) => {
                    out.push(TransportEvent::Connecting { attempt: *attempt })
                }
                Link::Down(reason) => out.push(TransportEvent::Disconnected {
                    reason: reason.clone(),
                    // **ZERO, AND IT IS HONEST.** The retry ladder is the
                    // seam's (`link.rs`, 500 ms doubling to 30 s) and it does
                    // not publish the next attempt's time. The store renders
                    // `retry_at` from this, so zero means *no countdown
                    // claimed* — which is the correct thing to show about a
                    // schedule this face cannot see. Inventing the number here
                    // would be a second copy of the ladder's arithmetic, which
                    // is how two retry clocks disagree on screen.
                    next_retry_in_ms: 0,
                }),
                Link::Up(boot_id) => {
                    // **EVERY CURRENT-STATE FACT IS SUSPECT UNTIL THE EXPORTS
                    // LAND**, and this is sent before any of them, exactly as
                    // the old reconnect did. What it no longer precedes is a
                    // REPAIR: there is no `threads/status` re-query and no
                    // `history` replay behind it, because rule 2.5 makes a
                    // re-subscribe's export replace the row rather than merge
                    // into it.
                    let resync = match &self.boot {
                        None => ResyncKind::First,
                        Some(b) if b == boot_id => ResyncKind::Blip,
                        Some(_) => ResyncKind::Restart,
                    };
                    out.push(TransportEvent::Invalidate {
                        reason: format!("the link came up · {}", resync.label()),
                    });
                    out.push(TransportEvent::Connected {
                        boot_id: boot_id.clone(),
                        resync,
                    });
                    self.boot = Some(boot_id.clone());
                }
            }
            self.link = Some(link);
        }

        // ── the narration channels: all three, every one a note ─────────────
        //
        // `progress`, `lifecycle` and `info` were FIREHOSE-ONLY by
        // construction — each carries an empty thread id, so no per-thread
        // filter could ever reach them — and this is where all three stop
        // needing one. The `narration` shape carries the identical fields under
        // the identical names, so the note text below is the text `store.rs`
        // built from the frames.
        for channel in CHANNELS {
            out.extend(self.narrate(seam, channel));
        }
        // A `lifecycle` wake-up is ALSO delivered as the seam's own event, and
        // it is deliberately NOT turned into a second note: the channel above
        // already carried it, and the daemon replays `back_up` to every
        // subscriber including a first-ever connection. The event is read for
        // one thing only — logging — because `boot_id` on the link is what
        // decides a restart here, and it did even under the firehose
        // (`transport.rs`: *"it explicitly cannot decide the resync kind"*).
        if let Some(seam::SeamEvent::Lifecycle {
            service,
            phase,
            boot_id,
            ..
        }) = &ev
        {
            tracing::debug!(%service, %phase, %boot_id, "mirror: a service moved");
        }

        // ── the fleet ───────────────────────────────────────────────────────
        out.extend(self.fleet(seam));
        // ── the open thread's new rows ──────────────────────────────────────
        out.extend(self.rows(seam, now));
        out
    }

    /// One narration channel's new lines, as notes. **The line's own fields,
    /// never `Line::text`** — see the comment inside.
    fn narrate(&mut self, seam: &Seam, channel: &str) -> Vec<TransportEvent> {
        let lines = seam.read(|h| view::narration(h, channel, None).known().unwrap_or_default());
        let (last_at, at_last) = self
            .narrated
            .get(channel)
            .cloned()
            .unwrap_or_else(|| (String::new(), Vec::new()));
        let mut newest = last_at.clone();
        let mut at_newest = at_last.clone();
        let mut out = Vec::new();
        for l in lines {
            let at = l
                .raw
                .get("at")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if at < last_at {
                continue;
            }
            if at == last_at && at_last.contains(&l.raw) {
                continue;
            }
            // **THERE IS NO `Line::text` ANY MORE, AND THAT IS THE REPAIR.**
            // The vendored `view.rs` used to build `Line::text` from
            // `raw["text"]` — a field the producer's `narration` line has never
            // declared, so it was the empty string on every line any producer
            // ever exported and a face that rendered it drew an empty activity
            // lane. `Line::kind` replaced it: the producer's own declared tag,
            // `Required`, so a line that cannot name its kind is now dropped by
            // `view::narration` instead of arriving with an empty one.
            //
            // This loop still reads `l.raw` rather than `l.kind`, and that is
            // deliberate rather than left over: `note_of` dispatches on the
            // CHANNEL it was asked for, and the channel and the kind are the
            // same word by construction (`narration` is keyed on the channel and
            // `lines.kind`'s vocabulary is the channel set). Reading `raw` keeps
            // every field under the producer's own name, which is why the text
            // each one builds is byte-identical to what `store.rs` built off the
            // legacy frame.
            let (text, level) = note_of(channel, &l.raw);
            if at > newest {
                newest = at.clone();
                at_newest.clear();
            }
            if at == newest {
                at_newest.push(l.raw.clone());
            }
            out.push(TransportEvent::Note { text, level });
        }
        // Bound it: only the lines sharing the NEWEST stamp are remembered, so
        // this cannot grow with the channel.
        self.narrated
            .insert(channel.to_string(), (newest, at_newest));
        out
    }
}

/// **THE NOTE A NARRATION LINE BECOMES, PER CHANNEL** — and each one is the
/// text `store.rs`'s own arm for the matching firehose frame produced, field for
/// field.
///
/// | channel | the text | the level |
/// |---|---|---|
/// | `progress` | `"{op} · {phase} · {detail}"` | `Critical` iff `done && ok == Some(false)`, else `Calm` |
/// | `lifecycle` | `"{service} {phase} ({reason})"` | `Warn` iff `phase == "going_down"`, else `Calm` |
/// | `info` | `"{domain} changed ({reason})"` | `Calm` |
///
/// A channel this build has never met gets its `kind` and its raw JSON, which is
/// the honest rendering of *the producer narrates something I have no words for*
/// — and the `Enum` on the producer's `kind` cell means a fourth channel is a
/// shape change somebody will see in a vendor bump.
fn note_of(channel: &str, raw: &Value) -> (String, NoteLevel) {
    let s = |k: &str| raw.get(k).and_then(Value::as_str).unwrap_or_default();
    match channel {
        "progress" => {
            let done = raw.get("done").and_then(Value::as_bool).unwrap_or(false);
            let ok = raw.get("ok").and_then(Value::as_bool);
            let level = if done && ok == Some(false) {
                NoteLevel::Critical
            } else {
                NoteLevel::Calm
            };
            (
                format!("{} · {} · {}", s("op"), s("phase"), s("detail")),
                level,
            )
        }
        "lifecycle" => {
            let phase = s("phase");
            let level = if phase == "going_down" {
                NoteLevel::Warn
            } else {
                NoteLevel::Calm
            };
            (format!("{} {} ({})", s("service"), phase, s("reason")), level)
        }
        "info" => (
            format!("{} changed ({})", s("domain"), s("reason")),
            NoteLevel::Calm,
        ),
        other => (format!("{other}: {raw}"), NoteLevel::Calm),
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  The fleet window, and where it comes from now
// ──────────────────────────────────────────────────────────────────────────

/// **WHICH THREADS THE FACE CAN SEE AT ALL, AND THE PRODUCER DECIDES IT.**
///
/// `threads/list` and `threads/status` each took a `limit` this face chose
/// (1,000 and 10,000), and `0031` records what that cost: the two numbers
/// defaulted to 200 and 100 behind the face's back and the header rendered
/// whichever reply landed last. The window is now the producer's, stated in its
/// own `shapes/list`:
///
/// * **`threads/recent-updates`** — `retained_items` 32, newest first, which is
///   the ORDER he looks at a fleet in. One keyless subscribe.
/// * **`threads/non-idle`** — the roster, one keyless subscribe, read through
///   the seam's own [`view::roster`]: EVERY non-idle thread — executing,
///   `parked`, `queued`, `retrying` — with the account it is placed on. It read
///   `accounts.*.threads` until R2.S8 deleted that cell, by a literal
///   `row.get("threads")` on an opaque row that no compiler could catch.
///   `view::roster` is typed against the vendored inventory, so the next move
///   is a build error. It is `roster` and NOT `running`: `running` is the
///   executing subset, and a fleet window built on it drops every parked thread
///   off the operator's screen (R2.S9's first pass did exactly that).
///
/// The union, deduplicated, in recency order with the non-idle roster appended. A
/// thread that is neither recent nor executing is not in this face's window and
/// never was — a page of 1,000 rendered as a fleet is honesty rule 1 with a
/// number on it, and that is why the count was retired in favour of naming the
/// window.
pub fn window(h: &held::Held) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in view::recent(h).known().unwrap_or_default() {
        if !out.contains(&r.id) {
            out.push(r.id);
        }
    }
    for id in view::roster(h).known().unwrap_or_default().into_keys() {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    // **AND THE LINEAGE OF WHAT IS HELD** (TUI-SUBAGENTS §3). A parent waiting
    // on its children is idle and may be in neither list, yet its row is the
    // one that must say "waiting on 2"; and each child's own `status` is what
    // its tree row reads. So the open thread joins the window, then every
    // held thread's children (the newest few runs) and its parent — one
    // generation either side. The open thread's whole tree is held below.
    for id in ledger::open(h) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    let mut more: Vec<String> = Vec::new();
    for id in &out {
        more.extend(children_held(h, id));
        if let Some(Some(p)) = view::parent_id(h, id).known() {
            more.push(p);
        }
    }
    for id in more {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    // **THE OPEN THREAD'S WHOLE TREE, TO THE ROOT AND DOWN** (TUI-P1c, TC-12).
    // The level above holds one generation either side of every thread in the
    // window, so a grandchild of the open thread — and everything under it —
    // had no key held: its row was drawn from its parent's `children` cell,
    // once, and its status never moved. The tree the face draws is rooted at
    // the outermost ancestor of the open thread, so that whole tree is held:
    // every ancestor (via `parent_id`), then every descendant of the root down
    // to `LINEAGE_DEPTH` levels (via `children`). A level arrives per wake-up —
    // a key is held, its cells land, the next wake-up reads them. Bounded by
    // `LINEAGE_MAX` keys so a parent with a huge fan-out cannot hold the fleet.
    let mut lineage: Vec<String> = Vec::new();
    for open in ledger::open(h) {
        let mut root = open.clone();
        for _ in 0..LINEAGE_DEPTH {
            match view::parent_id(h, &root).known() {
                Some(Some(p)) if p != root => root = p,
                _ => break,
            }
        }
        let mut level = vec![root];
        for _ in 0..=LINEAGE_DEPTH {
            let mut next = Vec::new();
            for id in level {
                if lineage.contains(&id) || lineage.len() >= LINEAGE_MAX {
                    continue;
                }
                next.extend(children_held(h, &id));
                lineage.push(id);
            }
            if next.is_empty() {
                break;
            }
            level = next;
        }
    }
    for id in lineage {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// The children of `id` the window holds: every one still owed, then the
/// newest `CHILDREN_HELD` runs.
fn children_held(h: &held::Held, id: &str) -> Vec<String> {
    let Some(c) = view::children(h, id).known() else {
        return Vec::new();
    };
    let runs = c.get("runs").and_then(Value::as_array).cloned().unwrap_or_default();
    let owed = c.get("outstanding").and_then(Value::as_array).cloned().unwrap_or_default();
    let recent = runs.iter().rev().take(CHILDREN_HELD).filter_map(|r| r.get("id"));
    owed.iter().chain(recent).filter_map(Value::as_str).map(str::to_string).collect()
}

/// How many of a parent's newest runs the window holds, beyond those still
/// owed — enough for the tree's last batch, bounded so an old parent with a
/// hundred spawns does not hold a hundred rows.
const CHILDREN_HELD: usize = 8;

/// How far the open thread's tree is held, up to its root and down from it.
const LINEAGE_DEPTH: usize = 8;

/// The most keys the open thread's tree may add to the window.
const LINEAGE_MAX: usize = 128;

/// Hold every key the window names, so its cells arrive. Idempotent —
/// `Book::hold` answers whether this was the first holder and only a first
/// holder sends a subscribe — so this runs on every wake-up for the price of a
/// `Vec::contains`.
///
/// A key appearing in the window for the first time is therefore HELD on one
/// wake-up and PROJECTED on the next, which is the correct order: a cell nobody
/// asked for reads `Holding::NotAsked`, and projecting that as a fact is the
/// defect the three-valued answer exists to prevent.
async fn hold_window(seam: &Seam) {
    for id in seam.read(window) {
        seam.hold(held::THREADS, &id).await;
    }
}

impl Projection {
    /// **THE FLEET, AS THE TWO EVENTS THE FACE ALREADY SPEAKS**, emitted only
    /// where the projection changed.
    ///
    /// Two events and not one, because `store.rs` reads two row shapes and
    /// splitting them is what keeps its arms at zero lines changed:
    /// `Fleet`'s row is the thread's STATE, `Threads`'s row is its IDENTITY.
    fn fleet(&mut self, seam: &Seam) -> Vec<TransportEvent> {
        let (statuses, headers, capped) = seam.read(|h| {
            let ids = window(h);
            let mut statuses = Vec::new();
            let mut headers = Vec::new();
            for id in &ids {
                statuses.push(status_row(h, id));
                headers.push(header_row(h, id));
            }
            // **THE PRODUCER'S OWN STATEMENT THAT THERE IS MORE**, off
            // `threads/summary`'s shape-level `truncated`. Not re-derived from
            // whether the window filled: two sources for one fact is a place
            // for them to disagree, and this one is the producer's.
            (statuses, headers, truncated(h).unwrap_or(false))
        });
        let mut out = Vec::new();
        let changed = |last: &BTreeMap<String, Value>, rows: &[Value]| -> bool {
            rows.iter().any(|r| {
                let id = r.get("thread_id").and_then(Value::as_str).unwrap_or("");
                last.get(id) != Some(r)
            })
        };
        if changed(&self.statuses, &statuses) || self.capped != Some(capped) {
            for r in &statuses {
                if let Some(id) = r.get("thread_id").and_then(Value::as_str) {
                    self.statuses.insert(id.to_string(), r.clone());
                }
            }
            out.push(TransportEvent::Fleet {
                statuses: statuses.clone(),
                capped,
            });
        }
        if changed(&self.headers, &headers) || self.capped != Some(capped) {
            for r in &headers {
                if let Some(id) = r.get("thread_id").and_then(Value::as_str) {
                    self.headers.insert(id.to_string(), r.clone());
                }
            }
            out.push(TransportEvent::Threads {
                rows: headers,
                capped,
            });
        }
        self.capped = Some(capped);
        out
    }

    /// **THE OPEN THREAD'S NEW ROWS, AS THE FRAMES THEY USED TO ARRIVE AS.**
    ///
    /// `threads/log` holds one named thread's live ledger — the producer's
    /// newest 4,000 events, every kind, unfiltered, assembled into nothing — and
    /// `ledger::rows_after` reads it with no request in it and no door it could
    /// ask through. Each row becomes the `ServerMsg::Event` it would have been
    /// on the firehose, so `store::apply_frame`, `classify_ledger_event` and
    /// every projection below them are unchanged.
    ///
    /// **`ledger::claim` IS THE WHOLE OF THE DEDUPLICATION**, and it replaces
    /// `transport::Cursors` outright: once per `(thread, seq)` in this process,
    /// with the floor seeded from the producer's own `seen_seq` rather than from
    /// anything this face persisted. The old cursor file, the repeat-over-drop
    /// bias, the three-way resync replay and the `Lagged` repair all go with it.
    fn rows(&mut self, seam: &Seam, now: DateTime<Utc>) -> Vec<TransportEvent> {
        let open = seam.read(|h| ledger::open(h));
        let mut out = Vec::new();
        for thread in open {
            let rows = seam.write(|h| rows_unclaimed(h, &thread));
            for r in rows {
                crate::diag!(
                    "frame",
                    "ROW thread={thread} seq={} kind={} message_id={} elided={} text_len={}",
                    r.seq,
                    r.kind,
                    r.message_id.map_or("-".to_string(), |m| m.to_string()),
                    r.elided,
                    r.event
                        .get("text")
                        .and_then(Value::as_str)
                        .map_or("absent".to_string(), |t| t.len().to_string())
                );
                out.push(TransportEvent::Frame {
                    msg: ServerMsg::Event {
                        thread_id: thread.clone(),
                        seq: r.seq,
                        message_id: r.message_id,
                        ts: r.ts.unwrap_or_default(),
                        event: ThreadEvent::parse(r.event),
                    },
                    // **NOT A REPLAY.** `replayed` marked a row that came back
                    // through `threads/{id}/history` rather than off the wire,
                    // and there is no history call left to come back through.
                    // Every row here is the producer's live window.
                    replayed: false,
                });
            }
            let _ = now;
        }
        out
    }
}

/// **A DEFECT IN THE VENDORED `view.rs`, WORKED AROUND HERE AND NAMED.**
///
/// `view::summary_truncated` is
/// `shared_cell(h, SUMMARY, "truncated", Value::as_bool)` — and the producer's
/// `shared.truncated` is an OBJECT, not a bool:
/// `{"truncated": <bool>, "reason": <str>}`
/// (`lib/threads/src/shapes/summary.rs`, which emits
/// `json!({"truncated": !complete, "reason": reason})`). `Value::as_bool` on an
/// object answers `None`, so `fact_of` returns
/// `NotHydrated("`truncated` is known and empty")` — **on every frame any
/// producer has ever exported, for the whole of that function's life.** It can
/// never answer `Known`.
///
/// Still true at `substrate-interface-layer@6bb96b1a`, `view.rs:428`. **The fix
/// belongs upstream**, in the one copy both faces diff against; `view.rs` is the
/// file the design says carries ZERO divergence, and a fence here would be a
/// divergence in the read vocabulary that §5's table forbids.
///
/// So this reads the producer's own object, above the seam, where it is this
/// face's workaround and not a second read vocabulary. It is not a
/// reimplementation of the view: it is three `get`s on a value the mirror
/// already holds.
///
/// `the_shared_truncated_cell_is_an_object_and_the_views_reader_cannot_see_it`
/// is the test, and it goes RED the day upstream is fixed — which is the point.
/// Delete this then, do not keep both.
fn truncated(h: &held::Held) -> Option<bool> {
    h.mirror(held::SUMMARY)?
        .shared()
        .get("truncated")?
        .get("truncated")?
        .as_bool()
}

/// One `TransportEvent::Fleet` row: the thread's STATE, and **only the cells the
/// mirror actually answers**.
///
/// `store.rs` reads `state` and `executing` as sticky (`if let Some(v) = …`) and
/// `queued`/`unread` as counted (`known_u32`), so an omitted field is already
/// spelled *no current answer* and lands as `Known::Unknown`. That is the whole
/// mechanism by which a `NotHydrated` cell renders as the `?` it always did.
fn status_row(h: &held::Held, id: &str) -> Value {
    let mut r = json!({ "thread_id": id });
    if let Some(s) = view::status(h, id).known() {
        r["state"] = json!(s.state);
        r["executing"] = json!(s.executing);
        r["detail"] = json!(s.detail);
    }
    // **THE STATUS THIS THREAD SHOWS, ROLLED UP OVER ITS WHOLE SUBTREE, AS
    // THE DAEMON SAYS IT** (R9.TS1, TUI-P1b2) — the `threads.rolled` cell,
    // verbatim. It replaced this face's own `rollup` over `threads/non-idle`
    // (R9.UI1), which spoke the producer's five state words rather than his
    // three and was a second copy of the rule.
    if let Some(v) = view::rolled(h, id).known() {
        r["rolled"] = v;
    }
    // Lineage, both directions — the parent this thread hangs under, and the
    // children it is waiting on.
    if let Some(Some(p)) = view::parent_id(h, id).known() {
        r["parent_id"] = json!(p);
    }
    if let Some(c) = view::children(h, id).known() {
        r["children"] = c;
    }
    if let Some(p) = view::positions(h, id).known() {
        r["queued"] = json!(p.queued);
        r["unread"] = json!(p.unread);
        r["last_seq"] = json!(p.newest_seq);
    }
    r
}

/// One `TransportEvent::Threads` row: the thread's IDENTITY.
///
/// `cwd`, `model` and `updated_at` come off the `header` cell, which is where
/// `threads/list` read them from in the first place; `account` is its own cell,
/// because *which login the daemon placed this on* is a decision the daemon
/// makes and not a setting anybody wrote.
///
/// **The `settings` blob is not read**, exactly as `store.rs`'s own header says
/// — the shape carries it (`view::settings`, byte-identical to what
/// `threads/{id}/get` returned, which the migration ledger pins) and nothing on
/// this screen is sourced from it.
fn header_row(h: &held::Held, id: &str) -> Value {
    let mut r = json!({ "thread_id": id });
    if let Some(hd) = view::header(h, id).known() {
        for (from, to) in [("cwd", "cwd"), ("model", "model"), ("updated_at", "updated_at")] {
            if let Some(v) = hd.get(from).and_then(Value::as_str) {
                r[to] = json!(v);
            }
        }
    }
    if let Some(Some(a)) = view::account(h, id).known() {
        r["account"] = json!(a);
    }
    if let Some(s) = view::status(h, id).known() {
        r["state"] = json!(s.state);
        r["executing"] = json!(s.executing);
    }
    if let Some(p) = view::positions(h, id).known() {
        r["last_seq"] = json!(p.newest_seq);
    }
    // **WHEN HE LAST SPOKE HERE, AS THE DAEMON KNOWS IT.** Since the threads
    // move the per-project `.mind/threads.json` indexes are no longer written on
    // his turns, so their `last_user_input_at` froze; the daemon's
    // `threads/recent-updates` entry `at` is the live clock (it moves ONLY on a
    // `user_message`, the same rule the band orders on). Carried here so the
    // store can order the recents band on it and age it from it.
    if let Some(at) = view::recent(h)
        .known()
        .unwrap_or_default()
        .into_iter()
        .find(|e| e.id == id)
        .and_then(|e| e.at)
    {
        r["last_user_input_at"] = json!(at.to_rfc3339());
    }
    // The daemon's name for the thread, for a recent thread no index carries.
    if let Some(Some(t)) = view::title(h, id).known() {
        r["title"] = json!(t);
    }
    // **THE NAME, LIVE, OFF THE ROW THIS FACE ALREADY HOLDS** (TUI-P1a). The
    // `app` cell carries the newest `summary_updated` payload for every held
    // key, so a rename — from this face, from the AUI, at creation — moves this
    // row and the header follows on the next projection. `threads/summary`'s
    // title above is the fallback: a keyless subscriber misses a thread's FIRST
    // name (that shape's "one known gap"), which is exactly the case of a
    // thread he names right after making it.
    if let Some(p) = view::app_payload(h, id, SUMMARY_EVENT_TYPE).known() {
        if let Some(n) = p.get("summary").and_then(Value::as_str).filter(|n| !n.trim().is_empty()) {
            r["name"] = json!(n);
            if p.get("manual").and_then(Value::as_bool) == Some(true) {
                r["name_manual"] = json!(true);
                if let Some(at) = p.get("ts").and_then(Value::as_str) {
                    r["name_at"] = json!(at);
                }
            }
        }
    }
    if !r.get("name").is_some_and(Value::is_string) {
        if let Some(t) = r.get("title").cloned() {
            r["name"] = t;
        }
    }
    // **WHO STARTED IT** — the one fact taken off the `settings` blob, and the
    // only one: `app_id`, and an agent's tag in its opaque `metadata`. Not the
    // blob itself, which is still never copied (see the test below this
    // function's module). `identity: true` says the cell was READ, so the store
    // can tell "no app" from "not told yet".
    if let Some(s) = view::settings(h, id).known() {
        r["identity"] = json!(true);
        if let Some(a) = s.get("app_id").and_then(Value::as_str) {
            r["app_id"] = json!(a);
        }
        if let Some(a) = s.get("metadata").and_then(|m| m.get("agent")).and_then(Value::as_str) {
            r["agent"] = json!(a);
        }
    }
    r
}

/// **THE BYTES OF A NAME HE GAVE** — what [`Handle::name`] sends as the
/// `summary_updated` event's `data`. `by: tui` says which face; `manual: true`
/// ranks it above an agent's drifting summary in `Store::label_of`; `ts` is
/// when, so a later manual name wins over an earlier one.
pub fn name_payload(name: &str, now: DateTime<Utc>) -> Value {
    json!({ "summary": name, "by": "tui", "manual": true, "ts": now.to_rfc3339() })
}

/// The application event type that NAMES a thread — the producer's
/// `shapes::summary::SUMMARY_EVENT_TYPE`, spelled once on this side.
pub const SUMMARY_EVENT_TYPE: &str = "summary_updated";

// ──────────────────────────────────────────────────────────────────────────
//  The open thread's bulk load — what `threads/{id}/history` paging was
// ──────────────────────────────────────────────────────────────────────────

/// **OPEN A THREAD: HOLD ITS WINDOW AND READ IT.** The replacement for
/// `reads::spawn_transcript`, and the shape of the change is the whole argument.
///
/// What it was: `threads/{id}/get` for `last_seq`, then up to nine
/// `threads/{id}/history` pages of 500 walking forward from
/// `last_seq - OPEN_CAP`, with a `max_pages` bound so a thread that grew while
/// we paged still terminated, and a comment explaining that `seq` was MEASURED
/// dense on the live daemon so the arithmetic landed where it said.
///
/// What it is: one keyed subscribe, and the producer's export IS the page. The
/// window is 4,000 rows — `retained_items` on its own `shapes/list` entry, the
/// same number `OPEN_CAP` guessed — so there is no page size left to choose, no
/// cursor arithmetic to get wrong and no density to depend on.
///
/// `MAX_OPEN_KEYS_PER_CONN` is 8 and is enforced as a RESERVATION: asking for a
/// ninth is REFUSED rather than clamped, which is why [`Handle::watch_log`] goes
/// through `Seam::watch_log` and evicts the least recently opened one first,
/// reading the ceiling off `shapes/list` and never hardcoding it.
pub fn spawn_transcript(h: Handle, tx: mpsc::Sender<Ev>, thread_id: String) {
    tokio::spawn(async move {
        h.watch_log(&thread_id).await;
        h.hold_thread(&thread_id).await;
        let seam = h.seam.clone();
        let fetch = move |id: String| {
            let seam = seam.clone();
            async move { fetch_resource(&seam, &id).await }
        };
        // **DRAWN FIRST, FILLED SECOND** (THREAD-LOAD-LATENCY F7). The window
        // is handed over the moment it lands; every elided row the cache does
        // not already hold says its words are loading, and the bodies follow
        // newest-first (the rows on screen) as `Ev::TranscriptBodies`.
        // Measured on prod: 218 elided bodies took 6.5 s through the resource
        // door, all of it spent on "loading this thread's tail" before.
        match open_transcript(&h, &thread_id, HYDRATE_WAIT).await {
            Err(why) => {
                let _ = tx.send(Ev::Transcript { thread_id, result: Err(why) }).await;
            }
            Ok((rows, from, pending)) => {
                let _ = tx
                    .send(Ev::Transcript { thread_id: thread_id.clone(), result: Ok((rows.clone(), from)) })
                    .await;
                fill_bodies(&h, &thread_id, rows, pending, &fetch, &tx).await;
            }
        }
    });
}

/// How many completed bodies one [`Ev::TranscriptBodies`] carries.
pub const BODY_CHUNK: usize = 24;

/// What an elided row says while its words are on their way. A message keeps
/// its kind and says so in its own text; anything else keeps its kind and its
/// skeleton. Never an empty box.
fn pending_event(e: &Elided) -> Value {
    let mut ev = match &e.skeleton {
        Some(sk) if sk.is_object() => sk.clone(),
        _ => json!({}),
    };
    ev["kind"] = json!(e.kind);
    if matches!(e.kind.as_str(), "user_message" | "assistant_text" | "thinking") {
        ev["text"] = json!("[loading this message's text from the daemon…]");
    }
    ev
}

/// [`read_transcript`]'s first half: wait for the window, read it, complete
/// every elided row the cache already holds, and put [`pending_event`] on the
/// rest — which are returned NEWEST FIRST, for [`fill_bodies`].
pub async fn open_transcript(
    h: &Handle,
    thread_id: &str,
    wait: std::time::Duration,
) -> Result<(Vec<LedgerRow>, u64, Vec<(u64, Elided)>), String> {
    let start = std::time::Instant::now();
    while !h.read(|held| with_log_rows(held, thread_id, |_, _| ()).is_some()) && start.elapsed() < wait {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    crate::diag!(
        "shape",
        "HYDRATE-WAIT shape={} key={thread_id} waited_ms={} state={}",
        held::LOG,
        start.elapsed().as_millis(),
        h.read(|held| log_state(held, thread_id))
    );
    let (mut rows, from) = transcript(h, thread_id)?;
    let elided = h.read(|held| elisions(held, thread_id));
    let mut pending = Vec::new();
    {
        let cache = h.bodies.0.lock().unwrap_or_else(|e| e.into_inner());
        for r in rows.iter_mut() {
            if let Some(el) = elided.get(&r.seq) {
                match cache.get(&(thread_id.to_string(), r.seq)) {
                    Some(v) => r.event = ThreadEvent::parse(v.clone()),
                    None => {
                        r.event = ThreadEvent::parse(pending_event(el));
                        pending.push((r.seq, el.clone()));
                    }
                }
            }
        }
    }
    pending.sort_by(|a, b| b.0.cmp(&a.0));
    crate::diag!(
        "shape",
        "TRANSCRIPT-DRAWN key={thread_id} rows={} elided={} pending={} waited_ms={}",
        rows.len(),
        elided.len(),
        pending.len(),
        start.elapsed().as_millis()
    );
    Ok((rows, from, pending))
}

/// [`read_transcript`]'s second half, off the face's loop: every pending body,
/// [`BODY_FETCH_CONCURRENCY`] in flight, newest first, handed to the store in
/// chunks of [`BODY_CHUNK`] as they complete. Stops early if the face has gone
/// away.
pub async fn fill_bodies<F, Fut>(
    h: &Handle,
    thread_id: &str,
    rows: Vec<LedgerRow>,
    pending: Vec<(u64, Elided)>,
    fetch: &F,
    tx: &mpsc::Sender<Ev>,
) where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    use futures_util::StreamExt;
    if pending.is_empty() {
        return;
    }
    let t0 = std::time::Instant::now();
    let n = pending.len();
    let mut by_seq: std::collections::HashMap<u64, LedgerRow> =
        rows.into_iter().map(|r| (r.seq, r)).collect();
    let bodies = &h.bodies;
    let mut done = futures_util::stream::iter(pending)
        .map(|(seq, el)| async move { (seq, resolve(thread_id, seq, &el, bodies, fetch).await) })
        .buffer_unordered(BODY_FETCH_CONCURRENCY);
    let mut chunk = Vec::new();
    while let Some((seq, v)) = done.next().await {
        if let Some(mut r) = by_seq.remove(&seq) {
            r.event = ThreadEvent::parse(v);
            chunk.push(r);
        }
        if chunk.len() >= BODY_CHUNK {
            chunk.sort_by_key(|r| r.seq);
            let rows = std::mem::take(&mut chunk);
            if tx.send(Ev::TranscriptBodies { thread_id: thread_id.to_string(), rows }).await.is_err() {
                return;
            }
        }
    }
    if !chunk.is_empty() {
        chunk.sort_by_key(|r| r.seq);
        let _ = tx.send(Ev::TranscriptBodies { thread_id: thread_id.to_string(), rows: chunk }).await;
    }
    crate::diag!("shape", "BODIES-FILLED key={thread_id} bodies={n} ms={}", t0.elapsed().as_millis());
}

/// How long opening a thread waits for its `threads/log` export to land
/// before it says it has no mirror for it.
pub const HYDRATE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// **THE TRANSCRIPT AS THE STORE IS HANDED IT.**
///
/// Two things the bare [`transcript`] read does not do, and both were
/// operator-visible:
///
/// * **It waits for the window.** `watch_log` returns when the subscribe has
///   LEFT, not when the export has LANDED, so reading the mirror straight
///   after it found the key unhydrated on most opens and put *"could not load
///   the thread"* on screen while the rows arrived a moment later anyway. It
///   now waits up to `wait` for the key to hydrate, and only then says it has
///   no mirror.
/// * **It fills every elided row** through the resource door, exactly as
///   [`deliver`] does for live rows, so an opened thread never shows his own
///   long message as an empty box.
pub async fn read_transcript<F, Fut>(
    h: &Handle,
    thread_id: &str,
    wait: std::time::Duration,
    fetch: F,
) -> Result<(Vec<LedgerRow>, u64), String>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, String>>,
{
    let (mut rows, from, pending) = open_transcript(h, thread_id, wait).await?;
    let t_bodies = std::time::Instant::now();
    let n = pending.len();
    let mut done = resolve_many(thread_id, pending, &h.bodies, &fetch).await;
    for r in rows.iter_mut() {
        if let Some(v) = done.remove(&r.seq) {
            r.event = ThreadEvent::parse(v);
        }
    }
    crate::diag!(
        "shape",
        "TRANSCRIPT-READY key={thread_id} rows={} fetched={n} bodies_ms={}",
        rows.len(),
        t_bodies.elapsed().as_millis()
    );
    Ok((rows, from))
}

/// The window as the face's rows, plus the seq the window STARTS at.
///
/// `Err` is the honest answer while the key has not hydrated: *this face holds
/// no mirror for the key* is not *the thread is empty*, and `store.rs` draws
/// the two differently. **Which of the two it is, this function cannot say** —
/// it reads local state and sends nothing, so it names the condition and not a
/// cause. The next wake-up re-reads, because
/// [`Projection::rows`] feeds every row above the claim floor whether or not
/// anybody asked for a transcript.
fn transcript(h: &Handle, thread_id: &str) -> Result<(Vec<LedgerRow>, u64), String> {
    let now = Utc::now();
    h.read(|held| {
        let rows = ledger::rows_after(held, thread_id, 0);
        if rows.is_empty() && !view::log_rows(held, thread_id).is_known() {
            // **SAY ONLY WHAT THIS CODE CAN SEE.** The condition above is two
            // reads of the face's OWN held mirror — `ledger::rows_after` and
            // `view::log_rows`, which is `items(h, threads_log::EVENTS, id)`
            // over local state. Neither asks the producer anything, so neither
            // can establish what the producer did.
            //
            // The message that stood here said *"the producer has not handed
            // `threads/log` over for … yet"*, and that is an attribution the
            // instrument cannot support: an unhydrated key here is equally
            // consistent with never having subscribed for it. **Three
            // investigators chased the producer on the strength of this one
            // sentence** and a question to the operator had to be withdrawn —
            // doctrine 9's shape (a correct instrument naming the wrong party)
            // in a user-facing string.
            //
            // The unhydrated-is-not-empty distinction is the part that was
            // right and it survives: `store.rs` draws the two differently.
            let state = log_state(held, thread_id);
            crate::diag!(
                "shape",
                "READ-ERR shape={} key={thread_id} reason={state} held_windows={:?}",
                held::LOG,
                ledger::open(held)
            );
            return Err(format!(
                "no mirror for `threads/log`/{thread_id} yet ({state})"
            ));
        }
        let from = view::log_covers_from(held, thread_id)
            .or_else(|| rows.first().map(|r| r.seq))
            .unwrap_or(0);
        Ok((
            rows.into_iter()
                .map(|r| LedgerRow {
                    seq: r.seq,
                    message_id: r.message_id,
                    ts: r
                        .ts
                        .as_deref()
                        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                        .map(|d| d.with_timezone(&Utc)),
                    received: now,
                    event: ThreadEvent::parse(r.event),
                })
                .collect(),
            from,
        ))
    })
}

// ──────────────────────────────────────────────────────────────────────────
//  The projection, proved by FOLDING FRAMES — no socket in any of it
// ──────────────────────────────────────────────────────────────────────────

/// **EVERY TEST HERE SEEDS BY FOLDING PRODUCER FRAMES**, through the real
/// [`held::Held::fold`], and then reads what [`Projection::step`] would have
/// told the face. Nothing pokes a `Mirror`'s insides and nothing opens a socket,
/// which is what makes these results evidence rather than a tautology over a
/// hand-built struct.
///
/// `Projection::step` takes a `&Seam` and the tests have no link, so each one
/// builds a `Seam` with `Supervision::None` — which spawns nothing and connects
/// to nothing — and folds into its `Held` directly. The `tokio` runtime is
/// needed only because `Seam::start` spawns the link task.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mirror::fixtures as fx;
    use serde_json::json;

    const AT: &str = "2026-10-07T12:00:00Z";

    /// A seam with no daemon behind it, and its mirror seeded by FOLDING.
    ///
    /// `spawn_dead` rather than `Seam::start` directly, so the port guard this
    /// crate's tests live under applies here too — see [`TestPorts`].
    fn seeded(frames: &[Value]) -> (Arc<Seam>, Projection) {
        let (h, _rx) = spawn_dead();
        {
            let mut g = h.seam.lock();
            g.learn(&fx::shape_infos());
            g.link = held::LinkState::Up {
                boot_id: "boot-1".into(),
                since: AT.parse().expect("a fixed stamp"),
            };
            for f in frames {
                g.fold(f);
            }
        }
        (h.seam.clone(), Projection::default())
    }

    fn hold(s: &Seam, shape: &'static str, key: &str) {
        s.lock().book.hold(shape, Some(key));
    }

    fn notes(evs: &[TransportEvent]) -> Vec<(String, NoteLevel)> {
        evs.iter()
            .filter_map(|e| match e {
                TransportEvent::Note { text, level } => Some((text.clone(), *level)),
                _ => None,
            })
            .collect()
    }

    fn fleet(evs: &[TransportEvent]) -> Option<(Vec<Value>, bool)> {
        evs.iter().find_map(|e| match e {
            TransportEvent::Fleet { statuses, capped } => Some((statuses.clone(), *capped)),
            _ => None,
        })
    }

    fn headers(evs: &[TransportEvent]) -> Option<Vec<Value>> {
        evs.iter().find_map(|e| match e {
            TransportEvent::Threads { rows, .. } => Some(rows.clone()),
            _ => None,
        })
    }

    fn frames(evs: &[TransportEvent]) -> Vec<(String, u64)> {
        evs.iter()
            .filter_map(|e| match e {
                TransportEvent::Frame {
                    msg: ServerMsg::Event { thread_id, seq, .. },
                    ..
                } => Some((thread_id.clone(), *seq)),
                _ => None,
            })
            .collect()
    }

    // ── the window ──────────────────────────────────────────────────────────

    /// **THE WINDOW IS `threads/recent-updates` ∪ the `threads/non-idle`
    /// roster, AND THE PRODUCER CHOSE BOTH HALVES.** (The second half was
    /// `accounts.*.threads` until R2.S8 deleted that cell.)
    ///
    /// `threads/list{limit: 1000}` and `threads/status{limit: 10000}` are what
    /// this replaces, and `0031` records the bill for a face choosing those
    /// numbers: they defaulted to 200 and 100 behind its back and the header
    /// rendered whichever reply landed last. Recency order first, then the
    /// executing set, deduplicated.
    #[tokio::test]
    async fn the_window_is_recency_then_the_executing_set_deduplicated() {
        let (s, _) = seeded(&[
            fx::recent_export(&[("t1", 0, 0), ("t2", 2, 5)]),
            // `t2` is executing AND recent — it must appear once; `t9` is
            // executing and not recent, which is the half `recent-updates`
            // cannot answer.
            fx::non_idle_export(&[("t2", "acct", "active", true), ("t9", "acct", "active", true)]),
        ]);
        assert_eq!(s.read(window), vec!["t1", "t2", "t9"]);
    }

    /// **A PARKED THREAD IS IN THE WINDOW, AND AN IDLE ONE IS NOT.**
    ///
    /// The window is the whole non-idle ROSTER, not its executing subset: `p7`
    /// is parked (rate-limited, not executing) and not recent, and it is exactly
    /// the thread the operator needs to see — a window read off `view::running`
    /// drops it. The opposing half: `t-old` is recent-and-gone from the roster
    /// in an earlier export and idle now, and `t-idle` was only ever named by a
    /// thread row the face holds; neither is non-idle, so a window that answered
    /// "every thread the daemon has ever had" puts them in and goes red.
    #[tokio::test]
    async fn a_parked_thread_is_in_the_window_and_an_idle_one_is_not() {
        let (s, _) = seeded(&[
            fx::recent_export(&[("t1", 0, 0)]),
            fx::non_idle_export(&[("t-old", "acct", "active", true)]),
        ]);
        // POSITIVE SENTINEL, read BETWEEN the two exports: `t-old` is non-idle
        // now and IS in the window. Without this read, a `window()` that kept a
        // union of every id it had ever seen would only ever be asked once —
        // after the second export — and would pass.
        assert_eq!(s.read(window), vec!["t1", "t-old"], "t-old is non-idle and drawn");
        s.write(|h| {
            h.fold(&fx::non_idle_export(&[
                ("t2", "acct", "active", true),
                ("p7", "acct-limited", "parked", false),
            ]));
        });
        hold(&s, held::THREADS, "t-idle");
        s.write(|h| {
            h.fold(&fx::threads_export("t-idle", "idle", false, 0, 0, json!({}), Some("acct")));
        });
        let w = s.read(window);
        assert_eq!(w, vec!["t1", "p7", "t2"], "recent first, then the roster in key order");
        for gone in ["t-old", "t-idle"] {
            assert!(!w.contains(&gone.to_string()), "`{gone}` is idle and is in the window: {w:?}");
        }
    }

    /// **A WAITING PARENT HOLDS ITS CHILDREN, AND ITS STATUS ROW CARRIES THE
    /// LINEAGE** (TUI-SUBAGENTS §3). The parent is idle — in neither the
    /// recents nor the roster — so it enters through the open log; its
    /// `children` cell then pulls both children into the window, and its
    /// `Fleet` row carries `children` while a child's carries `parent_id` and
    /// the park reason. RED without the change: the window was recents ∪
    /// roster only, and no status row carried any of the three.
    #[tokio::test]
    async fn a_waiting_parent_holds_its_children_and_carries_its_lineage() {
        let (s, mut p) = seeded(&[fx::recent_export(&[]), fx::non_idle_export(&[])]);
        hold(&s, held::THREADS, "p1");
        let mut parent = fx::threads_export("p1", "idle", false, 0, 0, json!({}), None);
        parent["row"]["children"] = json!({
            "outstanding": ["c1", "c2"],
            "runs": [
                {"id": "c1", "agent": "scout", "name": null, "wake": "end",
                 "started_at": AT, "request_id": "r1", "finished": null},
                {"id": "c2", "agent": "scout", "name": null, "wake": "end",
                 "started_at": AT, "request_id": "r2", "finished": null}
            ]
        });
        parent["cells"]["children"] = fx::known(AT);
        parent["row"]["parent_id"] = Value::Null;
        parent["cells"]["parent_id"] = fx::known(AT);
        s.write(|h| h.fold(&parent));
        hold(&s, held::LOG, "p1");
        let w = s.read(window);
        for id in ["p1", "c1", "c2"] {
            assert!(w.contains(&id.to_string()), "`{id}` is not in the window: {w:?}");
        }
        hold(&s, held::THREADS, "c2");
        let mut child = fx::threads_export("c2", "parked", false, 0, 0, json!({}), None);
        child["row"]["status"]["detail"] = json!("no budget config for key scout-b");
        child["row"]["parent_id"] = json!("p1");
        child["cells"]["parent_id"] = fx::known(AT);
        s.write(|h| h.fold(&child));
        let rows = fleet(&p.fleet(&s)).expect("no fleet event").0;
        let row = |id: &str| rows.iter().find(|r| r["thread_id"] == id).cloned().expect(id);
        assert_eq!(row("p1")["children"]["outstanding"], json!(["c1", "c2"]));
        assert_eq!(row("c2")["parent_id"], "p1");
        assert_eq!(row("c2")["detail"], "no budget config for key scout-b");
    }

    /// **THE OPEN THREAD'S WHOLE TREE IS HELD, TO THE ROOT AND DOWN — SO A
    /// DEPTH-2 CHILD'S ROW MOVES LIVE** (TUI-P1c, TC-12).
    ///
    /// ```text
    /// p1 ─┬ c1 (OPEN) ── g1 ── gg1
    ///     └ c2 ── s1
    /// r1 (recent) ── rc1 ── rg1
    /// ```
    ///
    /// He entered `c1`. The tree is drawn from `p1`, so every node of it must
    /// be held: `c2` and `s1` (the sibling's fold) and `gg1` (depth 2 below the
    /// open thread, depth 3 below the root). The old window held one generation
    /// either side of the open thread — `p1` and `g1` — and nothing else, so
    /// `gg1`, `c2`, `s1` were drawn once from a parent's `children` cell and
    /// never moved. Opposed: `r1` is only recent, so only ONE generation of
    /// it is held (`rc1`), never `rg1` — a window that held every descendant
    /// of everything goes red.
    ///
    /// FORCED RED: the lineage block in `window` deleted (depth-1 only) — `gg1`
    /// is missing. Opposed: the lineage walk run from every windowed thread
    /// instead of the open one's root — `rg1` is in.
    #[tokio::test]
    async fn the_open_threads_whole_tree_is_held_and_only_its_tree() {
        let (s, _) = seeded(&[fx::recent_export(&[("r1", 0, 0)]), fx::non_idle_export(&[])]);
        let node = |id: &str, parent: Option<&str>, kids: &[&str]| {
            let mut v = fx::threads_export(id, "idle", false, 0, 0, json!({}), None);
            v["row"]["children"] = json!({
                "outstanding": [],
                "runs": kids.iter().map(|k| json!({"id": k, "agent": "scout", "name": null,
                    "wake": "end", "started_at": AT, "request_id": k, "finished": null}))
                    .collect::<Vec<_>>()
            });
            v["cells"]["children"] = fx::known(AT);
            v["row"]["parent_id"] = parent.map_or(Value::Null, |p| json!(p));
            v["cells"]["parent_id"] = fx::known(AT);
            v
        };
        let tree = [
            ("p1", None, &["c1", "c2"][..]),
            ("c1", Some("p1"), &["g1"][..]),
            ("g1", Some("c1"), &["gg1"][..]),
            ("gg1", Some("g1"), &[][..]),
            ("c2", Some("p1"), &["s1"][..]),
            ("s1", Some("c2"), &[][..]),
            ("r1", None, &["rc1"][..]),
            ("rc1", Some("r1"), &["rg1"][..]),
            ("rg1", Some("rc1"), &[][..]),
        ];
        for (id, parent, kids) in tree {
            hold(&s, held::THREADS, id);
            s.write(|h| h.fold(&node(id, parent, kids)));
        }
        hold(&s, held::LOG, "c1");
        let w = s.read(window);
        for id in ["c1", "p1", "g1", "gg1", "c2", "s1"] {
            assert!(w.contains(&id.to_string()), "`{id}` of the open tree is not held: {w:?}");
        }
        assert!(w.contains(&"rc1".to_string()), "sentinel: a recent thread's child is held: {w:?}");
        assert!(!w.contains(&"rg1".to_string()), "a recent thread's GRANDCHILD is held: {w:?}");
    }

    /// **THE QUEUE PANEL READS THE `threads.queue` CELL, NOT THE WIRE.**
    ///
    /// The two methods are gone from the producer; the cell is the ruling that
    /// replaced them. (Two shas stood here as that ruling's provenance and are
    /// REMOVED: neither resolves as an object in this repository — R2.S2b.)
    /// The seam here is DOWN (port 1), so
    /// an answer can only have come off the mirror — and each leaf must carry
    /// exactly its own half, which a constant answer cannot do.
    #[tokio::test]
    async fn the_queue_reads_answer_from_the_threads_queue_cell() {
        let (h, _rx) = spawn_dead();
        {
            let mut g = h.seam.lock();
            g.learn(&fx::shape_infos());
            g.link = held::LinkState::Up {
                boot_id: "boot-1".into(),
                since: AT.parse().expect("a fixed stamp"),
            };
            g.book.hold(held::THREADS, Some("t1"));
            let q = |id: u64, text: &str, state: &str| {
                json!({"message_id": id, "text": text, "intent": "say",
                       "created_at": AT, "state": state})
            };
            g.fold(&fx::export(
                held::THREADS,
                Some("t1"),
                json!({"queue": [q(1, "still waiting", "waiting"), q(2, "did not get through", "failed")]}),
                json!({"queue": fx::known(AT)}),
            ));
        }
        let waiting = h.enveloped_queue_read("queue", "t1").await.expect("the cell is known");
        let failed = h.enveloped_queue_read("queue_failed", "t1").await.expect("the cell is known");
        let ids = |v: &Value, k: &str| -> Vec<u64> {
            v[k].as_array()
                .map(|a| a.iter().filter_map(|m| m["message_id"].as_u64()).collect())
                .unwrap_or_default()
        };
        assert_eq!(ids(&waiting, "waiting"), vec![1], "{waiting}");
        assert_eq!(ids(&failed, "failed"), vec![2], "{failed}");
        assert_eq!(failed["failed"][0]["text"], "did not get through", "verbatim");
    }

    /// **THE RETIRED `accounts.*.threads` FRAME PUTS NOTHING IN THE WINDOW, AND
    /// THE ROSTER FRAME CARRYING THE SAME THREAD DOES.**
    ///
    /// The read this replaced was `row.get("threads")` on an opaque account row —
    /// a literal key the compiler cannot see. A face that kept it would put `t9`
    /// in the window off the OLD frame below and pass every test that seeds the
    /// old shape. So the old frame is fed and must move nothing; and the positive
    /// sentinel, the same thread on a `threads/non-idle` frame, must move it —
    /// otherwise the first half would pass on a window that reads nothing at all.
    #[tokio::test]
    async fn the_retired_account_threads_cell_is_not_read_and_the_roster_is() {
        let (s, _) = seeded(&[
            fx::recent_export(&[("t1", 0, 0)]),
            fx::legacy_accounts_export_with_threads("acct", &[("t9", true)]),
        ]);
        assert_eq!(
            s.read(window),
            vec!["t1"],
            "`t9` came off `accounts.*.threads`, the cell R2.S8 deleted — this face \
             still reads the old shape"
        );
        s.write(|h| {
            h.fold(&fx::non_idle_export(&[("t9", "acct", "active", true)]));
        });
        assert_eq!(s.read(window), vec!["t1", "t9"], "the roster frame is the source");
    }

    /// **A THREAD NO EXPORT NAMED IS NOT IN THE WINDOW**, and the empty window
    /// is the honest answer before anything hydrates — not *the fleet is empty*.
    #[tokio::test]
    async fn an_unhydrated_fleet_is_an_empty_window_and_not_an_empty_fleet() {
        let (s, mut p) = seeded(&[]);
        assert!(s.read(window).is_empty());
        // And the projection says nothing about threads rather than publishing
        // a fleet of zero rows, which `store.rs` would read as *I was told*.
        let out = p.step(&s, None, Utc::now());
        assert_eq!(fleet(&out).map(|(rows, _)| rows.len()), Some(0));
        assert_eq!(headers(&out).map(|r| r.len()), Some(0));
    }

    // ── R9.TS1 / TUI-P1b2: the daemon's rolled status, on the fleet row ────

    /// **THE FLEET ROW CARRIES THE DAEMON'S `rolled` CELL, VERBATIM, AND NO
    /// ROLLUP OF THIS FACE'S OWN** (TUI-P1b2). `p` is idle with an idle child
    /// `c`, and the roster says a grandchild `g` under `c` is PARKED — the old
    /// face-side rule (`rollup_in`, R9.UI1) would have rolled that up to
    /// `parked`. The daemon says `active (+1)` (a running descendant it knows
    /// and this window does not hold) and that is what the row carries, with
    /// its `children_summary`, and there is no `rollup` key at all. `q` is held
    /// with no `rolled` cell hydrated: its row carries no `rolled` (no answer,
    /// not a guessed one), and `r`'s `idle` is not `p`'s, so a constant fails.
    ///
    /// FORCED RED: (a) the `r["rolled"] = v` line removed — `p` has no
    /// `rolled`; (b) opposed, the old `rollup_in` line restored — the row
    /// grows a `rollup` key.
    #[tokio::test]
    async fn the_fleet_row_carries_the_daemons_rolled_cell_and_no_rollup_of_its_own() {
        let (s, mut proj) = seeded(&[fx::recent_export(&[]), fx::non_idle_export(&[])]);
        let rolled = |status: &str, display: &str, x: u64, sum: Option<&str>| {
            json!({"status": status, "error": null, "active_descendants": x, "display": display,
                   "children_active": null, "children_idle": null, "children_error": null,
                   "children_summary": sum})
        };
        for (id, r) in [
            ("p", Some(rolled("active", "active (+1)", 1, Some("1 active, 19 idle")))),
            ("r", Some(rolled("idle", "idle", 0, None))),
            ("q", None),
        ] {
            hold(&s, held::THREADS, id);
            let mut t = fx::threads_export(id, "idle", false, 0, 0, json!({}), Some("acct"));
            if let Some(r) = r {
                t["row"]["rolled"] = r;
                t["cells"]["rolled"] = fx::known(AT);
            }
            s.write(|h| h.fold(&t));
            hold(&s, held::LOG, id);
        }
        s.write(|h| {
            h.fold(&fx::non_idle_export_with_ancestors(&[("g", "acct", "parked", false, &["c", "p"][..])]));
        });
        let rows = fleet(&proj.fleet(&s)).expect("no fleet event").0;
        let row = |id: &str| rows.iter().find(|r| r["thread_id"] == id).cloned().unwrap_or_else(|| panic!("{id}: {rows:?}"));
        assert_eq!(row("p")["rolled"]["display"], json!("active (+1)"), "{:?}", row("p"));
        assert_eq!(row("p")["rolled"]["children_summary"], json!("1 active, 19 idle"));
        assert_eq!(row("r")["rolled"]["display"], json!("idle"));
        assert!(row("q").get("rolled").is_none(), "an unhydrated cell must be omitted: {:?}", row("q"));
        for id in ["p", "r", "q"] {
            assert!(row(id).get("rollup").is_none(), "a face-side rollup is back on {id}: {:?}", row(id));
        }
    }

    // ── THE LOAD-BEARING ONE: Fact -> Known, by omission ────────────────────

    /// **A CELL THE PRODUCER HAS NOT HYDRATED IS OMITTED, NOT GUESSED** — and
    /// that omission is the whole mechanism by which an un-hydrated cell renders
    /// as the `?` it always did, with `store.rs` unchanged.
    ///
    /// `store.rs` reads `state`/`executing` as sticky (`if let Some(v) = …`) and
    /// `queued`/`unread` through `known_u32`, which answers `Known::Unknown` for
    /// an absent field. So the three-valued `Fact` and the store's `Known`
    /// already agree, and this is the assertion that says so rather than a
    /// comment claiming it.
    #[tokio::test]
    async fn a_cell_the_producer_has_not_hydrated_is_omitted_rather_than_guessed() {
        let (s, _) = seeded(&[fx::recent_export(&[("t1", 0, 0)])]);
        hold(&s, held::THREADS, "t1");
        // Asked for, nothing exported: the row is its identity and nothing else.
        let bare = s.read(|h| status_row(h, "t1"));
        assert_eq!(bare, json!({"thread_id": "t1"}));
        for absent in ["state", "executing", "queued", "unread", "last_seq"] {
            assert!(
                bare.get(absent).is_none(),
                "`{absent}` was published for a thread the producer has not \
                 exported — that is a guess rendered as a fact"
            );
        }
        // And once the export lands, every one of them is there.
        s.write(|h| {
            h.fold(&fx::threads_export(
                "t1",
                "active",
                true,
                3,
                41,
                json!({"cwd": "/tmp", "model": "sonnet"}),
                Some("acct"),
            ));
        });
        let full = s.read(|h| status_row(h, "t1"));
        assert_eq!(full["state"], json!("active"));
        assert_eq!(full["executing"], json!(true));
        assert_eq!(full["last_seq"], json!(41), "`newest.seq` is the old `last_seq`");
        assert_eq!(full["unread"], json!(0));
        assert_eq!(full["queued"], json!(0));
    }

    /// **THE IDENTITY ROW IS THE `header` CELL, AND THE `settings` BLOB IS NOT
    /// READ** — which is `store.rs`'s own rule, kept rather than quietly
    /// widened now that the shape carries settings for free.
    #[tokio::test]
    async fn the_identity_row_is_the_header_cell_and_never_the_settings_blob() {
        let (s, _) = seeded(&[fx::recent_export(&[("t1", 0, 0)])]);
        hold(&s, held::THREADS, "t1");
        s.write(|h| {
            h.fold(&fx::threads_export(
                "t1",
                "idle",
                false,
                0,
                7,
                json!({"cwd": "/should/not/be/read", "model": "opus"}),
                Some("acct"),
            ));
        });
        let row = s.read(|h| header_row(h, "t1"));
        // `header` says `/tmp` and `sonnet`; `settings` says something else.
        // The row must be the header's.
        assert_eq!(row["cwd"], json!("/tmp"));
        assert_eq!(row["model"], json!("sonnet"));
        assert_eq!(row["account"], json!("acct"));
        assert!(row.get("settings").is_none());
        assert!(
            row.as_object().expect("an object").values().all(|v| v != "/should/not/be/read"),
            "the settings blob reached the identity row: {row}"
        );
    }

    // ── TUI-P1a: the name, who started it, and the first line ───────────────

    /// A `threads` export whose `app` cell and `settings` say what the test
    /// needs, and optionally a first message page.
    fn named_export(id: &str, app: Value, settings: Value, pages: Option<Value>) -> Value {
        let mut e = fx::threads_export(id, "idle", false, 0, 7, settings, Some("acct"));
        e["row"]["app"] = app;
        if let Some(p) = pages {
            e["row"]["pages"] = p;
            e["cells"]["pages"] = fx::known(AT);
        }
        e
    }

    /// **THE IDENTITY ROW CARRIES THE THREAD'S NAME OFF THE `app` CELL, AND A
    /// SECOND NAME REPLACES THE FIRST** — a rename arriving on the row this
    /// face already holds is all it takes (TUI-P1a, TC-03). Opposed by a thread
    /// with no `summary_updated`, which carries NO name (so a constant name
    /// fails), and the manual flag and its stamp travel with a person's name
    /// only.
    ///
    /// Forcings (report): delete the `app_payload` block in `header_row` → the
    /// renamed assertions are red; copy `manual` unconditionally → the agent
    /// summary's assertion is red.
    #[tokio::test]
    async fn the_identity_row_carries_the_live_name_and_a_rename_replaces_it() {
        let (s, _) = seeded(&[fx::recent_export(&[("t1", 0, 0), ("t2", 0, 0)])]);
        hold(&s, held::THREADS, "t1");
        hold(&s, held::THREADS, "t2");
        for (name, by_hand) in [("Tax letter", true), ("Tax letter, final", true), ("agent drift", false)] {
            let mut payload = json!({"summary": name, "ts": AT});
            if by_hand {
                payload["manual"] = json!(true);
            }
            s.write(|h| h.fold(&named_export("t1", json!({"summary_updated": payload}), Value::Null, None)));
            let row = s.read(|h| header_row(h, "t1"));
            assert_eq!(row["name"], json!(name), "the row did not follow the rename: {row}");
            assert_eq!(row.get("name_manual").is_some(), by_hand, "{row}");
        }
        s.write(|h| h.fold(&named_export("t2", json!({}), Value::Null, None)));
        let row = s.read(|h| header_row(h, "t2"));
        assert!(row.get("name").is_none(), "an unnamed thread was given a name: {row}");
    }

    /// **WHO STARTED IT: `app_id` AND AN AGENT'S TAG, AND `identity` ONLY ONCE
    /// THE SETTINGS CELL WAS READ** (TUI-P1a, TC-05). N = 2 attributed rows
    /// (his face; an agent's tagged child that copied his face's app id) and
    /// one un-attributed; and the settings blob itself still never reaches the
    /// row.
    #[tokio::test]
    async fn the_identity_row_says_who_started_the_thread_and_nothing_else_of_the_settings() {
        let (s, _) = seeded(&[fx::recent_export(&[("a", 0, 0), ("b", 0, 0), ("c", 0, 0)])]);
        for id in ["a", "b", "c"] {
            hold(&s, held::THREADS, id);
        }
        s.write(|h| {
            h.fold(&named_export("a", json!({}), json!({"app_id": "harness-tui", "cwd": "/secret/cwd"}), None));
            h.fold(&named_export("b", json!({}),
                json!({"app_id": "harness-tui", "metadata": {"agent": "agents_spawn:a"}}), None));
            h.fold(&named_export("c", json!({}), json!({"cwd": "/x"}), None));
        });
        let (a, b, c) = s.read(|h| (header_row(h, "a"), header_row(h, "b"), header_row(h, "c")));
        assert_eq!((a["identity"].clone(), a["app_id"].clone(), a.get("agent")), (json!(true), json!("harness-tui"), None));
        assert_eq!(b["agent"], json!("agents_spawn:a"), "{b}");
        assert_eq!(c["identity"], json!(true));
        assert!(c.get("app_id").is_none(), "an un-attributed thread was given an app: {c}");
        assert!(a.as_object().unwrap().values().all(|v| v != "/secret/cwd"), "the settings blob leaked: {a}");
        // Before the cell is read, no `identity` — "not told" is not "nobody".
        let (s2, _) = seeded(&[fx::recent_export(&[("d", 0, 0)])]);
        assert!(s2.read(|h| header_row(h, "d")).get("identity").is_none());
    }

    /// **THE FIRST LINE OF THE FIRST USER MESSAGE ON A PAGE** — skipping the
    /// non-user messages before it and blank lines inside it; `None` for a
    /// page with no message of a person's (opposes a constant answer).
    #[test]
    fn the_first_line_is_the_first_user_messages_first_non_blank_line() {
        let page = |msgs: Value| serde_json::to_vec(&json!({"thread_id": "t", "from": 0, "to": 3, "messages": msgs})).unwrap();
        assert_eq!(
            first_line_from_page(&page(json!([
                {"kind": "custom", "text": "summary_updated"},
                {"kind": "user_message", "text": "\n   fix the login page  \nthen the rest"},
                {"kind": "user_message", "text": "second message"},
            ]))).as_deref(),
            Some("fix the login page")
        );
        assert_eq!(
            first_line_from_page(&page(json!([{"kind": "user_message", "text": "why is the build red?"}]))).as_deref(),
            Some("why is the build red?")
        );
        assert_eq!(first_line_from_page(&page(json!([{"kind": "assistant_text", "text": "hello"}]))), None);
        assert_eq!(first_line_from_page(b"not json"), None);
    }

    /// **ONLY AN UNNAMED THREAD'S FIRST PAGE IS OWED A READ, AND ONLY ONCE PER
    /// PAGE ID.** A named thread's first line could never be drawn, so it is
    /// never fetched; a page already asked for is not asked again.
    #[tokio::test]
    async fn only_an_unnamed_threads_first_page_is_owed_a_read_and_only_once() {
        let (s, _) = seeded(&[fx::recent_export(&[("named", 0, 0), ("bare", 0, 0), ("bare2", 0, 0)])]);
        for id in ["named", "bare", "bare2"] {
            hold(&s, held::THREADS, id);
        }
        let pages = |id: &str| Some(json!([{"id": format!("{id}.m0-3"), "from": 0, "to": 3}]));
        s.write(|h| {
            h.fold(&named_export("named", json!({"summary_updated": {"summary": "Has a name"}}), Value::Null, pages("named")));
            h.fold(&named_export("bare", json!({}), Value::Null, pages("bare")));
            h.fold(&named_export("bare2", json!({}), Value::Null, pages("bare2")));
        });
        let tried = Tried::default();
        let mut owed = s.read(|h| first_pages_owed(h, &tried));
        owed.sort();
        assert_eq!(
            owed,
            vec![("bare".to_string(), "bare.m0-3".to_string()), ("bare2".to_string(), "bare2.m0-3".to_string())]
        );
        tried.lock().unwrap().insert("bare.m0-3".into());
        let owed = s.read(|h| first_pages_owed(h, &tried));
        assert_eq!(owed, vec![("bare2".to_string(), "bare2.m0-3".to_string())], "a page was owed twice");
    }

    // ── the link, and the last thing the ladder still decides ───────────────

    /// **`Invalidate` BEFORE `Connected`, AND THE RESYNC WORD IS THE
    /// `boot_id`.** Everything the ladder is still allowed to decide.
    #[tokio::test]
    async fn the_link_says_invalidate_then_connected_and_the_word_is_the_boot_id() {
        let (s, mut p) = seeded(&[]);
        let first = p.step(&s, None, Utc::now());
        match (&first[0], &first[1]) {
            (TransportEvent::Invalidate { .. }, TransportEvent::Connected { resync, boot_id }) => {
                assert_eq!(*resync, ResyncKind::First);
                assert_eq!(boot_id, "boot-1");
            }
            other => panic!("the link's first two events were {other:?}"),
        }
        // The SAME boot: a socket blip, and no repair behind it.
        s.write(|h| {
            h.link = held::LinkState::Down {
                reason: "dropped".into(),
            }
        });
        let _ = p.step(&s, None, Utc::now());
        s.write(|h| {
            h.link = held::LinkState::Up {
                boot_id: "boot-1".into(),
                since: AT.parse().unwrap(),
            }
        });
        let blip = p.step(&s, None, Utc::now());
        assert!(blip.iter().any(|e| matches!(
            e,
            TransportEvent::Connected {
                resync: ResyncKind::Blip,
                ..
            }
        )));
        // A DIFFERENT boot: a restart.
        s.write(|h| {
            h.link = held::LinkState::Up {
                boot_id: "boot-2".into(),
                since: AT.parse().unwrap(),
            }
        });
        let restart = p.step(&s, None, Utc::now());
        assert!(restart.iter().any(|e| matches!(
            e,
            TransportEvent::Connected {
                resync: ResyncKind::Restart,
                ..
            }
        )));
    }

    /// **A DROPPED LINK CLAIMS NO COUNTDOWN.** The retry ladder is the seam's
    /// and it does not publish the next attempt, so `next_retry_in_ms` is zero
    /// rather than a second copy of the ladder's arithmetic.
    #[tokio::test]
    async fn a_dropped_link_claims_no_countdown() {
        let (s, mut p) = seeded(&[]);
        let _ = p.step(&s, None, Utc::now());
        s.write(|h| {
            h.link = held::LinkState::Down {
                reason: "the socket dropped".into(),
            }
        });
        let out = p.step(&s, None, Utc::now());
        let down = out
            .iter()
            .find_map(|e| match e {
                TransportEvent::Disconnected {
                    reason,
                    next_retry_in_ms,
                } => Some((reason.clone(), *next_retry_in_ms)),
                _ => None,
            })
            .expect("a drop is reported");
        assert_eq!(down.0, "the socket dropped");
        assert_eq!(down.1, 0);
    }

    /// **A WAKE-UP WITH NO NEWS SAYS NOTHING.** The seam's events carry no
    /// facts, so a face woken for nothing must not be told the fleet moved —
    /// `TransportEvent::Fleet` is `activity_grew: true` in the store and would
    /// scroll a quiet screen.
    #[tokio::test]
    async fn a_wake_up_with_no_news_says_nothing() {
        let (s, mut p) = seeded(&[fx::recent_export(&[("t1", 0, 0)])]);
        let first = p.step(&s, None, Utc::now());
        assert!(!first.is_empty(), "the first step has the link to report");
        let second = p.step(&s, None, Utc::now());
        assert!(
            second.is_empty(),
            "a second wake-up with nothing changed produced {second:?}"
        );
    }

    // ── the three firehose-only channels ───────────────────────────────────

    /// **ALL THREE NARRATION CHANNELS BECOME NOTES, WITH THE FIREHOSE'S OWN
    /// TEXT AND LEVEL.**
    ///
    /// `progress`, `lifecycle` and `info` were firehose-ONLY by construction —
    /// each carries an empty thread id, so no per-thread filter could reach
    /// them. These three strings are byte-for-byte what `store.rs`'s own
    /// `ServerMsg::Progress`/`Lifecycle`/`Info` arms produced, which is the
    /// evidence this is a migration and not a redesign.
    #[tokio::test]
    async fn all_three_firehose_only_channels_arrive_as_the_notes_they_were() {
        let (s, mut p) = seeded(&[
            fx::narration_export(
                "progress",
                &[fx::progress_line(AT, "login", "probing", "two of four", false, true)],
            ),
            fx::narration_export(
                "lifecycle",
                &[fx::lifecycle_line(AT, "threads", "going_down", "self-update")],
            ),
            fx::narration_export("info", &[fx::info_line(AT, "accounts", "a login rotated")]),
        ]);
        let out = p.step(&s, None, Utc::now());
        let got = notes(&out);
        assert!(
            got.contains(&("login · probing · two of four".into(), NoteLevel::Calm)),
            "{got:?}"
        );
        assert!(
            got.contains(&("threads going_down (self-update)".into(), NoteLevel::Warn)),
            "{got:?}"
        );
        assert!(
            got.contains(&("accounts changed (a login rotated)".into(), NoteLevel::Calm)),
            "{got:?}"
        );
    }

    /// **A FAILED `progress` IS `Critical`, AND ONLY WHEN IT IS BOTH DONE AND
    /// NOT OK.** `store.rs`'s predicate exactly: a step that is still running
    /// and not yet ok is not a failure.
    #[tokio::test]
    async fn only_a_finished_and_failed_progress_line_is_critical() {
        for (done, ok, want) in [
            (true, false, NoteLevel::Critical),
            (true, true, NoteLevel::Calm),
            (false, false, NoteLevel::Calm),
        ] {
            let (s, mut p) = seeded(&[fx::narration_export(
                "progress",
                &[fx::progress_line(AT, "login", "p", "d", done, ok)],
            )]);
            let got = notes(&p.step(&s, None, Utc::now()));
            assert!(
                got.iter().any(|(_, l)| *l == want),
                "done={done} ok={ok} wanted {want:?}, got {got:?}"
            );
        }
    }

    /// **THE FIX LANDED UPSTREAM, AND THIS IS THE RED IT WAS WRITTEN TO BE.**
    ///
    /// What stood here was `the_seams_line_text_is_a_field_no_producer_sends`: it
    /// asserted `lines[0].text == ""` and said in its own doc that *"what belongs
    /// here is a red the day it lands, so this face stops working around
    /// something that was repaired."* It landed. `Line::text` is gone from the
    /// chassis's `view.rs` and `Line::kind` replaced it — the producer's own
    /// declared tag, `Required`, read off `raw["kind"]` — so the old assertion no
    /// longer compiles, which is exactly the signal it was left as.
    ///
    /// The claim is now the positive one: the field a face reads is the field the
    /// producer declares, and `view::narration` DROPS a line that has no `kind`
    /// rather than carrying it with an empty one. The second half of that is what
    /// the vacuity guard below is for — an empty `lines` would make every
    /// assertion here pass.
    #[tokio::test]
    async fn the_seams_line_carries_the_producers_own_kind() {
        let (s, _) = seeded(&[fx::narration_export(
            "info",
            &[fx::info_line(AT, "accounts", "a login rotated")],
        )]);
        let lines = s.read(|h| view::narration(h, "info", None).known().unwrap_or_default());
        assert_eq!(lines.len(), 1, "a kinded line must survive the filter_map");
        assert_eq!(
            lines[0].kind,
            seam::inventory::narration::lines::kind::INFO,
            "`Line::kind` is the producer's declared tag, not a word this face chose"
        );
        // And the fields the note IS built from are all on `raw`.
        assert_eq!(lines[0].raw["domain"], json!("accounts"));
        assert_eq!(lines[0].raw["reason"], json!("a login rotated"));

        // **AND A LINE WITH NO `kind` IS NOT A LINE.** The counter-case, because
        // `kind` being `Required` is only a fact if its absence costs something:
        // without this, a `filter_map` that silently kept kindless lines would
        // pass every assertion above.
        let (s2, _) = seeded(&[fx::narration_export(
            "info",
            &[json!({"at": AT, "domain": "accounts", "reason": "no kind here"})],
        )]);
        let dropped = s2.read(|h| view::narration(h, "info", None).known().unwrap_or_default());
        assert!(
            dropped.is_empty(),
            "a narration line with no `kind` must be dropped, not carried with an \
             empty one — got {dropped:?}"
        );
    }

    /// A narration line already told is not told again, including a second line
    /// sharing its timestamp — `since` is strictly-greater, so the same-stamp
    /// case needs its own memory.
    #[tokio::test]
    async fn a_narration_line_is_told_once_even_when_two_share_a_stamp() {
        let (s, mut p) = seeded(&[fx::narration_export(
            "info",
            &[
                fx::info_line(AT, "accounts", "first"),
                fx::info_line(AT, "budgets", "second"),
            ],
        )]);
        assert_eq!(notes(&p.step(&s, None, Utc::now())).len(), 2);
        assert_eq!(notes(&p.step(&s, None, Utc::now())).len(), 0);
        // A THIRD at the same stamp is still news.
        s.write(|h| {
            h.fold(&fx::narration_export(
                "info",
                &[
                    fx::info_line(AT, "accounts", "first"),
                    fx::info_line(AT, "budgets", "second"),
                    fx::info_line(AT, "gauge", "third"),
                ],
            ));
        });
        let third = notes(&p.step(&s, None, Utc::now()));
        assert_eq!(third.len(), 1, "{third:?}");
        assert_eq!(third[0].0, "gauge changed (third)");
    }

    // ── the open thread's rows ──────────────────────────────────────────────

    /// **A LOG ROW BECOMES THE `event` FRAME IT USED TO ARRIVE AS, ONCE.**
    /// `seam::ledger::claim` is the whole of the deduplication and it replaces
    /// `transport::Cursors` outright.
    #[tokio::test]
    async fn a_log_row_becomes_the_frame_it_was_and_is_delivered_exactly_once() {
        let (s, mut p) = seeded(&[fx::log_export(
            "t1",
            vec![
                fx::log_item(1, "assistant_text", AT, json!({"text": "one"})),
                fx::log_item(2, "assistant_text", AT, json!({"text": "two"})),
            ],
        )]);
        hold(&s, held::LOG, "t1");
        let first = p.step(&s, None, Utc::now());
        assert_eq!(frames(&first), vec![("t1".into(), 1), ("t1".into(), 2)]);
        // Nothing new: nothing delivered. The window still holds both rows, so
        // without `claim` this would repeat them on every wake-up.
        assert_eq!(frames(&p.step(&s, None, Utc::now())), Vec::new());
        // One appended row, and only it.
        s.write(|h| {
            h.fold(&fx::log_transition(
                "t1",
                fx::log_item(3, "assistant_text", AT, json!({"text": "three"})),
            ));
        });
        assert_eq!(
            frames(&p.step(&s, None, Utc::now())),
            vec![("t1".into(), 3)]
        );
    }

    /// **THE CLAIM FLOOR IS THE PRODUCER'S OWN `seen_seq`**, so a fresh process
    /// does not re-deliver what the operator has already read — with no state of
    /// its own to persist and nothing to seed from a query. The old transport
    /// kept that cursor on disk.
    #[tokio::test]
    async fn the_floor_is_the_producers_seen_seq_and_not_anything_this_face_kept() {
        let (s, mut p) = seeded(&[
            fx::recent_export(&[("t1", 0, 0)]),
            fx::log_export(
                "t1",
                vec![
                    fx::log_item(1, "assistant_text", AT, json!({"text": "old"})),
                    fx::log_item(2, "assistant_text", AT, json!({"text": "old"})),
                    fx::log_item(3, "assistant_text", AT, json!({"text": "new"})),
                ],
            ),
        ]);
        hold(&s, held::LOG, "t1");
        hold(&s, held::THREADS, "t1");
        // The producer says he has seen through 2.
        s.write(|h| {
            h.fold(&fx::threads_export(
                "t1",
                "idle",
                false,
                2,
                3,
                Value::Null,
                None,
            ));
        });
        assert_eq!(s.read(|h| ledger::floor(h, "t1")), Some(2));
        assert_eq!(
            frames(&p.step(&s, None, Utc::now())),
            vec![("t1".into(), 3)],
            "rows at or below the producer's cursor were handed over again"
        );
    }

    /// **THE TRANSCRIPT IS THE WINDOW, AND AN UNHYDRATED KEY IS AN ERROR AND
    /// NOT AN EMPTY THREAD.** `store.rs` draws the two differently, and
    /// *this face holds no hydrated mirror for the key* is not *there is
    /// nothing here*.
    ///
    /// **The attribution is deliberately absent from both.** The guard reads
    /// local state only, so it cannot tell an unanswered subscription from an
    /// unanswered producer, and the assertion below pins that it does not
    /// guess. R2.S2b forced that assertion red by restoring the old wording.
    #[tokio::test]
    async fn an_unhydrated_log_is_an_error_and_a_hydrated_one_is_rows() {
        let (s, _) = seeded(&[]);
        let h = Handle { seam: s.clone(), bodies: Bodies::default() };
        let err = transcript(&h, "t1").expect_err("not hydrated");
        assert!(err.contains("threads/log"), "{err}");
        // **AND IT DOES NOT BLAME A PARTY IT CANNOT OBSERVE.** The guard that
        // produced this reads the face's own mirror twice and sends nothing, so
        // a refusal naming the producer is a claim the instrument cannot make.
        // Asserted on the RUNTIME string, not on this file's source, so the
        // probe is not witnessing itself; forced red in R2.S2b by restoring the
        // old wording, which fires on `producer`.
        assert!(
            !err.to_lowercase().contains("producer"),
            "the unhydrated-`threads/log` refusal attributes the condition to \
             the producer. This code knows only that its OWN mirror has no \
             hydrated entry for the key -- it never asked. {err}"
        );
        s.write(|g| {
            g.fold(&fx::log_export(
                "t1",
                vec![fx::log_item(9, "assistant_text", AT, json!({"text": "hi"}))],
            ));
        });
        let (rows, from) = transcript(&h, "t1").expect("hydrated");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].seq, 9);
        assert_eq!(from, 9, "the window's own floor, not a page this face chose");
        // **BYTE-IDENTICAL TO WHAT THE FIREHOSE DELIVERED**, which is what keeps
        // every `store.rs` classifier unchanged.
        // `ThreadEvent` is not `PartialEq` — it is the crate's TOLERANT mirror
        // of the wire and its unknown arm carries a raw `Value` — so the
        // comparison is on its `kind()` plus what the row's own payload said,
        // which is what `store.rs` branches on.
        assert_eq!(rows[0].event.kind(), "assistant_text");
        assert!(
            !matches!(rows[0].event, ThreadEvent::Unknown { .. }),
            "a `threads/log` row parsed as an unknown kind, so this face would \
             render it as a hole where the firehose rendered a message"
        );
    }

    // ── capped ──────────────────────────────────────────────────────────────

    /// **THE VENDORED `view::summary_truncated` CANNOT ANSWER, AND THIS IS THE
    /// PROOF PLUS THE RED FOR THE DAY IT IS FIXED.**
    ///
    /// It calls `Value::as_bool` on a value the producer sends as an OBJECT. See
    /// [`super::truncated`] for the whole finding. Two assertions, opposed: the
    /// view's reader is NOT `Known` for a frame that definitely carries the
    /// scalar, and this face's own reader IS.
    #[tokio::test]
    async fn the_shared_truncated_cell_is_an_object_and_the_views_reader_cannot_see_it() {
        let (s, _) = seeded(&[fx::summary_truncated_export(true)]);
        // The raw value, so the frame is not in doubt.
        assert_eq!(
            s.read(|h| h
                .mirror(held::SUMMARY)
                .expect("the summary mirror")
                .shared()
                .get("truncated")
                .cloned()),
            Some(json!({"truncated": true})),
        );
        assert!(
            !s.read(|h| view::summary_truncated(h).is_known()),
            "`view::summary_truncated` now answers — the upstream fix landed, so \
             `mirror::truncated` is a duplicate and must be deleted rather than \
             kept beside it"
        );
        assert_eq!(s.read(truncated), Some(true));
    }

    /// **`capped` IS THE PRODUCER'S OWN `truncated`, NOT A PAGE SIZE THIS FACE
    /// CHOSE.** A page of 1,000 rendered as a fleet size is honesty rule 1 with
    /// a number on it, which is why the count was retired in favour of naming
    /// the window.
    #[tokio::test]
    async fn capped_is_the_producers_truncated_and_never_a_page_size() {
        let (s, mut p) = seeded(&[fx::recent_export(&[("t1", 0, 0)])]);
        assert_eq!(fleet(&p.step(&s, None, Utc::now())).map(|(_, c)| c), Some(false));
        // The producer says its summary covers fewer than all threads.
        s.write(|h| {
            h.fold(&fx::summary_truncated_export(true));
        });
        let out = p.step(&s, None, Utc::now());
        assert_eq!(
            fleet(&out).map(|(_, c)| c),
            Some(true),
            "the producer's `truncated` did not reach the face: {out:?}"
        );
    }

    // ── the carve-out ───────────────────────────────────────────────────────

    /// **THE ENVELOPE DOOR TAKES ONLY THE TWO.** A third leaf is a panic in a
    /// test rather than a reply key nobody declared.
    ///
    /// **The `expected` string below is coupled to the `assert!` message in
    /// [`Handle::enveloped_queue_read`] by string equality and by nothing else.**
    /// Changing one without the other is a red, and R2.S2b forced exactly that
    /// before trusting this test.
    #[tokio::test]
    #[should_panic(expected = "is not one of the two enveloped queue reads")]
    async fn a_third_queue_read_cannot_go_through_the_envelope() {
        let (s, _) = seeded(&[]);
        let h = Handle { seam: s, bodies: Bodies::default() };
        // The link is down, so this would refuse anyway — the panic is the
        // assertion and it fires before any socket is touched.
        let _ = h.enveloped_queue_read("history", "t1").await;
    }

    // ── the port lever ──────────────────────────────────────────────────────

    /// **ONE HALF OF THE LEVER IS REFUSED, AND BOTH-OR-NEITHER IS FINE.**
    #[test]
    fn the_legacy_port_alone_is_refused_and_says_which_door_it_is() {
        let _g = TEST_PORTS.lock().unwrap_or_else(|e| e.into_inner());
        let restore = (
            std::env::var("TUI_THREADS_PORT").ok(),
            std::env::var("TUI_THREADS_CHASSIS_PORT").ok(),
        );
        std::env::set_var("TUI_THREADS_PORT", "4000");
        std::env::remove_var("TUI_THREADS_CHASSIS_PORT");
        let err = check_ports().expect_err("one half alone is refused");
        assert!(err.contains("TUI_THREADS_CHASSIS_PORT"), "{err}");
        assert!(
            err.contains(&link::DEFAULT_CHASSIS_PORT.to_string()),
            "the refusal must say WHERE the frames would go instead: {err}"
        );
        // Both: fine.
        std::env::set_var("TUI_THREADS_CHASSIS_PORT", "4001");
        assert!(check_ports().is_ok());
        // Neither: fine — that is the operator's ordinary run.
        std::env::remove_var("TUI_THREADS_PORT");
        std::env::remove_var("TUI_THREADS_CHASSIS_PORT");
        assert!(check_ports().is_ok());
        // Only the chassis port: fine, and it is the useful case.
        std::env::set_var("TUI_THREADS_CHASSIS_PORT", "4001");
        assert!(check_ports().is_ok());
        for (k, v) in [
            ("TUI_THREADS_PORT", restore.0),
            ("TUI_THREADS_CHASSIS_PORT", restore.1),
        ] {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    // ── elided bodies: the empty echo and the vanishing reply ───────────────

    /// A body over the producer's 1 KiB inline bound — the size at which it
    /// lifts a message's `text` behind the resource door (`Payload::Split`).
    fn long(word: &str) -> String {
        format!("{word} ").repeat(1 + 1100 / (word.len() + 1))
    }

    fn sse(kind: &str, body: Value) -> Value {
        let mut ev = body;
        ev["type"] = json!(kind);
        json!({"raw": {"event": ev}})
    }

    /// A resource door that serves exactly `served`, and refuses anything else.
    fn door(
        served: Vec<(&'static str, String)>,
    ) -> impl Fn(String) -> std::future::Ready<Result<Vec<u8>, String>> {
        move |id: String| {
            std::future::ready(
                served
                    .iter()
                    .find(|(k, _)| *k == id)
                    .map(|(_, v)| v.clone().into_bytes())
                    .ok_or_else(|| format!("unknown: no resource {id}")),
            )
        }
    }

    fn apply_all(store: &mut crate::store::Store, evs: Vec<TransportEvent>) {
        for e in evs {
            store.apply(e, Utc::now());
        }
    }

    fn said(store: &crate::store::Store) -> Vec<(bool, String)> {
        crate::store::thread_blocks(
            store,
            false,
            false,
            &Default::default(),
            &[],
            &tui_render::TreeId::Parent,
            None,
        )
        .into_iter()
        .filter(|b| b.is_message())
        .map(|b| {
            let user = matches!(
                &b,
                tui_render::Block::Message {
                    sender: tui_render::model::Sender::User,
                    ..
                }
            );
            (user, b.text().to_string())
        })
        .collect()
    }

    /// **THE VANISHING REPLY.** The reply assembles live from its deltas, so it
    /// is on screen; then the authoritative `assistant_text` lands, which
    /// RETIRES the live box — and its words are behind the resource door. A
    /// face that renders the row without fetching them replaces a box full of
    /// words with an empty one: *appears for a second and then disappears*.
    #[tokio::test]
    async fn a_reply_whose_words_are_behind_the_door_does_not_vanish_when_it_lands() {
        let body = long("reply");
        let (s, mut p) = seeded(&[fx::log_export(
            "t1",
            vec![
                fx::log_item(
                    1,
                    "stream_event",
                    AT,
                    sse("content_block_start", json!({"index":0,"content_block":{"type":"text"}})),
                ),
                fx::log_item(
                    2,
                    "stream_event",
                    AT,
                    sse(
                        "content_block_delta",
                        json!({"index":0,"delta":{"type":"text_delta","text":"the live words"}}),
                    ),
                ),
            ],
        )]);
        hold(&s, held::LOG, "t1");
        let bodies = Bodies::default();
        let fetch = door(vec![("t1.3", body.clone())]);
        let mut store = crate::store::Store::new("ws://x".into(), "/h".into());
        store.open("t1");

        apply_all(&mut store, deliver(&mut p, &s, None, Utc::now(), &bodies, &fetch).await);
        assert_eq!(said(&store), vec![(false, "the live words".to_string())]);

        s.write(|g| {
            g.fold(&fx::log_transition(
                "t1",
                fx::log_item_split(3, "assistant_text", AT, json!({}), "t1.3", body.len() as u64),
            ));
        });
        apply_all(&mut store, deliver(&mut p, &s, None, Utc::now(), &bodies, &fetch).await);
        assert_eq!(
            said(&store),
            vec![(false, body.clone())],
            "the reply's box lost its words when the final row landed"
        );
    }

    /// **THE EMPTY ECHO.** His own message, over 1 KiB, arrives with its
    /// `text` lifted out; the row must carry the words the daemon stored.
    #[tokio::test]
    async fn a_long_user_message_is_drawn_with_its_words_not_as_an_empty_row() {
        let body = long("mine");
        let (s, mut p) = seeded(&[fx::log_export(
            "t1",
            vec![fx::log_item_split(
                7,
                "user_message",
                AT,
                json!({"intent": "immediate"}),
                "t1.7",
                body.len() as u64,
            )],
        )]);
        hold(&s, held::LOG, "t1");
        let mut store = crate::store::Store::new("ws://x".into(), "/h".into());
        store.open("t1");
        let fetch = door(vec![("t1.7", body.clone())]);
        apply_all(
            &mut store,
            deliver(&mut p, &s, None, Utc::now(), &Bodies::default(), &fetch).await,
        );
        assert_eq!(said(&store), vec![(true, body.clone())]);
    }

    /// **THE SAME TWO, ON OPEN.** The transcript load carries the window, and
    /// an elided row in it is filled before the store sees it — including a
    /// row whose WHOLE event is behind the door (`there`).
    #[tokio::test]
    async fn the_transcript_load_fills_elided_rows() {
        let body = long("opened");
        let (s, _) = seeded(&[fx::log_export(
            "t1",
            vec![
                fx::log_item_split(4, "user_message", AT, json!({}), "t1.4", body.len() as u64),
                json!({"seq": 5, "kind": "assistant_text", "ts": AT,
                       "payload": {"there": {"id": "t1.5", "bytes": 10, "transports": ["ws"]}}}),
            ],
        )]);
        hold(&s, held::LOG, "t1");
        let h = Handle { seam: s.clone(), bodies: Bodies::default() };
        let whole = json!({"kind": "assistant_text", "text": "a whole event"}).to_string();
        let fetch = door(vec![("t1.4", body.clone()), ("t1.5", whole)]);
        let (rows, _) = read_transcript(&h, "t1", std::time::Duration::ZERO, &fetch)
            .await
            .expect("hydrated");
        let texts: Vec<_> = rows.iter().map(|r| r.event.body_text().map(str::to_string)).collect();
        assert_eq!(
            texts,
            vec![Some(body.clone()), Some("a whole event".to_string())]
        );
    }

    /// **A WAKE-UP WITH NO NEWS IS CHEAP, HOWEVER LONG THE OPEN WINDOWS ARE**
    /// (THREAD-LOAD-LATENCY F5 — the idle-CPU budget). Every heartbeat, every
    /// `threads` move and every other thread's row wakes the projection; it
    /// used to deep-copy every open `threads/log` window (up to 8 x 4,000 rows)
    /// twice per wake to find the zero new rows. Sampled on his live face that
    /// copy-and-drop was most of its CPU while nothing on screen changed.
    ///
    /// Eight full windows; the first step claims every row (and must return
    /// them all, in order); then 200 empty wakes must each cost well under a
    /// millisecond. Prints the per-wake cost for the doc.
    #[tokio::test]
    async fn a_wake_with_no_news_does_not_copy_the_open_windows() {
        let threads: Vec<String> = (0..8).map(|i| format!("t{i}")).collect();
        let frames: Vec<Value> = threads
            .iter()
            .map(|t| {
                fx::log_export(
                    t,
                    (1..=4_000u64)
                        .map(|seq| fx::log_item(seq, "assistant_text", AT, json!({"text": format!("row {seq} of {t} with some words in it")})))
                        .collect(),
                )
            })
            .collect();
        let (s, mut p) = seeded(&frames);
        for t in &threads {
            hold(&s, held::LOG, t);
        }
        let first = p.step(&s, None, Utc::now());
        let rows = first.iter().filter(|e| matches!(e, TransportEvent::Frame { .. })).count();
        assert_eq!(rows, 8 * 4_000, "the first wake must hand over every row once");
        let t0 = std::time::Instant::now();
        const WAKES: u32 = 200;
        for _ in 0..WAKES {
            let out = p.step(&s, None, Utc::now());
            assert!(!out.iter().any(|e| matches!(e, TransportEvent::Frame { .. })), "a row was handed over twice");
        }
        let per_wake = t0.elapsed() / WAKES;
        eprintln!("IDLE-WAKE per_wake_us={}", per_wake.as_micros());
        // Copying the windows measured ~6-10 ms a wake (release); the borrowing
        // read is tens of microseconds. 2 ms leaves room for a loaded machine.
        assert!(
            per_wake < std::time::Duration::from_millis(2),
            "an empty wake cost {per_wake:?} with eight 4,000-row windows open"
        );
    }

    /// **AN OPENED THREAD IS DRAWN BEFORE ITS ELIDED BODIES ARRIVE, AND THEN
    /// FILLED NEWEST FIRST** (THREAD-LOAD-LATENCY F7). The first hand-over
    /// carries every row, each elided one saying its words are loading (never
    /// empty); the bodies follow in `BODY_CHUNK`s, the newest chunk first, and
    /// together they complete every pending row exactly once. A body the cache
    /// already holds is never shown as loading.
    #[tokio::test]
    async fn an_opened_thread_is_drawn_first_and_its_bodies_fill_in_newest_first() {
        const N: u64 = 60;
        let items: Vec<Value> = (1..=N)
            .map(|seq| fx::log_item_split(seq, "user_message", AT, json!({}), &format!("t1.{seq}"), 2_000))
            .collect();
        let (s, _) = seeded(&[fx::log_export("t1", items)]);
        hold(&s, held::LOG, "t1");
        let h = Handle { seam: s.clone(), bodies: Bodies::default() };
        // One body is already cached: it must be drawn complete at once.
        h.bodies
            .0
            .lock()
            .unwrap()
            .insert(("t1".into(), 7), json!({"kind": "user_message", "text": "cached 7"}));
        let fetch = |id: String| async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            Ok::<Vec<u8>, String>(format!("words of {id}").into_bytes())
        };
        let (rows, _, pending) = open_transcript(&h, "t1", std::time::Duration::ZERO).await.expect("hydrated");
        assert_eq!(rows.len(), N as usize);
        assert_eq!(pending.len(), N as usize - 1, "the cached body was left pending");
        assert_eq!(pending.first().map(|p| p.0), Some(N), "pending is not newest first");
        for r in &rows {
            let t = r.event.body_text().unwrap_or_default().to_string();
            if r.seq == 7 {
                assert_eq!(t, "cached 7");
            } else {
                assert!(t.contains("loading"), "row {} drawn as {t:?} before its body arrived", r.seq);
            }
        }
        let (tx, mut rx) = mpsc::channel::<Ev>(64);
        fill_bodies(&h, "t1", rows, pending, &fetch, &tx).await;
        drop(tx);
        let mut filled = Vec::new();
        let mut first_chunk_max = None;
        while let Some(ev) = rx.recv().await {
            let Ev::TranscriptBodies { thread_id, rows } = ev else { panic!("unexpected event") };
            assert_eq!(thread_id, "t1");
            assert!(rows.len() <= BODY_CHUNK);
            first_chunk_max.get_or_insert(rows.iter().map(|r| r.seq).max().unwrap_or(0));
            for r in rows {
                assert_eq!(r.event.body_text(), Some(format!("words of t1.{}", r.seq).as_str()));
                filled.push(r.seq);
            }
        }
        filled.sort_unstable();
        let want: Vec<u64> = (1..=N).filter(|s| *s != 7).collect();
        assert_eq!(filled, want, "every pending row exactly once");
        assert_eq!(first_chunk_max, Some(N), "the newest rows were not filled first");
    }

    /// **ELIDED BODIES ARE FETCHED CONCURRENTLY, AND LAND IN ROW ORDER**
    /// (THREAD-LOAD-LATENCY F4). Forty elided rows behind a door that takes
    /// 20 ms each: one at a time is 800 ms; bounded concurrency is a few
    /// round trips. The door counts how many requests are in flight at once,
    /// so the test proves overlap (> 1) and the bound (<= the constant), and
    /// every row still carries its own words.
    #[tokio::test]
    async fn the_transcript_load_fetches_elided_bodies_concurrently_and_in_order() {
        const N: u64 = 40;
        let items: Vec<Value> = (1..=N)
            .map(|seq| fx::log_item_split(seq, "user_message", AT, json!({}), &format!("t1.{seq}"), 2_000))
            .collect();
        let (s, _) = seeded(&[fx::log_export("t1", items)]);
        hold(&s, held::LOG, "t1");
        let h = Handle { seam: s.clone(), bodies: Bodies::default() };
        let inflight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fetch = {
            let (inflight, peak) = (inflight.clone(), peak.clone());
            move |id: String| {
                let (inflight, peak) = (inflight.clone(), peak.clone());
                async move {
                    use std::sync::atomic::Ordering::SeqCst;
                    let now = inflight.fetch_add(1, SeqCst) + 1;
                    peak.fetch_max(now, SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    inflight.fetch_sub(1, SeqCst);
                    Ok::<Vec<u8>, String>(format!("words of {id}").into_bytes())
                }
            }
        };
        let t0 = std::time::Instant::now();
        let (rows, _) = read_transcript(&h, "t1", std::time::Duration::ZERO, &fetch)
            .await
            .expect("hydrated");
        let took = t0.elapsed();
        let texts: Vec<_> = rows.iter().map(|r| r.event.body_text().map(str::to_string)).collect();
        let want: Vec<_> = (1..=N).map(|seq| Some(format!("words of t1.{seq}"))).collect();
        assert_eq!(texts, want, "a body landed on the wrong row");
        let peak = peak.load(std::sync::atomic::Ordering::SeqCst);
        assert!(peak > 1, "bodies were fetched one at a time (peak in flight {peak})");
        assert!(peak <= BODY_FETCH_CONCURRENCY, "the bound was exceeded: {peak}");
        assert!(
            took < std::time::Duration::from_millis(20 * N / 2),
            "{N} bodies took {took:?}; serial would be {} ms",
            20 * N
        );
    }

    /// **OPENING A THREAD WAITS FOR ITS WINDOW**, rather than reading the
    /// mirror the instant after the subscribe left and reporting it empty.
    /// That race is what put *"could not load the thread"* on screen on most
    /// opens while the rows arrived a moment later anyway.
    #[tokio::test]
    async fn opening_a_thread_waits_for_its_window_to_land() {
        let (s, _) = seeded(&[]);
        hold(&s, held::LOG, "t1");
        let h = Handle { seam: s.clone(), bodies: Bodies::default() };
        let later = s.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            later.write(|g| {
                g.fold(&fx::log_export(
                    "t1",
                    vec![fx::log_item(9, "assistant_text", AT, json!({"text": "hi"}))],
                ));
            });
        });
        let got = read_transcript(&h, "t1", std::time::Duration::from_secs(5), door(vec![])).await;
        assert!(got.is_ok(), "{got:?}");
    }
}
