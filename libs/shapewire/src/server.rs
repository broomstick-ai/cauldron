//! # The chassis server — the whole network API, once
//!
//! **Operator ruling, 2026-09-26:** *"I would want the whole api rolled into
//! the chassis if it's not already, including health and resource."* This
//! supersedes `docs/shapes.md` 6.2's "shapewire carries frame TYPES, not a
//! server" clause, which is annotated in place.
//!
//! The reason is the generality requirement and not tidiness. If the chassis
//! serves any two of the operator's systems, then every producer
//! re-implementing the handshake, auth, `/health`, dispatch and the connection
//! lifecycle is exactly the duplication the chassis exists to prevent — two
//! producers would mean two subtly-different handshakes, and the difference
//! would be discovered by a consumer rather than by a compiler.
//!
//! ## What is here, and what a producer still owns
//!
//! | Here | A producer's |
//! |---|---|
//! | the `axum` router: `/ws`, `/health` | — |
//! | the `Welcome` handshake and the shared-secret `auth` frame | the secret's VALUE, through [`Producer::secret`] |
//! | the connection loop's four `select!` arms | the contents of one of them, through [`ProducerStream`] |
//! | the connection lifecycle: id minting, `hub.dropped`, `shape_subs`, the `Lagged` repair | — |
//! | `shapes.list` / `shapes.subscribe` / `shapes.unsubscribe` | — (they were always chassis methods wearing a producer's match) |
//! | the typed error envelope ([`crate::envelope`]) | a [`crate::envelope::Coded`] impl, if it wants its own errors typed |
//! | `/health`'s fixed key set | the VALUES, through [`Producer::identity`], plus extras |
//! | — | its METHODS, behind [`Producer::handle`] and [`Producer::names`] |
//! | — | its own legacy transport, behind [`ProducerStream`] and [`ProducerControl`] |
//!
//! ## The two extension points, and why there are two rather than one
//!
//! `docs/chassis-writes.md` §10.2 proposed one — `extra_stream(conn) ->
//! Option<BoxStream<Value>>`. The code forced two changes to that shape, both
//! load-bearing:
//!
//! 1. **A producer's legacy CONTROL frames need a door too.** `threads`'
//!    `{"type":"subscribe","threads":null}` is what all three real consumers
//!    send, and it is not a `request` — so it cannot arrive through
//!    [`Producer::handle`] as §10.2 assumed without changing the wire. It
//!    arrives through [`ProducerControl::client_frame`] instead, which is
//!    handed any frame whose `type` the chassis does not claim.
//! 2. **The stream and the control half cannot be one object.** Their futures
//!    and their handlers live in different `select!` arms, and one arm's future
//!    expression borrowing an object mutably makes it untouchable in every
//!    other arm's handler. `threads` shares what it must (the subscription
//!    filter) through its own `Arc<Mutex<…>>`; the chassis is not told there is
//!    one.
//!
//! **A frame from a producer is forwarded as already-serialised TEXT, never as
//! a `serde_json::Value`.** That is not a micro-optimisation: `serde_json::Map`
//! is a `BTreeMap` here, so a `Value` round trip SORTS the keys, and a legacy
//! wire that was byte-identical before the move would have come out
//! alphabetised. Byte-identity of the legacy wire is therefore a construction —
//! there is no chassis type a producer frame passes through.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};

use crate::envelope::{
    self, ChassisMsg, ChassisServerMsg, ClientFrame, Coded, HealthWire, Identity, Outcome, Refusal,
};
use crate::hub::{Hub, Kind};
use crate::wire::ConnId;

// ─────────────────────────────────────────────────────────────────────────────
// 0. THE PATH GRAMMAR — `docs/shape-design-guide.md` rule 1
// ─────────────────────────────────────────────────────────────────────────────

/// **THE ADDRESSING SURFACE IS THE SHAPE HIERARCHY, AND A METHOD IS A LEAF ON
/// IT.**
///
/// Operator ruling, 2026-09-28: *"controllers inline it is… shapes should make
/// up 95% of the hierarchy, and resources and controllers are leaf nodes with a
/// single purpose. Their context is the parent shape."* And, on the commands
/// that name no key: *"they are not root-level, they are under, e.g. `daemon`,
/// `gauge`."*
///
/// So every method is
///
/// ```text
///     <family>[/<subject>]/<leaf>
/// ```
///
/// * **`family`** — the noun the state lives under. A shape path where a shape
///   exists (`accounts`, `threads`, `threads/summary`), otherwise the noun a
///   shape would be keyed on (`budgets`, `gauge`, `daemon`, `ledger`,
///   `shapes`). Never empty, so **nothing sits at the root**.
/// * **`subject`** — the key the call is ABOUT, present exactly when the call
///   REQUIRES one. This is the whole point of the move and not a spelling
///   choice: a subject that travels as a parameter can disagree with the key
///   the fold re-keys onto, and every promise-subject bug in this producer came
///   from that disagreement — including `accounts.disable default`, which
///   opened its promise under the ALIAS while the fold re-keyed to the host
///   account, so the id was stranded forever. In `accounts/default/disable` the
///   subject IS the path and there is no second place for it to be written
///   down.
/// * **`leaf`** — ONE segment, the operation or the read. Multi-word leaves
///   spell the space as `_` (`pool_share_set`, `queue_cancel`), because `/`
///   means exactly one thing — a step down the hierarchy — and a leaf that used
///   it would be claiming to be a shape.
///
/// **An OPTIONAL key is a filter, not a subject.** `threads/status` answers for
/// one thread or for all of them and cannot be split in two without inventing a
/// second act; `threads/{thread_id}/get` cannot answer at all without its key.
/// Required goes in the path; optional stays a parameter. That is the whole
/// rule, and it is mechanical rather than a judgment per method.
///
/// # Parsing, and why it is right-to-left
///
/// A key may contain `/` — `shapes/threads/summary/subscribe` names the shape
/// `threads/summary` — so a left-to-right parse cannot find the boundary. Both
/// the FAMILY set and the LEAF set are closed and known to the producer, so the
/// parse is: the last segment is the leaf; try the longest family first and
/// take whatever is left over as the subject. The first template that matches
/// wins, and because families are tried longest-first a subjectless template
/// always beats a subject-bearing one with a shorter family.
///
/// **The known collision, stated rather than hidden:** a key whose value equals
/// a sub-family segment is shadowed when a template exists with the same leaf
/// under that sub-family. No such pair exists today (`accounts/login/begin` has
/// no `accounts/{account}/begin` twin), and adding one would make one key
/// unaddressable. `no_family_shadows_a_key` pins that no pair exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodPath {
    /// The family — everything before the subject. Never empty, never starts or
    /// ends with `/`.
    pub family: &'static str,
    /// The PARAMETER NAME the subject segment fills, e.g. `"thread_id"`.
    /// `None` when this method is about the family and not about one key.
    pub subject: Option<&'static str>,
    /// The leaf. One segment, no `/`.
    pub leaf: &'static str,
}

impl MethodPath {
    /// Read a template such as `threads/{thread_id}/send` or `gauge/probe`.
    ///
    /// Panics on a malformed template, which is a build-time mistake in a
    /// producer's own inventory and not something a consumer can provoke;
    /// `every_template_is_well_formed` pins it for this tree.
    pub fn parse(template: &'static str) -> Self {
        let segs: Vec<&'static str> = template.split('/').collect();
        assert!(
            segs.len() >= 2,
            "a method template has at least a family and a leaf: {template:?}"
        );
        assert!(
            segs.iter().all(|s| !s.is_empty()),
            "no empty segment in a method template: {template:?}"
        );
        let leaf = segs[segs.len() - 1];
        assert!(
            !leaf.starts_with('{'),
            "the LEAF may not be the subject: {template:?}"
        );
        let mut subject = None;
        let mut family_end = segs.len() - 1;
        for (i, s) in segs[..segs.len() - 1].iter().enumerate() {
            if let Some(name) = s.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                assert!(i > 0, "the FAMILY may not be the subject: {template:?}");
                assert!(subject.is_none(), "one subject per template: {template:?}");
                assert!(
                    i == segs.len() - 2,
                    "the subject is the segment before the leaf: {template:?}"
                );
                subject = Some(name);
                family_end = i;
            }
        }
        let family_len: usize =
            segs[..family_end].iter().map(|s| s.len() + 1).sum::<usize>() - 1;
        Self {
            family: &template[..family_len],
            subject,
            leaf,
        }
    }
}

