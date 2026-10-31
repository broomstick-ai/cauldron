//! # `threads` as a chassis [`Producer`]
//!
//! The network API moved into `shapewire` (operator ruling, 2026-09-26;
//! `docs/shapes.md` 6.2's annotation, `docs/chassis-writes.md` §10). This module
//! is what is left behind on this side of that move: the things the chassis
//! genuinely cannot know, and nothing else.
//!
//! 1. **The METHODS.** [`ThreadsProducer::handle`] is `server::handle` behind
//!    the trait, with `WS_METHODS` behind [`Producer::names`]. The chassis
//!    answers its own (`shapes/list`, `shapes/{shape}/subscribe`,
//!    `shapes/{shape}/unsubscribe`, `durable/barrier`, …) before this is ever
//!    called.
//! 2. **The resource door** ([`ThreadsProducer::resources`]) and the
//!    durability barrier ([`ThreadsProducer::durable`]).
//!
//! ## What is NOT here any more (R2.S4, 2026-10-10)
//!
//! This module used to carry the LEGACY FIREHOSE on the chassis door too: a
//! `ThreadsStream` extension arm (the subscription filter, the per-thread
//! replay cursors behind `Lagged`) and a `ThreadsControl` that answered
//! `{"type":"subscribe"}` / `{"type":"unsubscribe"}` and replayed the boot note
//! to a firehose subscriber once per connection. Every consumer reads those
//! facts as shapes now (`narration`, `boot`, `threads/log`), so both extension
//! points are the chassis's empty defaults: [`NoStream`] sends nothing of its
//! own, and [`NoControl`] disowns every frame type, which the chassis answers
//! with a `malformed` refusal (`invalid frame: unknown type \`subscribe\``).

use std::sync::Arc;

use shapewire::envelope::{Identity, Refusal};
use shapewire::server::{NoControl, NoStream, Producer};
use shapewire::wire::ConnId;
use serde_json::Value;

use crate::protocol::PROTO;
use crate::runtime::ThreadManager;
use crate::server::{DAEMON_NAME, WS_METHODS};

type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// This daemon, as the chassis sees it.
pub struct ThreadsProducer {
    manager: Arc<ThreadManager>,
    identity: Identity,
    /// Resolved ONCE, at construction, exactly as `drive` resolved it once per
    /// connection: `Config::effective_secret` reads an env var, and a secret
    /// that could change between two connections of one process would make
    /// `auth_required` on the `Welcome` a claim the next frame could contradict.
    secret: Option<String>,
}

impl ThreadsProducer {
    pub fn new(manager: Arc<ThreadManager>) -> Self {
        let secret = manager.config.effective_secret();
        let identity = Identity {
            daemon: DAEMON_NAME.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            proto: PROTO,
            boot_id: manager.boot_id.clone(),
        };
        Self {
            manager,
            identity,
            secret,
        }
    }

    pub fn manager(&self) -> &Arc<ThreadManager> {
        &self.manager
    }

    /// **ONE MESSAGE PAGE** — every event with a message id in `[from, to]`,
    /// spelled as `threads/{id}/query` spelled a message. See
    /// `crate::shapes::pages` for the id, the bytes and the retention.
    fn fetch_page(
        &self,
        id: &str,
        thread_id: &str,
        from: u64,
        to: u64,
    ) -> Result<shapewire::resource::Found, Refusal> {
        use crate::shapes::pages;
        use shapewire::resource::{Found, Retention};
        let err = |e: crate::Error| {
            Refusal::new(
                shapewire::envelope::codes::ERROR,
                format!("reading the page behind {id}: {e}"),
            )
        };
        // **A RANGE PAST THE NEWEST MESSAGE WAS NEVER PUBLISHED.** The `pages`
        // cell is cut at `last_message_id`, which is this same `max_message_id`,
        // so an id whose `to` is above it is one no shape issued — `unknown`,
        // as `body::unknown` answers for a body this store does not hold.
        let newest = self.manager.store.max_message_id(thread_id).map_err(err)?;
        if newest == 0 || to > newest {
            return Err(Refusal::new(
                shapewire::envelope::codes::UNKNOWN,
                format!(
                    "no message page is held under {id}. A page id is read off the \
                     `threads` shape's `pages` cell and never built"
                ),
            ));
        }
        let q = crate::store::MessageQuery {
            ids: None,
            from: Some(from),
            to: Some(to),
            tail: None,
            kinds: None,
            cursor: None,
            limit: pages::PAGE_MESSAGES as usize,
        };
        let got = self.manager.store.query_messages(thread_id, &q).map_err(err)?;
        let messages: Vec<serde_json::Value> = got
            .events
            .iter()
            .map(|e| crate::server::cited_message_json(e, true, 0))
            .collect();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "thread_id": thread_id,
            "from": from,
            "to": to,
            "messages": messages,
        }))
        .unwrap_or_default();
        let len = bytes.len() as u64;
        // FULL pages are immutable; the OPEN page is re-named by the next
        // message, which is what `Latest` means.
        let retention = if pages::is_full(from, to) {
            Retention::Fixed
        } else {
            Retention::Latest
        };
        Ok(Found::whole(
            retention,
            pages::PAGE_CONTENT_TYPE,
            len,
            Box::new(std::io::Cursor::new(bytes)),
        ))
    }
}