/// What a concrete path resolved to: the template that matched, and the subject
/// the path carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routed {
    /// The template. **This is what reaches [`Producer::handle`]** — so a
    /// producer's match arms are fixed literals and the variable segment never
    /// reaches them as text.
    pub template: &'static str,
    /// `(parameter name, value)` — what the subject segment carried, ready to
    /// be bound into the params.
    pub subject: Option<(&'static str, String)>,
}

/// **RESOLVE ONE CONCRETE PATH AGAINST AN INVENTORY.** `None` = no template
/// matches, which is `unknown_method`.
pub fn route(templates: &[&'static str], path: &str) -> Option<Routed> {
    if path.is_empty() || path.contains("//") || path.starts_with('/') || path.ends_with('/') {
        return None;
    }
    let segs: Vec<&str> = path.split('/').collect();
    if segs.len() < 2 {
        return None;
    }
    let leaf = segs[segs.len() - 1];
    // Longest family first: a subjectless template beats a subject-bearing one.
    for k in (1..segs.len()).rev() {
        let family_len = segs[..k].iter().map(|s| s.len() + 1).sum::<usize>() - 1;
        let family = &path[..family_len];
        let wants_subject = k < segs.len() - 1;
        for t in templates {
            let m = MethodPath::parse(t);
            if m.family != family || m.leaf != leaf || m.subject.is_some() != wants_subject {
                continue;
            }
            let subject = m.subject.map(|name| {
                let from = family.len() + 1;
                let to = path.len() - leaf.len() - 1;
                (name, path[from..to].to_string())
            });
            return Some(Routed {
                template: t,
                subject,
            });
        }
    }
    None
}

/// **BIND THE SUBJECT INTO THE PARAMS — and REFUSE a second copy of it.**
///
/// The handler still reads `thread_id` out of `params`, exactly as it did when
/// the caller put it there; what changed is that the caller can no longer put
/// it there. A request that names its subject in BOTH places is refused rather
/// than resolved by precedence: a precedence rule is a place for the two to
/// disagree, and deleting that place is the entire reason the subject moved
/// into the path.
pub fn bind_subject(params: &mut Value, routed: &Routed) -> Result<(), Refusal> {
    let Some((name, value)) = routed.subject.as_ref() else {
        return Ok(());
    };
    if !params.is_object() {
        *params = json!({});
    }
    let obj = params.as_object_mut().expect("just made it an object");
    if obj.contains_key(*name) {
        return Err(Refusal::malformed(format!(
            "`{name}` is the SUBJECT of `{}` and rides the path, not the params — \
             send `{}` and drop `{name}`",
            routed.template,
            routed.template.replace(&format!("{{{name}}}"), value),
        )));
    }
    obj.insert((*name).to_string(), Value::String(value.clone()));
    Ok(())
}

/// How often the chassis pings an idle socket.
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

/// **THE METHODS THE CHASSIS ANSWERS ITSELF.**
///
/// They are matched BEFORE [`Producer::handle`], so a producer that still has
/// arms of these names keeps them as dead code rather than as a conflict — and
/// that is deliberate during the migration, where `threads`' own router is
/// still serving the same three names out of its own match.
///
/// `shapes/{shape}/subscribe` and `shapes/{shape}/unsubscribe` are here because
/// they need this connection's id and its receiver; `shapes/list` is here
/// because it is a chassis question and the producer has no part of the answer.
/// All three were chassis methods wearing a producer's match, which is why
/// moving them is a simplification and not a cost.
///
/// **THE SHAPE IS THE SUBJECT AND IT RIDES THE PATH** (2026-09-28). It was the
/// one required parameter on the chassis's own surface, and leaving it in the
/// params while every producer method moved would have left the chassis
/// exempting itself from the law it enforces. It is also the case that proves
/// the grammar has to parse right-to-left: `threads/summary` is a shape name
/// with a `/` in it, so `shapes/threads/summary/subscribe` has a TWO-SEGMENT
/// subject and no left-to-right parse could find its boundary.
///
/// **`durable/barrier` IS THE FOURTH, AND IT IS HERE FOR A FOURTH REASON**
/// (2026-10-02, rule 2.7's third stage). It is not that it needs this
/// connection's id — it does not. It is that the RULE is the chassis's: which
/// refusals come before the fsync, that a ceiling is named and never clamped,
/// that an id this producer never minted is not answered `durable`, and that a
/// producer with no durability refuses instead of downgrading. All of that is
/// [`crate::command::barrier`]; the producer contributes one `commit()`. Left in
/// a producer's match, every producer would re-derive the order of those checks
/// and they would drift — which is exactly what happened to `shapes.list` before
/// it moved here.
pub const CHASSIS_METHODS: &[&str] = &[
    "durable/barrier",
    "shapes/list",
    "shapes/{shape}/subscribe",
    "shapes/{shape}/unsubscribe",
    // **THE SECOND RESOURCE DOOR**, and it is the chassis's for the same reason
    // the first one is: the subject is an opaque, shape-issued id, the gate that
    // mints it is `crate::resource`'s single constructor, and a producer that
    // routed this itself would be a second place for the no-query rule to be
    // forgotten. See `resource::read_resource_inline`.
    //
    // It is HERE rather than only on HTTP because the consuming chassis chooses
    // per payload and needs both doors to exist to have a choice
    // (`mirror::Mirror::plan`). Nothing about which door a consumer uses reaches
    // the producer: both arrive at one `Resources::fetch`.
    "resource/{id}/read",
];

// ─────────────────────────────────────────────────────────────────────────────
// 1. What a producer hands the chassis
// ─────────────────────────────────────────────────────────────────────────────

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// **WHAT A PRODUCER HANDS THE CHASSIS. EVERYTHING ELSE IS THE CHASSIS'S.**
pub trait Producer: Send + Sync + 'static {
    /// The hub's input type — the producer's own event union.
    type Input: Kind + Clone + Send + Sync + 'static;
    /// This producer's per-connection stream. [`NoStream`] when it has none.
    type Stream: ProducerStream;
    /// This producer's per-connection control-frame handler. [`NoControl`] when
    /// it has no frames of its own.
    type Control: ProducerControl;

    /// Name, version, proto, boot id. `Welcome` and `/health` both read THIS
    /// and nothing else, so the handshake and the deployment probe cannot
    /// disagree about what is running.
    fn identity(&self) -> &Identity;

    /// The shape hub. The chassis subscribes, unsubscribes, lists and drops
    /// against it; it never emits.
    fn hub(&self) -> &Hub<Self::Input>;

    /// The shared secret, if one is configured. `None` = the socket is open and
    /// `auth_required` is false on `Welcome`.
    fn secret(&self) -> Option<String> {
        None
    }

    /// **THE PINNED METHOD INVENTORY** — was `threads`' `WS_METHODS`. The
    /// chassis adds [`CHASSIS_METHODS`]; the union is the daemon's surface.
    fn names(&self) -> &'static [&'static str];

    /// Route one request to a payload or a typed refusal. Was `handle`.
    ///
    /// A producer that wants its errors typed maps them through
    /// [`Coded`]; one that does not gets [`envelope::codes::ERROR`], which is
    /// what every refusal carried before R1.
    fn handle<'a>(&'a self, method: &'a str, params: Value) -> BoxFut<'a, Result<Value, Refusal>>;

    /// EXTRA `/health` fields. The six fixed keys are not negotiable and an
    /// extra that shadows one panics at build time — see [`HealthWire::build`].
    fn health_extras(&self) -> BTreeMap<String, Value> {
        BTreeMap::new()
    }

    /// Open this producer's per-connection halves. Called once per socket,
    /// before the `Welcome`.
    fn connect(&self, conn: ConnId) -> (Self::Stream, Self::Control);

    /// **THE RESOURCE DOOR'S PRODUCER HALF.** `None` — the default — means this
    /// producer serves no resources.
    ///
    /// The route is mounted either way, and answers
    /// [`crate::envelope::codes::UNSUPPORTED`] (501) when this returns `None`.
    /// That is the honest answer and a 404 is not: a 404 from a real route and a
    /// 404 from no route are the same bytes, so a producer with no resources
    /// would be indistinguishable from one that has lost a resource. See
    /// [`crate::resource`].
    fn resources(&self) -> Option<&dyn crate::resource::Resources> {
        None
    }

    /// **RULE 2.7's THIRD STAGE, DECLARED OR REFUSED BY THIS ONE METHOD.**
    ///
    /// `None` — the default — means this producer promises no durability, and
    /// `durable/barrier` answers [`crate::envelope::codes::UNSUPPORTED`]. That is
    /// the REFUSAL the rule requires, and it is the default on purpose: a
    /// producer that has not thought about durability must not be able to imply
    /// one by accident.
    ///
    /// On the same terms as [`Self::resources`] — the method exists on every
    /// chassis so the refusal is reachable and distinguishable from "no such
    /// method". See [`crate::command::Durable`].
    fn durable(&self) -> Option<&dyn crate::command::Durable> {
        None
    }

    /// **THE MCP DOOR'S PRODUCER HALF.** `None` — the default — means this
    /// producer exposes no tools to agents, and `POST /mcp` answers
    /// [`envelope::codes::UNSUPPORTED`] (501).
    ///
    /// On the same terms as [`Self::resources`]: the route is mounted on every
    /// chassis so the refusal is reachable and distinguishable from a chassis
    /// with no such route. The door is a SECOND CARRIER for methods that
    /// already have a home behind [`Self::handle`] — see [`crate::mcp`] for
    /// what the chassis owns and what the producer's façade owns.
    fn mcp(&self) -> Option<&dyn crate::mcp::McpTools> {
        None
    }
}

/// One frame off a producer's per-connection stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtraFrame {
    /// Already serialised. Forwarded to the socket verbatim.
    Send(String),
    /// The producer's stream ended, or failed unrecoverably. The chassis closes
    /// the connection — `threads`' `Err(_) => break` on its firehose.
    Closed,
}

/// **THE EXTENSION ARM.** A producer's own per-connection stream, polled by the
/// chassis as one more `select!` arm and forwarded verbatim.
///
/// It is deliberately dumb. `threads`' firehose arm filters by subscription
/// state, replays a boot note once, and advances per-thread replay cursors for
/// its `Lagged` recovery; none of that is chassis knowledge, so all of it lives
/// behind this one method.
pub trait ProducerStream: Send + 'static {
    /// Yield the next frame to forward.
    ///
    /// **`deliverable` is `false` while the connection has not authenticated**,
    /// and the contract has two halves, both of which `threads` got from
    /// today's code rather than from a preference:
    ///
    /// * the producer must still CONSUME its stream, so an unauthenticated
    ///   connection cannot bank a backlog and be handed a flood the instant it
    ///   authenticates (today's loop drains and discards); and
    /// * it must not record anything as DELIVERED, because a replay cursor that
    ///   advances past a frame nobody received tells a client to resume after an
    ///   event it never got.
    ///
    /// The default never resolves: a producer with no legacy transport has a
    /// three-arm loop and writes nothing.
    fn next(&mut self, deliverable: bool) -> BoxFut<'_, ExtraFrame> {
        let _ = deliverable;
        Box::pin(std::future::pending())
    }
}

/// **THE SECOND EXTENSION POINT.** A client frame whose `type` the chassis does
/// not claim.
pub trait ProducerControl: Send + 'static {
    /// Answer a producer-owned client frame.
    ///
    /// `Some(frames)` — mine; send these, in this order, already serialised.
    /// `None` — not mine; the chassis refuses the frame as invalid.
    fn client_frame(&mut self, ty: &str, raw: &Value) -> Option<Vec<String>> {
        let _ = (ty, raw);
        None
    }
}

/// A producer with no legacy transport.
pub struct NoStream;
impl ProducerStream for NoStream {}

/// A producer with no control frames of its own.
pub struct NoControl;
impl ProducerControl for NoControl {}

// ─────────────────────────────────────────────────────────────────────────────
// 2. The bearer registry — the second door's authenticator
// ─────────────────────────────────────────────────────────────────────────────

/// The header the resource door reads, and the ONLY place it will ever read a
/// bearer from.
pub const AUTHORIZATION: &str = "authorization";

/// **EVERY LIVE CONNECTION'S BEARER.**
///
/// One token per connection, minted when that connection completes the
/// handshake and revoked when its socket drops — the same `Drop` that closes its
/// shape subscriptions, so "the connection ended" and "the grant expired" are
/// the same event under every exit, including a panic and an abort.
///
/// **Why a map lookup and not the `constant_time_eq` the secret gets.** The
/// daemon secret is a low-entropy operator-chosen string, where a timing oracle
/// is a real attack and a byte-by-byte compare is the wrong primitive. A bearer
/// is 122 bits of v4 UUID randomness minted per connect; there is nothing for a
/// timing oracle to converge on, and a linear constant-time scan over every live
/// connection would be a per-request cost paid for no property.
#[derive(Default)]
pub struct Bearers {
    inner: std::sync::Mutex<BearerMap>,
}

#[derive(Default)]
struct BearerMap {
    by_conn: std::collections::HashMap<ConnId, String>,
    by_token: std::collections::HashMap<String, ConnId>,
}

impl Bearers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue this connection's bearer, or return the one it already has.
    ///
    /// **Idempotent per connection, deliberately.** A client that sends a second
    /// `auth` frame gets the same grant rather than a new one, because a second
    /// token would orphan the first: the client would hold a bearer this
    /// registry still honours and does not know it handed out twice.
    pub fn issue(&self, conn: ConnId) -> envelope::Bearer {
        let mut m = self.inner.lock().unwrap();
        if let Some(t) = m.by_conn.get(&conn) {
            return envelope::Bearer::from_token(t.clone());
        }
        let token = uuid::Uuid::new_v4().simple().to_string();
        m.by_conn.insert(conn, token.clone());
        m.by_token.insert(token.clone(), conn);
        envelope::Bearer::from_token(token)
    }

    /// Revoke this connection's bearer. Called from the connection guard.
    pub fn revoke(&self, conn: ConnId) {
        let mut m = self.inner.lock().unwrap();
        if let Some(t) = m.by_conn.remove(&conn) {
            m.by_token.remove(&t);
        }
    }

    pub fn live(&self) -> usize {
        self.inner.lock().unwrap().by_conn.len()
    }

    /// **THE RESOURCE DOOR'S ONLY AUTHENTICATOR — A HEADER, NEVER A QUERY.**
    ///
    /// The no-query rule is load-bearing and it is not about the transport: the
    /// resource endpoint takes exactly one opaque shape-issued id and never a
    /// query, because the drift risk was a PARALLEL NAMESPACE — a second way to
    /// name a resource, growing filters and selectors beside the shape that
    /// already names it. This function's signature is where that rule is
    /// enforced rather than asserted: it takes a `HeaderMap` and has no access
    /// to a URI, so a bearer in a query string is not refused, it is
    /// unreachable.
    pub fn verify(&self, headers: &axum::http::HeaderMap) -> Result<ConnId, Refusal> {
        let Some(raw) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
            return Err(Refusal::new(
                envelope::codes::UNAUTHORIZED,
                "missing Authorization: Bearer <token>; the websocket handshake issues one",
            ));
        };
        let Some(token) = raw
            .strip_prefix("Bearer ")
            .or_else(|| raw.strip_prefix("bearer "))
        else {
            return Err(Refusal::new(
                envelope::codes::UNAUTHORIZED,
                "Authorization must be `Bearer <token>`",
            ));
        };
        self.inner
            .lock()
            .unwrap()
            .by_token
            .get(token.trim())
            .copied()
            .ok_or_else(|| {
                Refusal::new(
                    envelope::codes::UNAUTHORIZED,
                    // No echo of what was presented. A refusal that quotes the
                    // token puts it in whatever logs the refusal.
                    "unknown or expired bearer; re-connect to be issued one",
                )
            })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. The router
// ─────────────────────────────────────────────────────────────────────────────

/// The chassis's router state: the producer, plus the bearer registry the
/// second door will authenticate against.
pub struct Chassis<P: Producer> {
    producer: Arc<P>,
    bearers: Arc<Bearers>,
    reads: Arc<crate::resource::ReadGate>,
}

impl<P: Producer> Clone for Chassis<P> {
    fn clone(&self) -> Self {
        Self {
            producer: Arc::clone(&self.producer),
            bearers: Arc::clone(&self.bearers),
            reads: Arc::clone(&self.reads),
        }
    }
}