impl ThreadsProducer {
    /// R9.C7g: does this request need what boot recovery is rebuilding?
    /// [`crate::recovery::needs_recovery`], plus one case only the store can
    /// decide: the history of a thread it has no row for yet — a child
    /// recovery has not materialized — would answer `[]`, which is not true,
    /// so it waits like any other (r37 inspect finding a).
    fn needs_recovery(&self, method: &str, params: &Value) -> bool {
        if crate::recovery::needs_recovery(method) {
            return true;
        }
        if crate::recovery::is_thread_history(method) {
            let Some(id) = params.get("thread_id").and_then(|v| v.as_str()) else { return false };
            return !matches!(self.manager.store.get_thread(id), Ok(Some(_)));
        }
        false
    }
}

impl Producer for ThreadsProducer {
    type Input = crate::shape::Input;
    type Stream = NoStream;
    type Control = NoControl;

    fn identity(&self) -> &Identity {
        &self.identity
    }

    fn hub(&self) -> &shapewire::hub::Hub<Self::Input> {
        self.manager.hub.core()
    }

    fn secret(&self) -> Option<String> {
        self.secret.clone()
    }

    fn names(&self) -> &'static [&'static str] {
        WS_METHODS
    }

    /// R9.C7: `/health` carries the update gate's counters under
    /// `restart_gate`, so cauldron can wait for a quiet moment before a re-pin
    /// restart. `/health` is served by the chassis and never enters `handle`,
    /// so reading it takes no `RequestPermit` and cannot make itself busy.
    fn health_extras(&self) -> std::collections::BTreeMap<String, Value> {
        self.manager.update.health_extras()
    }

    fn handle<'a>(&'a self, method: &'a str, params: Value) -> BoxFut<'a, Result<Value, Refusal>> {
        Box::pin(async move {
            // THE IN-FLIGHT-REQUEST HALF OF THE QUIET CONDITION (intent item
            // 64): the permit is held for the whole life of the request, so "no
            // in-flight WS request being served" is a counter the update gate
            // reads rather than something anybody has to remember to check.
            // It stays HERE and not in the chassis: an update gate is a
            // property of this daemon's deployment, not of the API.
            let _serving = self.manager.update.enter_request();
            // R9.C7g: the daemon serves while boot recovery runs behind it. A
            // method that needs what recovery is rebuilding waits for it
            // (bounded) and is otherwise refused `busy`; it is never answered
            // from half-recovered state. See `crate::recovery`.
            let rec = self.manager.update.recovery();
            if rec.is_recovering() && self.needs_recovery(method, &params) && !rec.wait_done().await {
                return Err(rec.refusal(method));
            }
            crate::server::handle(&self.manager, method, params)
                .await
                // `Error` has no `Coded` impl, so every producer refusal carries
                // `code: "error"` — which is today's behaviour spelled honestly
                // rather than a silent default. The paths that NEEDED a type —
                // the shape refusals — are chassis-owned now and typed there.
                .map_err(shapewire::server::refuse)
        })
    }

    /// **RULE 2.7's THIRD STAGE, DECLARED BY ASKING THE STORE** rather than by
    /// this module believing something about it.
    ///
    /// `None` when the store promises nothing — which is every in-memory store,
    /// so the test fleet refuses the barrier by construction and the refusal
    /// path is the one most of the suite exercises. `Some` is answered by
    /// [`Store::durability_barrier`], and the capability is re-read on every
    /// call: a cached `true` would survive a store that had stopped syncing.
    fn durable(&self) -> Option<&dyn shapewire::command::Durable> {
        match self.manager.store.durability() {
            crate::store::Durability::None { .. } => None,
            crate::store::Durability::PerCommit { .. } | crate::store::Durability::AtBarrier => {
                Some(self)
            }
        }
    }

    /// **NO EXTENSION ARM AND NO CONTROL FRAMES.** Until R2.S4 this subscribed
    /// a receiver to the legacy firehose here, at connect and before the
    /// welcome, and handed back the control half that answered `subscribe`.
    /// Both are gone with the firehose; see the module doc.
    fn connect(&self, _conn: ConnId) -> (NoStream, NoControl) {
        (NoStream, NoControl)
    }

    /// **THE FIRST RESOURCE DOOR ON THIS PRODUCER.** Before this, every id on
    /// every door answered `501 unsupported` and `docs/shape-design-guide.md`
    /// recorded it as *"unbuilt on this producer"*.
    fn resources(&self) -> Option<&dyn shapewire::resource::Resources> {
        Some(self)
    }

    /// **THE MCP DOOR'S PRODUCER HALF** (R4.S1): the `agents` façade, below.
    fn mcp(&self) -> Option<&dyn shapewire::mcp::McpTools> {
        Some(self)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The MCP door — the `agents` façade, and the token's validity window
// ─────────────────────────────────────────────────────────────────────────────

/// **WHAT THIS PRODUCER HANDS THE MCP DOOR.** Three things the chassis cannot
/// know: the key, the validity window, and the tools.
///
/// The façade itself — tool → one method path → `handle` → the acknowledgement
/// guard — is [`crate::agents::call`], and it goes through the SAME `handle`
/// the `/ws` door reaches, holding the same request permit the update gate
/// counts (`enter_request`). So "no WS request is being served" and "no MCP
/// call is being served" are one counter, and the restart gate's quiet
/// condition covers both doors with no second thing to remember. §2.4's
/// bounded `agents_gather` (spelling (c)) is what keeps that permit short: no
/// call outlives one `poll_s`.
impl shapewire::mcp::McpTools for ThreadsProducer {
    /// **THE SIGNATURE, THEN THE WINDOW.** The chassis's key verifies the
    /// token; this producer then asks its own store whether the turn the
    /// token was minted for is the one running — `running_turns` for the
    /// thread, with the same spawn cookie. A token is void the moment its turn
    /// ends (`docs/managed-subagents.md` §5.1): the row is cleared in the turn
    /// epilogue, and a later turn on the same thread carries a new cookie.
    /// Since SH-A3 the window also holds while an `attached_sessions` row with
    /// the token's cookie exists on the token's thread (an interactive session
    /// attached through `cauldron-claude`, `docs/SHIMS.md` §A); detach deletes it.
    fn verify(&self, authorization: Option<&str>) -> Result<shapewire::token::Claims, Refusal> {
        use shapewire::envelope::codes::UNAUTHORIZED;
        let Some(raw) = authorization else {
            return Err(Refusal::new(
                UNAUTHORIZED,
                "missing Authorization: Bearer <run token>; the daemon mints one per turn into \
                 the child's environment as THREADS_RUN_TOKEN",
            ));
        };
        let Some(token) = raw
            .strip_prefix("Bearer ")
            .or_else(|| raw.strip_prefix("bearer "))
        else {
            return Err(Refusal::new(UNAUTHORIZED, "Authorization must be `Bearer <run token>`"));
        };
        // No echo of what was presented: a refusal that quotes the token puts
        // it in whatever logs the refusal.
        let claims = self
            .manager
            .run_token_key
            .verify(token.trim())
            .map_err(|e| Refusal::new(UNAUTHORIZED, format!("run token refused: {e}")))?;
        // The managed window: a `running_turns` row for the thread with the
        // token's cookie. Checked first and unchanged, so every token this
        // accepted before SH-A3 is accepted still.
        let running = match self.manager.store.get_running_turn(&claims.thread_id) {
            Ok(Some(row)) if row.spawn_cookie == claims.turn => return Ok(claims),
            Ok(row) => row,
            Err(e) => {
                return Err(Refusal::new(
                    shapewire::envelope::codes::ERROR,
                    format!("reading running_turns for {}: {e}", claims.thread_id),
                ))
            }
        };
        // SH-A3 (docs/SHIMS.md §SH-A3): the ATTACHED window. A session the
        // operator drives in his own terminal holds no `running_turns` row; its
        // token is good while an `attached_sessions` row with the token's cookie
        // exists ON THE TOKEN'S THREAD. Detach (and the reaper, on pid death)
        // deletes the row, so a gone row is an ended session. A cookie that
        // belongs to another thread's session is refused, never re-homed.
        match self.manager.store.get_attached_session(&claims.turn) {
            Ok(Some(row)) if row.thread_id == claims.thread_id => return Ok(claims),
            Ok(Some(_)) => {
                return Err(Refusal::new(
                    UNAUTHORIZED,
                    format!(
                        "the token's cookie names an attached session on another thread, not \
                         thread {}; a run token is good only on the thread it was minted for",
                        claims.thread_id
                    ),
                ))
            }
            Ok(None) => {}
            Err(e) => {
                return Err(Refusal::new(
                    shapewire::envelope::codes::ERROR,
                    format!("reading attached_sessions for {}: {e}", claims.thread_id),
                ))
            }
        }
        match running {
            Some(_) => Err(Refusal::new(
                UNAUTHORIZED,
                format!(
                    "the token's turn on thread {} has ended; a later turn is running under a \
                     different cookie. A run token is void the moment its turn ends",
                    claims.thread_id
                ),
            )),
            None => Err(Refusal::new(
                UNAUTHORIZED,
                format!(
                    "no turn is running on thread {}; a run token is void the moment its turn \
                     ends",
                    claims.thread_id
                ),
            )),
        }
    }

    /// Ours (the in-process `agents_*` façade), then the tools the cauldron
    /// supervisor declares (R5.S2, DEC-34) — see [`crate::mcp_route`].
    fn tools(&self) -> Vec<shapewire::mcp::ToolDef> {
        let mut tools = crate::agents::defs();
        let declared = crate::mcp_route::declared_tools(&tools);
        tools.extend(declared);
        tools
    }

    fn call<'a>(
        &'a self,
        caller: &'a shapewire::token::Claims,
        name: &'a str,
        arguments: Value,
    ) -> BoxFut<'a, Result<Value, Refusal>> {
        Box::pin(async move {
            // The same permit `handle` holds for a WS request — one counter
            // for both doors, see the impl doc.
            let _serving = self.manager.update.enter_request();
            // R9.C7g: every agent tool acts on, or reads, the managed tree
            // boot recovery is still resuming — same wait-or-`busy` as `handle`.
            let rec = self.manager.update.recovery();
            if rec.is_recovering() && !rec.wait_done().await {
                return Err(rec.refusal(name));
            }
            // R5.S2 (DEC-34): an in-process tool is served here; every other
            // name goes THROUGH THE SUPERVISOR, which owns the tool processes.
            if crate::agents::tool(name).is_some() {
                crate::agents::call(&self.manager, caller, name, arguments).await
            } else {
                crate::mcp_route::call(caller, name, arguments).await
            }
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The resource door — message bodies, by one opaque id
// ─────────────────────────────────────────────────────────────────────────────

/// **ONE OPAQUE ID → ONE MESSAGE BODY, AND NOTHING ELSE RESOLVES HERE.**
///
/// The chassis hands over a [`ResourceId`] it minted through its single
/// constructor, so this never sees a query, a path or a second parameter — the
/// no-query rule is structural and is not restated here. What this adds is the
/// producer-side half the chassis cannot know: which of ITS resources an opaque
/// segment names, and which typed refusal a miss earns.
///
/// **It serves BOTH doors.** `GET /resource/<id>` and the websocket's
/// `resource/{id}/read` both reach this one `fetch`, so an id that resolves on
/// one resolves on the other and a consumer cannot tell them apart by which ids
/// work. That is what makes `mirror::Mirror::plan`'s choice invisible rather
/// than merely undocumented.
///
/// ## No [`Uploads`], deliberately
///
/// `Uploads::digest` exists so the CHASSIS can verify a PUT against the digest
/// it was keyed by. A message body's id is a durable store key — `(thread_id,
/// seq)` — and not a content digest, so there is nothing for the chassis to
/// verify and nothing a consumer could legitimately choose. `resource.rs` says
/// it outright: *"a producer whose ids are durable store keys must not implement
/// `Uploads` at all"*. A message is written by `threads/{id}/event`, which is a
/// typed command that mints the `seq`; PUT stays a 405.
///
/// [`Uploads`]: shapewire::resource::Uploads
/// [`ResourceId`]: shapewire::resource::ResourceId
impl shapewire::resource::Resources for ThreadsProducer {
    fn fetch(
        &self,
        id: &shapewire::resource::ResourceId,
        range: Option<shapewire::resource::ByteRange>,
    ) -> Result<shapewire::resource::Found, Refusal> {
        use crate::shapes::body;
        use shapewire::resource::{Found, Retention};

        // **THE RANGE IS IGNORED, AND THAT IS CORRECT RATHER THAN LAZY.**
        // `Found::offset` stays 0 and the chassis skips the difference itself; a
        // message body is already in memory by the time its length is known, so
        // seeking would save nothing. `Found::offset`'s contract is explicit
        // that a producer which ignores the range *"is correct and merely
        // slower"*.
        let _ = range;

        // **A MESSAGE PAGE FIRST** (R2.S3d, `crate::shapes::pages`). Its id's
        // last segment starts with `m`, so it can never parse as a body id and
        // a body id can never parse as a page: the two grammars are disjoint
        // and the order of the two parses decides nothing.
        if let Some((thread_id, from, to)) = crate::shapes::pages::parse_page_id(id.as_str()) {
            return self.fetch_page(id.as_str(), thread_id, from, to);
        }
        let Some((thread_id, seq)) = body::parse_body_id(id.as_str()) else {
            return Err(body::unknown(id.as_str()));
        };
        let Some(stored) = self.manager.store.event_at(thread_id, seq).map_err(|e| {
            Refusal::new(
                shapewire::envelope::codes::ERROR,
                format!("reading the event behind {id}: {e}"),
            )
        })?
        else {
            return Err(body::unknown(id.as_str()));
        };
        // **EVERY KIND IS SERVED NOW, AND THE ARM THAT USED TO REFUSE IS GONE.**
        // This read `body_text_of_payload` and answered `unknown` for a kind with
        // no body, on the stated grounds that *"the shape publishes no id for a
        // bodiless event, so arriving here means the consumer built an id"*.
        // `crate::shapes::log` publishes a summary for rows of EVERY kind — and a
        // summary naming a resource the door refuses would be a shape claiming
        // something exists that cannot be read — so that reasoning expires with
        // it. `body::served_of` decides the bytes and the media type off the
        // event's KIND, which the consumer already holds on the row, so there is
        // still exactly one id scheme and nothing is guessed from the bytes.
        //
        // **What is UNCHANGED: the three `BODY_KINDS` are served byte-for-byte as
        // they were, under `text/plain`**, because `served_of` asks `body_text`
        // first and that function did not move. `threads.body`'s summaries and
        // `tests/thread_bodies.rs` describe the same door they always did.
        //
        // **An unreadable payload no longer reaches here as an error**, and that is
        // not a dropped refusal: `Store::event_at` goes through `rows_to_events`,
        // which degrades a payload that will not parse into `Unknown { raw }` at
        // the one site that counts and warns about it. So the door serves the raw
        // text the store holds, under `application/json`, which is what the row's
        // summary measured — rather than refusing bytes a shape has already told a
        // consumer the size of.
        let (text, content_type) = body::served_of(&stored.event);
        let bytes = text.into_bytes();
        let len = bytes.len() as u64;
        Ok(Found::whole(
            // IMMUTABLE: `(thread_id, seq)` is UNIQUE and no `UPDATE events` or
            // `DELETE FROM events` exists anywhere in this crate, so the row an
            // id names cannot change under the writer set that exists. Deletion
            // is orthogonal to the kind and would answer `gone` — or, for this
            // store, `unknown`, which is what it can honestly say.
            Retention::Fixed,
            content_type,
            len,
            Box::new(std::io::Cursor::new(bytes)),
        ))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Rule 2.7's third stage, on this producer
// ─────────────────────────────────────────────────────────────────────────────

/// **`threads` CAN PROMISE DURABILITY, AND NOW IT PROMISES IT ONLY WHEN ASKED.**
///
/// The split between this impl and `shapewire::command::barrier` is the
/// whole of the rule's division of labour: the chassis decides which refusals
/// come before the fsync, that the ceiling is named and never clamped, and that
/// an id this producer never minted is not answered `durable`. This impl does
/// two things the chassis cannot: it reads the promise book's watermark, and it
/// asks the store to commit.
impl shapewire::command::Durable for ThreadsProducer {
    /// The promise book's own watermark. Not a count this module keeps —
    /// `PromiseBook` is the one id space and a second counter beside it would be
    /// a second source for the same number.
    fn issued(&self) -> shapewire::command::PromiseId {
        self.manager.promises.issued()
    }

    fn commit(&self) -> Result<(), Refusal> {
        match self.manager.store.durability_barrier() {
            Ok(outcome) => {
                // **THE NUMBERS STOP HERE.** Rule 2.7's reply carries the stage
                // and nothing else — *"ack should just inform the general
                // milestones, not protocol-specific details. User should not
                // care."* A log line is where "the checkpoint folded 41 frames"
                // belongs, because the operator is the only reader who can act
                // on it.
                tracing::debug!(?outcome, "durability barrier");
                Ok(())
            }
            // **A BARRIER THAT COULD NOT COMMIT REFUSES.** `busy` because the
            // remedy is to call it again: the only way to reach here on a store
            // that declared durability is a checkpoint that could not get the
            // lock it needed inside `busy_timeout`. It is NOT `unsupported` —
            // that is the standing capability, answered before this is ever
            // called — and it is NOT `error`, which would say the consumer has
            // nothing to try.
            Err(e) => Err(Refusal::new(
                shapewire::envelope::codes::BUSY,
                format!(
                    "the durability barrier did not commit, so nothing is being \
                     claimed durable: {e}"
                ),
            )),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Standing the server up
// ─────────────────────────────────────────────────────────────────────────────

/// The chassis over this manager, unserved. Hold this when you need the
/// [`shapewire::server::Bearers`] — a test asserting a grant's lifetime, or
/// the resource route when it lands.
pub fn chassis(
    manager: Arc<ThreadManager>,
) -> shapewire::server::Chassis<ThreadsProducer> {
    // **THE SHAPES ARE REGISTERED HERE.** `start_watchers` is the idempotent
    // once-per-process hook (`watchers_started.swap`) and a shape task needs a
    // runtime to be spawned into; it is NOT the chassis's business, so it cannot
    // live in `shapewire::server`, and a chassis server standing up without it
    // would answer `unknown shape: accounts` to every subscribe. (The legacy
    // server's `serve_on_with_shutdown` called it too until R2.S4 deleted that
    // server; this is now the only path.)
    manager.start_watchers();
    shapewire::server::Chassis::new(Arc::new(ThreadsProducer::new(manager)))
}

/// **STAND THE SERVER UP** on `addr` (port 0 → ephemeral). `run_daemon` and
/// every test go through [`chassis`], so "what the tests prove" and "what the
/// daemon runs" cannot drift; `run_daemon` binds first itself (the port-take
/// is the singleton — see [`crate::server::bind`]) and serves the listener.
pub async fn serve(
    manager: Arc<ThreadManager>,
    addr: std::net::SocketAddr,
) -> std::io::Result<(std::net::SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = shapewire::server::bind(addr).await?;
    // **THE TURNS' DOOR IS WHERE WE ACTUALLY LISTEN** (R4.S5): the listener's
    // own address, so a port-0 bind hands every turn the ephemeral port.
    manager.set_mcp_endpoint(listener.local_addr()?);
    chassis(manager).serve_on_with_shutdown(listener, std::future::pending::<()>())
}