impl<P: Producer> Chassis<P> {
    pub fn new(producer: Arc<P>) -> Self {
        Self {
            producer,
            bearers: Arc::new(Bearers::new()),
            reads: Arc::new(crate::resource::ReadGate::new()),
        }
    }
    pub fn producer(&self) -> &Arc<P> {
        &self.producer
    }
    /// The live grants. The resource route authenticates against this, and a
    /// test can read it to assert a grant's lifetime.
    pub fn bearers(&self) -> &Arc<Bearers> {
        &self.bearers
    }
    /// The resource door's in-flight read counter — see
    /// [`crate::resource::MAX_INFLIGHT_RESOURCE_READS`].
    pub fn reads(&self) -> &Arc<crate::resource::ReadGate> {
        &self.reads
    }

    /// **THE ROUTER.**
    ///
    /// **`/resource/:id` is here now**, and it is exactly the one route the
    /// previous revision of this docstring promised — [`Bearers::verify`],
    /// [`envelope::codes`] and this method were the seam, and nothing else here
    /// moved to close it.
    ///
    /// **It is mounted unconditionally, including for a producer that serves no
    /// resources**, and that is the correction to `chassis-writes.md` §10.4 step
    /// 1 rather than a return to it. §10.4 wanted a route "answering 404 for
    /// everything", which is indistinguishable from no route. This route answers
    /// [`envelope::codes::UNSUPPORTED`] with 501 and a typed body, which is
    /// distinguishable from both a missing route and an unknown id — the
    /// difference between refusing honestly and pretending to work.
    ///
    /// **Three methods, one route.** `GET` and `HEAD` read; `PUT` uploads a blob
    /// under its own digest and is 405 unless the producer implements
    /// [`crate::resource::Uploads`]. `HEAD` is registered explicitly rather than
    /// left to axum's HEAD-falls-back-to-GET, which would run the whole read and
    /// discard the bytes — a probe costing a full resource read.
    ///
    /// `DefaultBodyLimit::disable()` applies to this route only, because the
    /// door has its own limit with its own typed refusal
    /// ([`crate::resource::MAX_UPLOAD_BYTES`], `too_large` carrying `limit`);
    /// axum's own limit would answer a bare 413 with no code in it.
    pub fn into_router(self) -> Router {
        Router::new()
            .route("/ws", get(ws_handler::<P>))
            .route("/health", get(health::<P>))
            .route(
                "/resource/:id",
                get(crate::resource::get_resource::<P>)
                    .head(crate::resource::head_resource::<P>)
                    .put(crate::resource::put_resource::<P>)
                    .layer(axum::extract::DefaultBodyLimit::disable()),
            )
            // **THE MCP DOOR** — a sibling of the resource door, same listener,
            // same process, same bearer discipline (a header, never a query).
            // `POST` is the only verb; `GET` (a server-initiated SSE stream)
            // and `DELETE` (session termination) answer 405, which the
            // streamable-HTTP spec permits and `crate::mcp` explains. Mounted
            // unconditionally for the resource door's reason.
            .route(
                crate::mcp::http::PATH,
                axum::routing::post(crate::mcp::http::post::<P>)
                    .get(crate::mcp::http::not_allowed)
                    .delete(crate::mcp::http::not_allowed),
            )
            .with_state(self)
    }

    /// Serve this chassis on an already-bound listener, stopping ACCEPTING when
    /// `shutdown` resolves and letting in-flight connections finish.
    ///
    /// `with_graceful_shutdown` rather than `handle.abort()` is what makes
    /// `/health`'s `graceful_shutdown: true` an honest advertisement rather than
    /// a claim: this chassis genuinely has a clean stop to offer a supervisor
    /// that asks politely.
    pub fn serve_on_with_shutdown(
        self,
        listener: tokio::net::TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
        let bound = listener.local_addr()?;
        let daemon = self.producer().identity().daemon.clone();
        let app = self.into_router();
        let handle = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app)
                .with_graceful_shutdown(shutdown)
                .await
            {
                tracing::error!(error = %e, daemon = %daemon, "chassis server error");
            }
        });
        Ok((bound, handle))
    }
}

/// Build the chassis router. The one-line form of [`Chassis::into_router`].
pub fn router<P: Producer>(producer: Arc<P>) -> Router {
    Chassis::new(producer).into_router()
}

/// Bind on `addr`, returning the actually-bound address (port 0 → ephemeral).
pub async fn bind(addr: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(addr).await
}

/// Serve a producer on an already-bound listener. The one-line form of
/// [`Chassis::serve_on_with_shutdown`], for a caller with no reason to hold the
/// [`Bearers`].
pub fn serve_on_with_shutdown<P: Producer>(
    producer: Arc<P>,
    listener: tokio::net::TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    Chassis::new(producer).serve_on_with_shutdown(listener, shutdown)
}

/// Serve on `addr` until the task is dropped.
pub async fn serve<P: Producer>(
    producer: Arc<P>,
    addr: SocketAddr,
) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = bind(addr).await?;
    serve_on_with_shutdown(producer, listener, std::future::pending::<()>())
}

async fn health<P: Producer>(State(c): State<Chassis<P>>) -> impl IntoResponse {
    let p = c.producer();
    Json(HealthWire::build(p.identity(), p.health_extras()))
}

async fn ws_handler<P: Producer>(
    ws: WebSocketUpgrade,
    State(c): State<Chassis<P>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| drive(socket, c))
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. One client connection
// ─────────────────────────────────────────────────────────────────────────────

/// **THE CONNECTION LOOP.** Four `select!` arms: the client, the producer's own
/// stream, the shape channel, and the keepalive ping.
///
/// Every invariant in here was a COMMENT before the move and is a test now
/// (`tests/chassis_lifecycle.rs`), because this is the one piece of the move
/// whose correctness lives in arm ordering rather than in types.
async fn drive<P: Producer>(socket: WebSocket, chassis: Chassis<P>) {
    let producer = Arc::clone(chassis.producer());
    let bearers = Arc::clone(chassis.bearers());
    let (mut tx, mut rx) = socket.split();

    // ── THE CONNECTION ID COMES FROM THE HUB, NOT FROM A STATIC ────────────
    //
    // Two servers share one hub during the migration. A `static AtomicU64` in
    // each would hand both of their first connections the id 1, and a frame
    // addressed `to: Some(1)` would reach whichever asked — one consumer
    // receiving another's export. The hub owns the id space because the hub is
    // what interprets the ids.
    let conn_id: ConnId = producer.hub().next_conn();
    let (mut stream, mut control) = producer.connect(conn_id);

    // ── THE CONNECTION'S OWN LIFETIME IS WHAT CLOSES ITS SUBSCRIPTIONS ─────
    //
    // Not a `hub.dropped(conn_id)` as the last statement of this function: that
    // statement has two ways not to run — a PANIC anywhere in the loop unwinds
    // past it, and an ABORT drops the task at its suspension point without
    // running anything after it. Both leave the shape task counting a
    // subscriber that is gone and the consumer ledger reporting a consumer that
    // is gone, which is the confidently-wrong state the ledger exists to
    // prevent. A `Drop` runs under every exit, so "the connection ended" and
    // "the entry closed" become the same event.
    //
    // **THE BEARER'S LIFETIME IS THE SAME EVENT.** The grant is revoked here
    // too, so a token cannot outlive the socket that was issued it under any
    // exit path either.
    struct ConnectionGuard<P: Producer>(Arc<P>, Arc<Bearers>, ConnId);
    impl<P: Producer> Drop for ConnectionGuard<P> {
        fn drop(&mut self) {
            self.0.hub().dropped(self.2);
            self.1.revoke(self.2);
        }
    }
    let _conn_guard = ConnectionGuard(Arc::clone(&producer), Arc::clone(&bearers), conn_id);

    let mut shape_subs: HashSet<(String, String)> = HashSet::new();
    // **STANDING MEMBERSHIP INTEREST, PER SHAPE** — the `(conn, None)` half of
    // the gate. Opened by a membership export addressed here and by nothing
    // else; closed by a keyless `shapes.unsubscribe`.
    let mut shape_members: HashSet<String> = HashSet::new();
    // ── THE SHAPE CHANNEL, AND WHY IT IS `None` HERE ───────────────────────
    //
    // **THE LOAD-BEARING LINE FOR "ZERO SHAPE FRAMES ON A LEGACY
    // CONNECTION".** The receiver is created on the FIRST `shapes.subscribe`
    // and never before, so a connection that only ever sends a producer's
    // legacy control frame has no receiver, is not polled by the third arm, and
    // cannot be handed a frame it does not understand. It also cannot be
    // LAGGED by one: shape frames ride a broadcast whose capacity is counted
    // separately from any producer's transport.
    let mut shapes_rx: Option<crate::hub::ConnFrames> = None;

    let secret = producer.secret();
    let mut authed = secret.is_none();

    {
        let id = producer.identity();
        let welcome = ChassisServerMsg::Welcome {
            proto: id.proto,
            daemon: id.daemon.clone(),
            daemon_version: id.version.clone(),
            boot_id: id.boot_id.clone(),
            auth_required: !authed,
            // **THE BEARER, ISSUED ON CONNECT AND GATED BY THE HANDSHAKE.** An
            // open socket has completed its handshake by arriving, so the grant
            // rides the `Welcome`. A socket with a secret to satisfy gets
            // nothing here and is issued one on its `AuthAck`.
            bearer: if authed {
                Some(bearers.issue(conn_id))
            } else {
                None
            },
        };
        if send(&mut tx, &welcome).await.is_err() {
            return;
        }
    }

    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;

    loop {
        tokio::select! {
            incoming = rx.next() => {
                match incoming {
                    None | Some(Err(_)) => break,
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(Message::Ping(_)))
                    | Some(Ok(Message::Pong(_)))
                    | Some(Ok(Message::Binary(_))) => {}
                    Some(Ok(Message::Text(text))) => {
                        let frame = envelope::classify(&text);
                        // THE AUTH GATE, and it is checked in the same place and
                        // the same order as before the move: an `auth` frame is
                        // always honoured, and every other frame is refused on
                        // its own request id until one succeeds.
                        match frame {
                            ClientFrame::Chassis(ChassisMsg::Auth { secret: given }) => {
                                let ok = match &secret {
                                    Some(s) => constant_time_eq(s.as_bytes(), given.as_bytes()),
                                    None => true,
                                };
                                if ok { authed = true; }
                                // The handshake is what gates issuing the
                                // grant, and `issue` is idempotent per
                                // connection so a second `auth` frame returns
                                // the same token rather than orphaning one.
                                let bearer = ok.then(|| bearers.issue(conn_id));
                                let ack = ChassisServerMsg::AuthAck { ok, bearer };
                                if send(&mut tx, &ack).await.is_err() {
                                    break;
                                }
                                if !ok { break; } // bad secret: close
                            }
                            ref other if !authed => {
                                // Echo the request id so callers can correlate
                                // the rejection. A producer frame has none.
                                let correlate = match other {
                                    ClientFrame::Chassis(ChassisMsg::Request { id, .. }) => id.clone(),
                                    _ => String::new(),
                                };
                                let msg = ChassisServerMsg::Response {
                                    correlate,
                                    outcome: Outcome::refused(Refusal::new(
                                        envelope::codes::UNAUTHORIZED,
                                        "unauthorized: send an auth frame first",
                                    )),
                                };
                                if send(&mut tx, &msg).await.is_err() { break; }
                            }
                            ClientFrame::Chassis(ChassisMsg::Request { id, method, params }) => {
                                // **THE RECEIVER IS CREATED HERE — BEFORE THE
                                // ITEM IS ENQUEUED** — so the export cannot be
                                // emitted into a channel this connection is not
                                // yet listening on. Moving this below the
                                // dispatch loses the first export of every
                                // cold subscribe, silently.
                                if crate::server::route(CHASSIS_METHODS, &method)
                                    .is_some_and(|r| r.template == "shapes/{shape}/subscribe")
                                    && shapes_rx.is_none()
                                {
                                    shapes_rx = Some(producer.hub().connect(conn_id));
                                }
                                let outcome =
                                    dispatch(
                                        &producer,
                                        conn_id,
                                        &method,
                                        params,
                                        &mut shape_subs,
                                        &mut shape_members,
                                    )
                                        .await;
                                let msg = ChassisServerMsg::Response { correlate: id, outcome };
                                if send(&mut tx, &msg).await.is_err() { break; }
                            }
                            ClientFrame::Producer { ty, raw } => {
                                match control.client_frame(&ty, &raw) {
                                    Some(frames) => {
                                        let mut broke = false;
                                        for f in frames {
                                            if send_text(&mut tx, f).await.is_err() {
                                                broke = true;
                                                break;
                                            }
                                        }
                                        if broke { break; }
                                    }
                                    None => {
                                        let msg = ChassisServerMsg::Response {
                                            correlate: String::new(),
                                            outcome: Outcome::refused(Refusal::malformed(
                                                format!("invalid frame: unknown type `{ty}`"),
                                            )),
                                        };
                                        if send(&mut tx, &msg).await.is_err() { break; }
                                    }
                                }
                            }
                            ClientFrame::Invalid(why) => {
                                let msg = ChassisServerMsg::Response {
                                    correlate: String::new(),
                                    outcome: Outcome::refused(Refusal::malformed(why)),
                                };
                                if send(&mut tx, &msg).await.is_err() { break; }
                            }
                        }
                    }
                }
            }
            // ── THE SECOND ARM: the producer's own stream, gated ───────────
            //
            // `authed` is passed IN rather than checked here, because the
            // producer has to keep consuming while it is false and only it
            // knows what "delivered" means for its own cursors.
            ef = stream.next(authed) => {
                match ef {
                    ExtraFrame::Send(text) => {
                        if send_text(&mut tx, text).await.is_err() { break; }
                    }
                    ExtraFrame::Closed => break,
                }
            }
            // ── THE THIRD ARM: shape frames, gated ────────────────────────
            //
            // The invariant, stated so a reviewer can attack it: **a connection
            // receives a `shape` frame IFF it has already received its own
            // export for that `(shape, key)` on this connection.** `shape_subs`
            // is populated by NOTHING except an export addressed to this
            // connection, so there is no path by which an unasked-for frame is
            // sent.
            sf = async {
                match shapes_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            }, if shapes_rx.is_some() => {
                if !authed { continue; }
                match sf {
                    Some(f) => {
                        let send_it = match f.to {
                            // MY export. The only thing that ever inserts into
                            // `shape_subs`.
                            Some(c) if c == conn_id => {
                                if let Some(k) = f.key.clone() {
                                    shape_subs.insert((f.shape.clone(), k));
                                }
                                // **A MEMBERSHIP EXPORT OPENS THE MEMBERSHIP
                                // GATE**, exactly as a keyed export opens that
                                // key's. It is the only thing that ever inserts
                                // here, so an `added` nobody asked for still
                                // cannot be sent.
                                if f.wire.members.is_some() {
                                    shape_members.insert(f.shape.clone());
                                }
                                true
                            }
                            // ANOTHER CONNECTION'S EXPORT, WHICH CAN NO LONGER
                            // ARRIVE HERE. A directed frame goes down its own
                            // connection's channel and nowhere else — that is the
                            // capacity-isolation fix — so this arm is unreachable
                            // rather than merely correct, and it says so instead
                            // of leaving a filter that looks load-bearing.
                            Some(other) => {
                                debug_assert!(
                                    false,
                                    "a frame addressed to {other} reached {conn_id}'s own \
                                     channel; `Hub::deliver` is the only sender and it \
                                     addresses one connection",
                                );
                                false
                            }
                            // AN UNDIRECTED FRAME — a derived event for one key,
                            // owed to whoever holds it, OR to whoever holds
                            // standing MEMBERSHIP interest in the shape.
                            //
                            // **THE SECOND HALF IS NEW, AND IT IS WHAT MAKES
                            // `detail: keys` A SUBSCRIPTION RATHER THAN A
                            // SNAPSHOT.** A membership subscriber holds no key
                            // — it asked what exists, not what any row says —
                            // so the key gate alone would have delivered it a
                            // roster and then nothing, for ever. That is
                            // `docs/shapes.md` §7.2's open gap: a keyless
                            // subscribe enumerated the keys of that moment and
                            // was blind to every later one.
                            None => match f.key.clone() {
                                Some(k) => {
                                    shape_subs.contains(&(f.shape.clone(), k))
                                        || (shape_members.contains(&f.shape)
                                            && matches!(
                                                f.wire.event,
                                                Some(crate::wire::Derived::Added)
                                                    | Some(crate::wire::Derived::Gone)
                                            ))
                                }
                                None => {
                                    shape_subs.iter().any(|(s, _)| *s == f.shape)
                                        || shape_members.contains(&f.shape)
                                }
                            },
                        };
                        if send_it
                            && send(&mut tx, &ChassisServerMsg::Shape(f.wire.clone()))
                                .await
                                .is_err()
                        {
                            break;
                        }
                        // A bounded `mpsc` has no `Lagged`: the producer drops what
                        // it cannot fit and counts it, because a shape task must
                        // never await one consumer's drain. Read the counter after
                        // each frame — prompt by construction, since a channel is
                        // full only when there are frames to read.
                        let dropped = shapes_rx.as_ref().map(|r| r.take_lost()).unwrap_or(0);
                        if dropped > 0 {
                            tracing::warn!(
                                dropped,
                                conn = conn_id,
                                "this connection's shape channel overflowed — re-exporting held keys"
                            );
                            // **THE NEGOTIATED ASK IS CARRIED THROUGH**, and
                            // **THE ROSTER IS REPAIRED TOO.** See
                            // `Hub::resubscribe_lagged` for both defects: routing
                            // this through `Hub::subscribe` handed a consumer that
                            // had just fallen behind `Retain::Max` and `Beat::Max`,
                            // and walking `shape_subs` alone left a `detail: keys`
                            // subscriber — which holds no key — with no repair at
                            // all.
                            for (shape, key) in shape_subs.iter() {
                                let _ = producer.hub().resubscribe_lagged(
                                    shape,
                                    Some(key),
                                    conn_id,
                                    crate::wire::Detail::Rows,
                                );
                            }
                            for shape in shape_members.iter() {
                                let _ = producer.hub().resubscribe_lagged(
                                    shape,
                                    None,
                                    conn_id,
                                    crate::wire::Detail::Keys,
                                );
                            }
                        }
                    }
                    // The hub dropped this connection's sink, which only happens
                    // once `hub.dropped(conn)` has run — i.e. after this loop.
                    None => { shapes_rx = None; }
                }
            }
            _ = ping.tick() => {
                if tx.send(Message::Ping(Vec::new())).await.is_err() { break; }
            }
        }
    }
    // NO `hub.dropped` CALL HERE, AND THAT IS THE POINT. It lives in
    // `ConnectionGuard::drop` at the top of this function, so the panic and
    // abort paths — which skip this line entirely — close their subscriptions
    // too.
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. Dispatch
// ─────────────────────────────────────────────────────────────────────────────

/// Route one request to an [`Outcome`]. The chassis's own methods first, the
/// producer's after.
///
/// **This is the one place every refusal on this daemon passes through**, which
/// is why R1 — typed error codes — folds in with this move rather than needing
/// its own. A chassis refusal is typed by construction; a producer refusal is
/// typed when the producer's error implements [`Coded`].
async fn dispatch<P: Producer>(
    producer: &Arc<P>,
    conn: ConnId,
    method: &str,
    params: Value,
    shape_subs: &mut HashSet<(String, String)>,
    shape_members: &mut HashSet<String>,
) -> Outcome {
    // ── THE ROUTE IS TAKEN ONCE, OVER THE UNION INVENTORY ────────────────
    //
    // **UNKNOWN METHOD IS ANSWERED STRUCTURALLY, NOT BY PROSE.**
    //
    // A producer's catch-all arm answers the same sentence, but as an `Error`
    // indistinguishable from every other bad-request — so typing it would have
    // meant matching on the English, which is the exact failure R1 exists to
    // end. The chassis holds the inventory, so the chassis can answer this from
    // the inventory.
    //
    // Since 2026-09-28 the inventory is PATH TEMPLATES, so this is also where
    // the subject comes off the path. What reaches the producer is the
    // TEMPLATE — `threads/{thread_id}/send` — so its match arms stay fixed
    // literals and the variable segment never reaches them as text.
    let Some(routed) = route(CHASSIS_METHODS, method).or_else(|| route(producer.names(), method))
    else {
        return Outcome::refused(Refusal::new(
            envelope::codes::UNKNOWN_METHOD,
            format!("unknown method: {method}"),
        ));
    };
    let mut params = params;
    if let Err(r) = bind_subject(&mut params, &routed) {
        return Outcome::refused(r);
    }
    match chassis_method(
        producer.as_ref(),
        conn,
        routed.template,
        &params,
        shape_subs,
        shape_members,
    ) {
        Some(Ok(payload)) => Outcome::ok(payload),
        Some(Err(r)) => Outcome::refused(r),
        None => match producer.handle(routed.template, params).await {
            Ok(payload) => Outcome::ok(payload),
            Err(r) => Outcome::refused(r),
        },
    }
}

/// **THE CHASSIS'S OWN METHODS.** `None` when `method` is not one of them, so
/// the caller falls through to the producer unchanged.
///
/// `shape_subs` is this CONNECTION's gate set, threaded in because
/// `shapes.unsubscribe` has to move it — see that arm. [`CHASSIS_METHODS`]'s own
/// doc already said these two *"need this connection's id and its receiver"*; the
/// gate set is the third thing of that kind and was the one nobody passed.
fn chassis_method<P: Producer>(
    producer: &P,
    conn: ConnId,
    method: &str,
    params: &Value,
    shape_subs: &mut HashSet<(String, String)>,
    shape_members: &mut HashSet<String>,
) -> Option<Result<Value, Refusal>> {
    match method {
        // **RULE 2.7's THIRD STAGE.** The whole rule is
        // [`crate::command::barrier`]; this arm is the route to it. Nothing
        // about it touches this connection — a barrier is about the producer's
        // STORE, so two connections asking for one get the same commit — and it
        // is a chassis method because the ORDER of its refusals is the rule,
        // not a producer's preference.
        //
        // **IT BLOCKS THIS TASK FOR THE LENGTH OF AN FSYNC**, on purpose and
        // like every other store call in this daemon: the alternative is a
        // `spawn_blocking` whose completion a caller could observe before the
        // commit landed, which is the one thing a barrier may not do.
        crate::command::BARRIER_METHOD => {
            Some(crate::command::barrier(producer.durable(), params))
        }
        "shapes/list" => {
            // Reads each shape task's `watch::Sender<ShapeInfo>`. No cell is
            // read from outside the task that owns it.
            //
            // **`durable` RIDES HERE AS A SIBLING OF `shapes`** — rule 2.8's
            // last clause, *"every ceiling rides `shapes.list`, so a consumer
            // can read the contract before it asks"*. It is not inside any one
            // shape because durability is the STORE's and no shape has its own,
            // and it is ABSENT rather than null when this producer promises none
            // — an absent contract is the same claim `durable/barrier` makes
            // when it refuses `unsupported`, read one round trip earlier.
            let durable = producer
                .durable()
                .map(crate::command::DurableContract::of);
            let mut payload = json!({ "shapes": producer.hub().list() });
            if let Some(d) = durable {
                payload["durable"] = serde_json::to_value(d)
                    .expect("a DurableContract is one integer");
            }
            Some(Ok(payload))
        }
        // The inline resource door. It needs neither `conn` nor the gate set: a
        // read is a pull with no subscription behind it, so there is nothing per
        // connection to remember — which is also why it is idempotent and why a
        // retry is the identical request.
        "resource/{id}/read" => Some(crate::resource::read_resource_inline(producer, params)),
        "shapes/{shape}/subscribe" => {
            let Some(shape) = params.get("shape").and_then(Value::as_str) else {
                return Some(Err(Refusal::malformed(
                    "missing required param: shape (a string)",
                )));
            };
            let key = params.get("key").and_then(Value::as_str);
            // ── HOW MUCH OF EACH KEY ───────────────────────────────────────
            //
            // **ABSENT MEANS THE CEILING** — rule 2.8's second clause. `rows`
            // is the ceiling, so every consumer written before this parameter
            // existed keeps getting exactly what it got, and one that does not
            // care never has to learn the knob. An unknown spelling is REFUSED
            // rather than defaulted: a consumer that asked for something this
            // producer does not serve must be told, not quietly given rows.
            let Some(detail) = crate::wire::Detail::parse(
                params.get("detail").and_then(Value::as_str),
            ) else {
                return Some(Err(Refusal::malformed(
                    "`detail` must be `keys` or `rows`; omit it for `rows`, which is \
                     the ceiling",
                )));
            };
            // ── HOW MUCH OF EACH WINDOWED CELL ─────────────────────────────
            //
            // **ABSENT MEANS THE CEILING** — rule 2.8's second clause again.
            // `"max"` says it out loud; a non-negative integer is `COUNT(n)`.
            // Anything else is refused rather than defaulted, for the same
            // reason `detail` is: a consumer that asked for something this
            // producer does not serve must be told, not quietly given the
            // ceiling. What the ceiling IS rides on `shapes.list` as
            // `max.retained_items`.
            let Some(retain) = crate::wire::Retain::parse(params.get("retain")) else {
                return Some(Err(Refusal::malformed(
                    "`retain` must be `max` or a non-negative integer; omit it for \
                     `max`, which is the ceiling. `max` means everything this shape \
                     is currently RETAINING, never everything that ever happened — \
                     read `max.retained_items` off `shapes.list` for the number",
                )));
            };
            // ── HOW OFTEN, AND IT IS THE FIRST RATE CEILING HERE ───────────
            //
            // **ABSENT MEANS THE CEILING** — rule 2.8's second clause once more,
            // and on this axis the ceiling is the FASTEST the shape goes, which
            // makes the default the most expensive delivery the contract allows.
            // That is the cost a rate ceiling has and a quantity ceiling does
            // not: `retain`'s default is free to the producer because the window
            // already exists, and this one is paid for as long as the
            // subscription lasts. `"max"` says it out loud; a non-negative
            // integer is milliseconds between beats. What the floor IS rides on
            // `shapes.list` as `max.beat_interval_ms`.
            let Some(beat) = crate::wire::Beat::parse(params.get("beat")) else {
                return Some(Err(Refusal::malformed(
                    "`beat` must be `max` or a non-negative integer of \
                     milliseconds; omit it for `max`, which is the ceiling. A \
                     rate ceiling is a FLOOR on the interval, so a SMALLER \
                     number is the ask that gets refused — read \
                     `max.beat_interval_ms` off `shapes.list` for the number",
                )));
            };
            // ── WHO IS ASKING ──────────────────────────────────────────────
            //
            // Optional on the wire, and refused when present but half-filled.
            // Parsed BEFORE the subscribe so a malformed identity refuses the
            // request rather than opening a subscription the ledger cannot
            // name. Nothing compares `consumer.shape_version` to anything: it is
            // a claim recorded for a human, not a handshake — see
            // `crate::ledger`'s module doc, whose ruling on that field is
            // untouched by the `version` param below.
            // ── HOW MANY ROWS, AND FROM WHERE — THE PAGING CHAIN ───────────
            //
            // `crate::wire::Paging`: absent → the producer's published default,
            // `null` → no pagination (the ceiling by name), a positive integer →
            // that many, refused above the ceiling naming it. `from` is the
            // consumer's own token — the `next` off the last page's final frame.
            let Some(page) = crate::wire::Paging::parse(params.get("page")) else {
                return Some(Err(Refusal::malformed(
                    "`page` must be a positive integer of rows, or `null` for no \
                     pagination; omit it for the producer's default — read `page` \
                     off `shapes.list` for both numbers",
                )));
            };
            let from = match params.get("from") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.as_str()),
                Some(_) => {
                    return Some(Err(Refusal::malformed(
                        "`from` must be a key — the `next` the last page's final \
                         frame carried; omit it for the first page",
                    )))
                }
            };
            // **IDENTITY IS COMPULSORY AT SUBSCRIBE TIME** (rule 2.1, operator
            // ruling 2026-10-08). `parse` no longer has an absent case to
            // return, so this is `Some(v)` rather than the `Option` it used to
            // forward: a subscribe that reaches past this line has a name.
            let identity = match crate::ledger::ConsumerIdentity::parse(params) {
                Ok(v) => Some(v),
                Err(e) => return Some(Err(Refusal::malformed(e))),
            };
            // ── WHICH CONTRACT IT WAS BUILT AGAINST — RULE 2.6 ─────────────
            //
            // **AND THIS ONE IS A HANDSHAKE, WHICH IS WHY IT IS A SEPARATE
            // PARAM FROM `consumer.shape_version`.** Rule 2.1 lists the two side
            // by side (`{shape, key?, at?, consumer, version}`) and they do
            // different jobs: `consumer.shape_version` is free text for a human
            // — *"even if it is a date"* — and `version` is the integer the
            // producer's declared contract is checked against, exactly once, at
            // the link. Collapsing them would mean either gating on free text or
            // refusing every consumer that spells its build as a date, and the
            // ledger's own ruling forbids the first.
            //
            // **ABSENT IS LEGAL and does NOT mean the ceiling.** There is no
            // ceiling here — a version is not a quantity. Absent means *this
            // consumer stated nothing*, which is served and recorded as such,
            // for the reason `ConsumerIdentity::parse` gives about its own
            // optionality: refusing every consumer built before this param
            // existed, the board included, inverts what the chassis is for.
            //
            // **PRESENT AND NOT AN INTEGER IS REFUSED, never defaulted** — the
            // treatment `detail`, `retain` and `beat` each get at this door. A
            // consumer that tried to state a version and failed must be told,
            // because the alternative is a link it believes is checked and is
            // not, which is worse than the gap absence leaves.
            // Parsed by the FLOOR, not here — see `crate::wire::parse_version`:
            // `threads` has its own legacy door beside this one, and a second
            // hand-written copy of this match is a rule reachable from one door and
            // not the other.
            let Some(version) = crate::wire::parse_version(params.get("version")) else {
                return Some(Err(Refusal::malformed(crate::wire::VERSION_PARAM_REFUSAL)));
            };
            // A past `at` is treated as absent: cells hold no history, so "the
            // snapshot reflects exactly the events at or before `at`" is not
            // implementable and `as_of` reports the REAL fold point instead of
            // lying. A future `at` is a bounded timer, and an absurd or
            // overflowing one is refused here rather than queued into a
            // `sleep_until` that would panic the task.
            let at = match params.get("at").and_then(Value::as_str) {
                Some(raw) => match chrono::DateTime::parse_from_rfc3339(raw) {
                    Ok(t) => Some(t.with_timezone(&chrono::Utc)),
                    Err(e) => {
                        return Some(Err(Refusal::malformed(format!(
                            "`at` must be RFC3339: {e}"
                        ))))
                    }
                },
                None => None,
            };
            Some(
                producer
                    .hub()
                    .subscribe_as(
                        shape, key, conn, at, identity, detail, retain, beat, version, page,
                        from,
                    )
                    // Rule 2.7's `accepted` for a subscribe, with a NAME on it: the
                    // export that satisfies this call carries the id in
                    // `ShapeWire::subscribes`. `queued` is kept — see the same arm
                    // in `threads`' own door for the argument.
                    .map(|id| json!({ "queued": true, "subscribe_id": id }))
                    // **A DEAD SHAPE ANSWERS AN ERROR, NEVER `{queued:true}`** —
                    // and after R1 it answers a CODE. The prose form was the
                    // whole reason a consumer could not tell `unknown` from
                    // `dead` from an absurd `at` except by parsing English.
                    .map_err(Refusal::from),
            )
        }
        "shapes/{shape}/unsubscribe" => {
            let Some(shape) = params.get("shape").and_then(Value::as_str) else {
                return Some(Err(Refusal::malformed(
                    "missing required param: shape (a string)",
                )));
            };
            let key = params.get("key").and_then(Value::as_str);
            producer.hub().unsubscribe(shape, key, conn);
            // ── **THE CONNECTION FORGETS THE KEY TOO** ───────────────────────
            //
            // **THE DEFECT THIS FIXES (2026-09-26).** `shape_subs` was inserted
            // into on every export addressed to this connection and REMOVED FROM
            // NOWHERE, and this arm touched only the hub. The task-side audience
            // for an undirected frame is *"any connection watching this shape"*,
            // and the third arm's gate is what narrows that to keys — so while any
            // OTHER subscriber kept the shape tracking, an unsubscribed key's
            // `row` and `gone` frames went on being delivered to a connection that
            // had asked to stop receiving them, forever. Verified over a real
            // socket on both ports before the fix.
            //
            // *"Unsubscribed on the task"* and *"unsubscribed on the connection"*
            // are two events and only one of them happened. Doing both in one
            // statement is what stops them drifting apart again.
            //
            // It also repairs the OVERFLOW path: on a dropped frame the third arm
            // re-subscribes every `(shape, key)` still in this set, so an
            // unsubscribed key used to come back tracked, exported and counted at
            // the next overflow.
            match key {
                Some(k) => {
                    shape_subs.remove(&(shape.to_string(), k.to_string()));
                }
                // "Stop sending me this shape" — the same membership the hub's own
                // keyless arm retains by connection. It closes the MEMBERSHIP
                // gate too: a keyless unsubscribe is the only door a
                // `detail: keys` subscriber has, so leaving `shape_members` set
                // would go on delivering `added`/`gone` to a connection that
                // asked to stop — the same stale-gate defect this arm was
                // written to fix, one field along.
                None => {
                    shape_subs.retain(|(s, _)| s != shape);
                    shape_members.remove(shape);
                }
            }
            Some(Ok(json!({ "ok": true })))
        }
        _ => None,
    }
}

/// The whole method surface of a chassis-served producer: the chassis's own
/// plus the producer's, sorted and deduplicated.
///
/// This is what replaces "the list matches the arms" for the three that moved.
pub fn method_surface<P: Producer>(producer: &P) -> Vec<&'static str> {
    let mut all: Vec<&'static str> = CHASSIS_METHODS
        .iter()
        .copied()
        .chain(producer.names().iter().copied())
        .collect();
    all.sort_unstable();
    all.dedup();
    all
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. Plumbing
// ─────────────────────────────────────────────────────────────────────────────

/// Constant-time byte comparison for the shared-secret check.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn send(
    tx: &mut futures::stream::SplitSink<WebSocket, Message>,
    msg: &ChassisServerMsg,
) -> Result<(), ()> {
    let json = serde_json::to_string(msg).map_err(|_| ())?;
    send_text(tx, json).await
}

async fn send_text(
    tx: &mut futures::stream::SplitSink<WebSocket, Message>,
    json: String,
) -> Result<(), ()> {
    tx.send(Message::Text(json)).await.map_err(|_| ())
}

/// A producer's error, mapped to a [`Refusal`] through its [`Coded`] impl.
///
/// Offered as a free function rather than a blanket `impl From` so a producer
/// with its own `From` conversions is not fighting a coherence rule.
pub fn refuse<E: Coded + std::fmt::Display>(e: E) -> Refusal {
    Refusal::new(e.code(), e.to_string())
}

// ═════════════════════════════════════════════════════════════════════════════
#[cfg(test)]
mod path_grammar_tests {
    //! **The grammar, from the parser's side.** The producer-side half — that
    //! `threads`' own inventory is well formed and that no family shadows a key
    //! — lives beside that inventory, in `threads::server`.

    use super::*;

    const INV: &[&str] = &[
        "gauge/probe",
        "accounts/login/begin",
        "accounts/{account}/disable",
        "accounts/login/{session_id}/complete",
        // **DELIBERATELY SHADOWING**, and it is the only reason the ordering is
        // observable at all: this pair is exactly what
        // `threads::server::no_family_shadows_a_key` forbids in the real
        // inventory. Here it is on purpose, so a parse that preferred the
        // SHORTEST family would resolve `accounts/login/s-7/complete` to this
        // template with the subject `login/s-7`, and the test below would see it.
        "accounts/{account}/complete",
        "shapes/{shape}/subscribe",
    ];

    /// **A SUBJECT WITH A `/` IN IT SURVIVES THE PATH**, which is the whole
    /// reason the parse runs right-to-left.
    ///
    /// `threads/summary` is a real shape name with a real slash in it, so
    /// `shapes/threads/summary/subscribe` has a TWO-SEGMENT subject and no
    /// left-to-right parse could find its boundary. The family set and the leaf
    /// set are both closed, so taking the leaf off the end and the longest known
    /// family off the front leaves exactly the subject.
    ///
    /// **Forced red** by parsing left-to-right (taking `segs[1]` as the whole
    /// subject): `assertion left == right failed: left: "threads" right: "threads/summary"`.
    #[test]
    fn a_subject_containing_a_slash_survives_the_path() {
        let r = route(INV, "shapes/threads/summary/subscribe").expect("a route");
        assert_eq!(r.template, "shapes/{shape}/subscribe");
        assert_eq!(r.subject.unwrap().1, "threads/summary");
    }

    /// **THE LONGEST FAMILY WINS, so a subjectless template beats a
    /// subject-bearing one.**
    ///
    /// `accounts/login/begin` and `accounts/{account}/disable` sit in the same
    /// family tree; only the ordering makes the first one reachable. And a
    /// SESSION id still resolves under the deeper family.
    ///
    /// **Forced red** by scanning `k` upwards instead of downwards
    /// (`for k in 1..segs.len()`):
    /// `assertion left == right failed: left: "accounts/{account}/complete" right: "accounts/login/{session_id}/complete"`.
    #[test]
    fn the_longest_family_wins() {
        let r = route(INV, "accounts/login/begin").expect("a route");
        assert_eq!(r.template, "accounts/login/begin");
        assert!(r.subject.is_none(), "{r:?}");

        let r = route(INV, "accounts/login/s-7/complete").expect("a route");
        assert_eq!(r.template, "accounts/login/{session_id}/complete");
        assert_eq!(r.subject.unwrap(), ("session_id", "s-7".to_string()));

        // And an ordinary account still routes under the shallow family.
        let r = route(INV, "accounts/alice-example-com/disable").expect("a route");
        assert_eq!(r.template, "accounts/{account}/disable");
        assert_eq!(r.subject.unwrap().1, "alice-example-com");
    }

    /// **A LEAF ONLY ROUTES UNDER ITS OWN FAMILY**, and a bare noun is not a
    /// method at all.
    ///
    /// **THIS TEST WAS REWRITTEN BECAUSE ITS MUTATION CAME BACK GREEN.** Its
    /// first version asserted only that malformed and one-segment paths do not
    /// route, and dropping the `segs.len() < 2` guard did not break it: the
    /// loop's range is `1..segs.len()`, which is EMPTY for a one-segment path,
    /// so the guard is defensive and its removal changes no outcome. Every
    /// malformed spelling in that list was the same — refused by the family
    /// comparison, not by the guard. A test that cannot fail is worse than no
    /// test, so this asserts the thing the parse actually decides.
    ///
    /// The root rule itself is not untested: it is enforced on the TEMPLATE side
    /// by `MethodPath::parse`, and `threads::server`'s
    /// `every_template_is_well_formed_and_nothing_is_at_the_root` forces that
    /// red.
    ///
    /// **Forced red** by dropping the `m.family != family` term from the match:
    /// `daemon/probe is not gauge/probe: Some(Routed { template: "gauge/probe", subject: None })`.
    #[test]
    fn a_leaf_only_routes_under_its_own_family() {
        // The leaf exists; the family does not own it.
        assert!(
            route(INV, "daemon/probe").is_none(),
            "daemon/probe is not gauge/probe: {:?}",
            route(INV, "daemon/probe")
        );
        assert!(
            route(INV, "budgets/aa/disable").is_none(),
            "a budget key is not an account: {:?}",
            route(INV, "budgets/aa/disable")
        );
        // And nothing at the root, nothing malformed.
        for bad in ["probe", "", "/gauge/probe", "gauge/probe/", "gauge//probe", "gauge"] {
            assert!(route(INV, bad).is_none(), "{bad:?} is not a method");
        }
    }

    /// **THE SUBJECT MAY NOT BE WRITTEN DOWN TWICE**, and the refusal names the
    /// path to send instead.
    ///
    /// **Forced red** by making `bind_subject` overwrite instead of refuse:
    /// `two spellings of one subject must be refused, not reconciled: Ok(())`.
    #[test]
    fn the_subject_may_not_be_written_down_twice() {
        let r = route(INV, "accounts/aa/disable").expect("a route");
        let mut p = json!({ "account": "bb" });
        let out = bind_subject(&mut p, &r);
        let Err(e) = out else {
            panic!("two spellings of one subject must be refused, not reconciled: {out:?}");
        };
        assert_eq!(e.code, envelope::codes::MALFORMED);
        assert!(e.message.contains("accounts/aa/disable"), "{}", e.message);

        // And with the params silent, the subject is bound under the name the
        // handler already reads.
        let mut p = json!({ "note": "gone" });
        bind_subject(&mut p, &r).expect("bind");
        assert_eq!(p["account"], json!("aa"));
        assert_eq!(p["note"], json!("gone"));
    }

    /// **THE CHASSIS'S OWN THREE ARE WELL FORMED**, held to the same grammar the
    /// producer's are. A chassis that exempted itself from the law it enforces
    /// would be the one place a subject could still ride the params.
    #[test]
    fn the_chassis_methods_are_well_formed() {
        for t in CHASSIS_METHODS {
            let m = MethodPath::parse(t);
            assert!(!m.family.is_empty(), "{t}");
            assert!(!m.leaf.contains('/'), "{t}");
            assert!(t.split('/').count() >= 2, "{t}");
        }
        assert_eq!(
            MethodPath::parse("shapes/{shape}/subscribe"),
            MethodPath {
                family: "shapes",
                subject: Some("shape"),
                leaf: "subscribe",
            }
        );
    }
}
