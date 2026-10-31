//! # The resource door — bytes over HTTP, by one opaque id
//!
//! **Operator ruling, 2026-09-26:** *"If we're going to have resources by id, I
//! am ok with that GET being a REST operation. It's only one endpoint with a
//! single input param: ID. That's not going to introduce drift. But going over
//! websocket might just be introducing challenges for an arbitrary reason."*
//!
//! This module is that endpoint. It supersedes `docs/chassis-resources.md` §6.4
//! and R8 — resources are NOT leaf shapes reached by `shapes.subscribe`, and
//! there is no `Tracking::Fetch`, no reader pool and no `LeafRoot`. HTTP
//! handlers already run on their own tasks, which is the whole runtime that
//! design was going to build.
//!
//! ## The rule that keeps it safe, and where it is ENFORCED
//!
//! The drift risk was never the transport. It was a parallel NAMESPACE — a
//! second way to NAME a resource, growing filters and selectors beside the shape
//! that already names it. So: **the door accepts exactly one opaque,
//! shape-issued id and never a query.** The shape stays the only namespace; an
//! id reaches this door only by having been read off a summary, and the door can
//! name nothing a shape has not already named.
//!
//! The hazard is the NEXT endpoint, and `docs/chassis-writes.md` §2.2 lists six
//! that will each look reasonable: `?path=/foo/bar`, `?thread=x&seq=y`,
//! `/<id>/versions`, `/<id>/meta`, `/latest/<lineage>`, `?as=json`. Every one is
//! refused, and the refusal is structural rather than documented in two places:
//!
//! * [`ResourceId`] has a private field and **exactly one constructor**,
//!   [`opaque_id`], which is where the no-query rule lives. Every handler in
//!   this module must produce a `ResourceId` before it can do anything, and
//!   [`Resources::fetch`] takes one, so there is no path from a URI to a
//!   producer that does not pass through that one refusal.
//! * That constructor refuses a non-empty query string with
//!   [`codes::MALFORMED`] rather than ignoring it, because an ignored query is
//!   how `?path=` gets added later and observed to "work".
//! * An id is ONE path segment of a closed charset — no `/`, no `%`, no `?`,
//!   `#`, `&` or `=` — so `/<id>/versions` cannot be spelled as an id and a
//!   percent-encoded separator cannot smuggle a path in. This is
//!   `chassis-resources.md` §4.4 item 10's `KEY_SEPARATOR` reservation, made for
//!   ids.
//!
//! ## What a producer implements
//!
//! [`Resources`] — one object-safe trait, defaulting to `None` on
//! [`crate::server::Producer::resources`], so a producer with no resources is
//! entirely unaffected and the route says so honestly ([`codes::UNSUPPORTED`],
//! 501) rather than answering a 404 that is indistinguishable from no route.
//!
//! **This is NOT `chassis-resources.md` §8.1's `Serves<R>`, and the difference
//! is forced by the ruling.** `Serves<R>` is generic over one resource type, so
//! a missing impl is a compile error at the shape that declares it. A door whose
//! only input is an OPAQUE id has no way to know which `R` an id belongs to —
//! the type cannot be recovered from the segment without the id encoding it,
//! which would be id construction by a rule the consumer must know. So the
//! producer dispatches on the id, and the `Handles<R>`-style compile-time proof
//! survives only where the SHAPE declares a resource, not at the door.
//!
//! ## Deliver-or-refuse, never deliver-and-warn
//!
//! Rule 2.4's one-source argument, applied to bytes: a response that carries
//! bytes AND a caveat about them is two claims about one resource, and the
//! consumer must then decide which to believe. So:
//!
//! * A `Range` that lies outside the resource is **refused** (416,
//!   [`codes::RANGE_UNSATISFIABLE`]) and never clamped, which is a deliberate
//!   departure from RFC 9110 §14.1.1's "take the last-byte-pos to be one less
//!   than the current length". A clamped range answers 206 with fewer bytes than
//!   were asked for, which tells the consumer its summary's `bytes` is wrong by
//!   implication — a second source for a number the summary states outright.
//!   416 tells it to re-read the row instead.
//! * The 416 carries **no `Content-Range: bytes */len`**, though RFC 9110 SHOULD
//!   generate one. That header is the resource's length, and the summary is the
//!   one place a resource's length is stated.
//! * A producer that positions its reader past the byte the range asked for is a
//!   producer defect and is refused [`codes::INTEGRITY`], not served from the
//!   wrong offset. See [`Found::offset`].

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::envelope::{codes, Refusal};
use crate::server::{Chassis, Producer};

// ─────────────────────────────────────────────────────────────────────────────
// 1. The bounds — every one of them, named
// ─────────────────────────────────────────────────────────────────────────────

/// The longest id this door will look at. Longer is [`codes::MALFORMED`] — an
/// id is a store handle or a digest, and neither is kilobytes.
pub const MAX_RESOURCE_ID_BYTES: usize = 512;

/// **THE STREAMING UNIT.** A GET reads and forwards this much at a time and
/// never the whole resource, so a 2 GB resource costs the same resident memory
/// as a 2 KB one.
pub const RESOURCE_CHUNK_BYTES: usize = 64 * 1024;

/// How many chunks may sit between the reader and the socket. This times
/// [`RESOURCE_CHUNK_BYTES`] plus one in-hand chunk is the **entire** memory one
/// in-flight GET can hold, whatever the resource's size.
pub const RESOURCE_CHUNKS_IN_FLIGHT: usize = 4;

/// **THE PER-DAEMON READ CAP**, refused as [`codes::BUSY`] (429).
///
/// Each in-flight GET holds one blocking-pool thread for as long as its consumer
/// takes to drain — the reader blocks on a full channel, which is what makes a
/// slow consumer slow only itself. Without a cap, enough slow consumers exhaust
/// tokio's blocking pool and every OTHER blocking call on the daemon (every
/// hydrate) stalls behind them. Reserved before the read starts, for the reason
/// `MAX_ARMED_TIMERS_PER_CONN` records: a cap discovered after the ack is an ack
/// that means nothing.
pub const MAX_INFLIGHT_RESOURCE_READS: usize = 64;

/// **THE UPLOAD CAP**, refused as [`codes::TOO_LARGE`] (413) carrying `limit`.
///
/// A PUT is buffered whole, because the digest is verified BEFORE the producer
/// is handed anything (§4.2) and a stream cannot be verified after it has been
/// stored. That buffering is what makes the cap necessary and it is why a blob
/// larger than this is chunked instead — chunking and manifests are
/// `chassis-writes.md` §4.3's and not this door's.
pub const MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

/// **THE MOST THIS PRODUCER WILL ANSWER INLINE OVER THE WEBSOCKET**, refused as
/// [`codes::TOO_LARGE`].
///
/// A websocket frame is read whole into memory by every client there is, and the
/// chassis sends it as one text frame — so unlike [`get_resource`], which
/// streams in [`RESOURCE_CHUNK_BYTES`] chunks and never holds a resource, the
/// inline door's resident cost IS the payload. This is the bound on that.
///
/// **It is NOT the consuming chassis's threshold and the two are not
/// coordinated.** `mirror`'s `WS_INLINE_CEILING_BYTES` is one consumer's
/// judgement about its own frame buffer; this is one producer's bound on its own
/// memory. Each half bounds what it pays for and neither publishes its number:
/// the operator's reason is that a published number becomes a thing consumers
/// depend on and can then never be changed. So this rides on no `ShapeMax`, no
/// `shapes.list` field and no frame — a consumer learns it only by exceeding it,
/// which is a refusal naming the limit, exactly as an upload does.
///
/// **It is `pub` and that does not publish it**, which is worth a sentence
/// because it looks like it should. This whole module is behind the `server`
/// feature — what a PRODUCER adds — so a mirror build
/// (`--no-default-features`, which `check-features.sh` proves links neither
/// axum nor tokio) cannot name this constant at all. It is visible to this
/// producer and to this producer's own tests, and to nothing that consumes it.
///
/// 1 MiB, an eighth of [`MAX_UPLOAD_BYTES`]: a read is answered on the
/// connection's own frame channel, which also carries every shape frame that
/// connection is owed, so an inline read is charged against a budget a PUT is
/// not.
pub const MAX_WS_INLINE_BYTES: u64 = 1024 * 1024;

/// `Cache-Control` for a resource whose bytes can never change under this id.
const CACHE_IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// `Cache-Control` for a MUTABLE lineage's current id — see [`Retention`].
const CACHE_REVALIDATE: &str = "no-cache";

// ─────────────────────────────────────────────────────────────────────────────
// 2. Retention — the operator's three kinds on one axis
// ─────────────────────────────────────────────────────────────────────────────

/// **THE THREE KINDS, STORED AS ONE AXIS** (`chassis-resources.md` §2.3, R7).
///
/// Immutable is Versioned with exactly one version; Mutable is Versioned with
/// retention zero. **Deletion is orthogonal to all three** — every kind can be
/// deleted, and a deleted id answers [`codes::GONE`] whatever its retention. The
/// operator's three NAMES are what ride the wire; the axis is what the chassis
/// stores.
///
/// ## The one thing the chassis does with it, and why it is not decorative
///
/// `Cache-Control`. The bytes behind any id are immutable — a mutable
/// resource's mutability lives in the entry row's POINTER, never in the bytes
/// behind an id — so a naive reading makes all three cacheable forever. That is
/// wrong for exactly one of them:
///
/// | Kind | `Cache-Control` | Why |
/// |---|---|---|
/// | `Fixed` / `All` | `public, max-age=31536000, immutable` | the id names one blob for ever; a cache need never revalidate |
/// | `Latest` | `no-cache` | the bytes cannot change, but the ID can be SUPERSEDED. A cache told `immutable` keeps serving bytes for an id the world has moved past and never learns it 410s — which makes the cache a second source for the pointer. `no-cache` stores the entry and revalidates, so a superseded id is refused instead of served |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Retention {
    /// Versioned: every version stays fetchable under its own id.
    #[serde(rename = "versioned")]
    All,
    /// Mutable, not versioned: the id changes on every write and a superseded id
    /// is REFUSED, never served.
    #[serde(rename = "mutable")]
    Latest,
    /// Immutable: one id, for ever — until it is deleted.
    #[serde(rename = "immutable")]
    Fixed,
}

impl Retention {
    fn cache_control(self) -> &'static str {
        match self {
            Retention::All | Retention::Fixed => CACHE_IMMUTABLE,
            Retention::Latest => CACHE_REVALIDATE,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. The id — one opaque segment, and the only place a query can be seen
// ─────────────────────────────────────────────────────────────────────────────

/// **ONE OPAQUE, SHAPE-ISSUED ID.**
///
/// The field is private and [`opaque_id`] is the only constructor. A producer
/// implementing [`Resources`] receives `&ResourceId` and therefore cannot forge
/// one, and no handler in this module can reach a producer without one — which
/// is what makes "never a query" a property of the code rather than a rule in a
/// document.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResourceId(String);

impl ResourceId {
    /// The segment as the shape issued it. Opaque to the chassis: it is never
    /// parsed, split, or interpreted here.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ResourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The characters an id may contain: URL-unreserved plus the three separators
/// the design's own id spellings use (`<tid>.<seq>`, base64url, `scheme:key`).
///
/// **What is absent is the point.** No `/` (a path), no `%` (a smuggled `/`),
/// no `?`, `&` or `=` (a query), no `#`. `GET /resource?path=/foo/bar` cannot be
/// spelled, and neither can an id that decodes into one.
fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b':' | b'@' | b'+')
}

/// **THE ONE GATE.** Turn a path segment plus whatever came after the `?` into a
/// [`ResourceId`], or refuse.
///
/// The query is REFUSED and never ignored. An ignored query is how the next
/// endpoint arrives: someone adds `?path=`, observes that the request succeeds,
/// and the parallel namespace exists before anybody reviews it.
fn opaque_id(segment: String, query: Option<String>) -> Result<ResourceId, Refusal> {
    if let Some(q) = query {
        if !q.is_empty() {
            return Err(Refusal::malformed(format!(
                "the resource endpoint takes one opaque id and no query; got `?{q}`. \
                 A path, a lineage, a version index and a rendering are all things \
                 the SHAPE names — read the id off its summary"
            )));
        }
    }
    if segment.is_empty() {
        return Err(Refusal::malformed("empty resource id"));
    }
    if segment.len() > MAX_RESOURCE_ID_BYTES {
        return Err(Refusal::malformed(format!(
            "resource id is {} bytes; the limit is {MAX_RESOURCE_ID_BYTES}",
            segment.len()
        )));
    }
    if let Some(bad) = segment.bytes().find(|b| !is_id_byte(*b)) {
        return Err(Refusal::malformed(format!(
            "resource id contains a byte outside the id charset (0x{bad:02x}); an id is \
             ONE opaque segment and carries no separator"
        )));
    }
    Ok(ResourceId(segment))
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. A byte range — a transport detail of one idempotent GET
// ─────────────────────────────────────────────────────────────────────────────

/// **A `Range` REQUEST, AND WHY RULE 2.5 IS NOT ENGAGED.**
///
/// Rule 2.5 bans replay and cursors because a cursor makes the PRODUCER hold a
/// position, which forces every consumer to implement resume-after-loss. A
/// `Range` is held by the CONSUMER: the producer keeps nothing between two GETs,
/// the same range always yields the same bytes, and a retry is the identical
/// request. It is `threads.history {after_seq}` with the position where it
/// already was, not a new kind of thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// `bytes=a-b` or `bytes=a-` (to the end).
    From { first: u64, last: Option<u64> },
    /// `bytes=-n` — the last `n` bytes.
    Suffix { len: u64 },
}

impl ByteRange {
    /// Parse a `Range` header value. `Ok(None)` = no range asked for.
    ///
    /// **A multi-range request is refused**, not served: `multipart/byteranges`
    /// is a second body format for one resource, and one endpoint serving two
    /// encodings of the same bytes is the rendering dial §2.2 refuses as
    /// `?as=json`.
    pub fn parse(headers: &HeaderMap) -> Result<Option<Self>, Refusal> {
        let Some(raw) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) else {
            return Ok(None);
        };
        let spec = raw.trim();
        let Some(rest) = spec.strip_prefix("bytes=") else {
            return Err(Refusal::malformed(format!(
                "Range must be `bytes=`-based; got `{spec}`"
            )));
        };
        if rest.contains(',') {
            return Err(Refusal::malformed(
                "one range per request: multipart/byteranges is a second encoding of \
                 one resource and this door serves one",
            ));
        }
        let (a, b) = rest
            .split_once('-')
            .ok_or_else(|| Refusal::malformed(format!("malformed Range: `{spec}`")))?;
        let (a, b) = (a.trim(), b.trim());
        let num = |s: &str| -> Result<u64, Refusal> {
            s.parse::<u64>()
                .map_err(|_| Refusal::malformed(format!("malformed Range: `{spec}`")))
        };
        match (a.is_empty(), b.is_empty()) {
            (true, true) => Err(Refusal::malformed(format!("malformed Range: `{spec}`"))),
            (true, false) => Ok(Some(ByteRange::Suffix { len: num(b)? })),
            (false, true) => Ok(Some(ByteRange::From {
                first: num(a)?,
                last: None,
            })),
            (false, false) => {
                let (first, last) = (num(a)?, num(b)?);
                if last < first {
                    // RFC 9110 says to IGNORE an unsatisfiable-by-syntax range.
                    // Ignoring it means answering 200 with the whole resource to
                    // a consumer that asked for eleven bytes — bytes plus an
                    // unstated correction.
                    return Err(Refusal::malformed(format!(
                        "Range `{spec}` ends before it starts"
                    )));
                }
                Ok(Some(ByteRange::From {
                    first,
                    last: Some(last),
                }))
            }
        }
    }

    /// Resolve against the resource's real length, REFUSING anything outside it.
    ///
    /// Returns the inclusive `[first, last]` actually served.
    fn resolve(self, len: u64) -> Result<(u64, u64), Refusal> {
        let unsat = |what: String| Refusal::new(codes::RANGE_UNSATISFIABLE, what);
        if len == 0 {
            return Err(unsat(
                "the resource is empty; no range over it is satisfiable".to_string(),
            ));
        }
        match self {
            ByteRange::Suffix { len: n } => {
                if n == 0 {
                    return Err(unsat("a zero-length suffix range".to_string()));
                }
                if n > len {
                    return Err(unsat(format!(
                        "a suffix range of {n} bytes over a resource of {len}; the summary \
                         that issued this id states its length — re-read the row"
                    )));
                }
                Ok((len - n, len - 1))
            }
            ByteRange::From { first, last } => {
                if first >= len {
                    return Err(unsat(format!(
                        "range starts at {first} in a resource of {len} bytes; the summary \
                         that issued this id states its length — re-read the row"
                    )));
                }
                match last {
                    // NOT CLAMPED. RFC 9110 would take the end to be `len - 1`;
                    // that answers 206 with fewer bytes than were asked for,
                    // which is the unstated correction rule 2.4 bans.
                    Some(l) if l >= len => Err(unsat(format!(
                        "range ends at {l} in a resource of {len} bytes; this door refuses \
                         a range it would have to shorten rather than serving a short one"
                    ))),
                    Some(l) => Ok((first, l)),
                    None => Ok((first, len - 1)),
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. What a producer hands back
// ─────────────────────────────────────────────────────────────────────────────

/// **ONE RESOLVED ID: its declared kind, its media type, its length, its bytes.**
pub struct Found {
    /// The declared kind. Drives `Cache-Control` and nothing else — see
    /// [`Retention`].
    pub retention: Retention,
    /// The media type the producer declares for these bytes. There is no
    /// negotiation and no `?as=` dial.
    pub content_type: String,
    /// **THE WHOLE RESOURCE'S LENGTH — never the range's.**
    ///
    /// The chassis resolves the requested range against THIS and refuses one
    /// that falls outside it, so a producer cannot clamp a range by reporting a
    /// shorter length than it will serve.
    pub len: u64,
    /// The absolute byte offset `body` begins at.
    ///
    /// **This is how a producer's seek is CHECKED rather than trusted.** A
    /// producer handed a range may position its reader at `range.first` and
    /// report that offset here; one that ignores the range leaves it `0` and the
    /// chassis skips the difference itself. A producer that reports an offset
    /// PAST the first byte asked for has over-seeked, and the chassis refuses
    /// [`codes::INTEGRITY`] rather than serving bytes from the wrong place under
    /// a `Content-Range` that says otherwise.
    pub offset: u64,
    /// The bytes, lazily. Dropped unread on a 304 or a 416, so opening a reader
    /// costs whatever the producer's `open` costs and never a read.
    pub body: Box<dyn Read + Send + 'static>,
}

impl Found {
    /// A whole resource, from byte zero.
    pub fn whole(
        retention: Retention,
        content_type: impl Into<String>,
        len: u64,
        body: Box<dyn Read + Send + 'static>,
    ) -> Self {
        Self {
            retention,
            content_type: content_type.into(),
            len,
            offset: 0,
            body,
        }
    }

    /// The same, with the reader already positioned at `offset`.
    pub fn at_offset(mut self, offset: u64) -> Self {
        self.offset = offset;
        self
    }
}

/// What a PUT did. The distinction is the whole of "idempotent": the second PUT
/// of one digest is not an error, it is [`Stored::AlreadyHeld`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stored {
    Created,
    AlreadyHeld,
}

/// A digest function the CHASSIS can compute, so that verification is the
/// door's and not a producer's promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Digest {
    Sha256,
}

impl Digest {
    /// Lowercase hex of the digest of `bytes`.
    pub fn hex(self, bytes: &[u8]) -> String {
        match self {
            Digest::Sha256 => {
                use sha2::Digest as _;
                let mut h = sha2::Sha256::new();
                h.update(bytes);
                h.finalize().iter().map(|b| format!("{b:02x}")).collect()
            }
        }
    }
    /// The length, in hex characters, an id under this function must have.
    pub fn hex_len(self) -> usize {
        match self {
            Digest::Sha256 => 64,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Digest::Sha256 => "sha256",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. The producer traits
// ─────────────────────────────────────────────────────────────────────────────

/// **THE READ HALF.** Resolve one opaque id to bytes, its declared
/// [`Retention`] and its media type — or refuse, typed.
///
/// Object-safe on purpose: the door's input is an opaque segment, so the
/// producer is the only thing that can say which of its resources an id names.
///
/// **Every refusal here is one of [`codes`]** and the producer picks it, because
/// only the producer's store knows the difference the design draws:
/// [`codes::UNKNOWN`] (never known — the consumer's summary is corrupt or from
/// another producer), [`codes::SUPERSEDED`] (a Mutable id that WAS current),
/// [`codes::GONE`] (existed, deleted), [`codes::FORBIDDEN`],
/// [`codes::INTEGRITY`] (the producer's own bytes do not match its declared
/// digest). A store that cannot tell `gone` from `unknown` answers `unknown`
/// honestly rather than guessing.
pub trait Resources: Send + Sync + 'static {
    /// Resolve `id`. `range` is passed through so a producer over a seekable
    /// store can position its reader; a producer that ignores it is correct and
    /// merely slower — see [`Found::offset`].
    ///
    /// **This is called before the range is validated**, because the length the
    /// range is validated against is [`Found::len`] and only this call knows it.
    /// A refused range drops the reader unread.
    fn fetch(&self, id: &ResourceId, range: Option<ByteRange>) -> Result<Found, Refusal>;

    /// The upload half, when this producer has one. `None` — the default —
    /// makes PUT a 405 [`codes::UNSUPPORTED`].
    ///
    /// The two halves are one `Option` and not two methods so that they cannot
    /// disagree: a producer cannot declare that it accepts uploads and then have
    /// no place to put them.
    fn uploads(&self) -> Option<&dyn Uploads> {
        None
    }
}

/// **THE WRITE HALF — bytes only.**
///
/// The MUTATION that re-points anything at an uploaded digest is a typed
/// websocket command and is not this door's: a PUT here makes the producer HOLD
/// bytes and states nothing about what references them. That is why the ack is
/// not a second source — a staged blob is stated by no shape
/// (`chassis-writes.md` §4.3).
///
/// **Rule 2.7: the PUT's ack is `accepted` and never `durable`.** Understood,
/// verified, will be applied — one stage, asked for by the act of PUTting bytes.
/// Durability is opt-in through a barrier that is another dispatch's, so nothing
/// here fsyncs and the response says `"stage":"accepted"` so that a 201 cannot be
/// read as a survival promise.
pub trait Uploads: Send + Sync + 'static {
    /// The function this producer's content-addressed ids are digests under.
    ///
    /// **This is what makes a PUT verifiable by the CHASSIS**: the door computes
    /// the digest of the bytes it received and compares it to the path segment
    /// before [`Uploads::put`] is ever called. A producer whose ids are durable
    /// store keys (`(thread_id, seq)`) must not implement [`Uploads`] at all —
    /// an id is never CHOSEN by a consumer, it is issued by a shape or computed
    /// from the bytes.
    fn digest(&self) -> Digest;

    /// Keep the blob. **The chassis has already verified that `bytes` hashes to
    /// `digest` under [`Uploads::digest`]** — this is never called otherwise, and
    /// never with a partial body, which is what makes each upload atomic and a
    /// resume free of producer session state.
    ///
    /// Idempotent: a digest already held is [`Stored::AlreadyHeld`], not an
    /// error.
    ///
    /// **THE STAGE THIS RETURNS IS `accepted`, NOT `durable` (rule 2.7).** An
    /// implementation must NOT fsync on this call and must not promise survival
    /// of a crash: `durable` is opt-in through a barrier, and a producer that
    /// quietly paid for it here would be imposing a stage nobody asked for. A
    /// producer that genuinely cannot hold a blob at all REFUSES — it does not
    /// accept the bytes and hope.
    fn put(&self, digest: &str, bytes: Bytes) -> Result<Stored, Refusal>;
}

// ─────────────────────────────────────────────────────────────────────────────
// 7. The read gate
// ─────────────────────────────────────────────────────────────────────────────

/// **THE IN-FLIGHT READ COUNTER.** See [`MAX_INFLIGHT_RESOURCE_READS`].
#[derive(Default)]
pub struct ReadGate {
    inflight: AtomicUsize,
}

impl ReadGate {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn inflight(&self) -> usize {
        self.inflight.load(Ordering::Acquire)
    }
    /// Reserve a slot, or `None` when the cap is reached.
    fn reserve(self: &std::sync::Arc<Self>) -> Option<ReadPermit> {
        let mut cur = self.inflight.load(Ordering::Acquire);
        loop {
            if cur >= MAX_INFLIGHT_RESOURCE_READS {
                return None;
            }
            match self.inflight.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(ReadPermit {
                        gate: std::sync::Arc::clone(self),
                    })
                }
                Err(seen) => cur = seen,
            }
        }
    }
}

/// Released on drop, which includes the pump task ending because the consumer
/// went away mid-stream.
struct ReadPermit {
    gate: std::sync::Arc<ReadGate>,
}

impl Drop for ReadPermit {
    fn drop(&mut self) {
        self.gate.inflight.fetch_sub(1, Ordering::AcqRel);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 8. Refusals, as HTTP
// ─────────────────────────────────────────────────────────────────────────────

/// A typed refusal on its way out of the door, with the status its code maps to
/// and whatever fields the design says that code carries.
struct Denied {
    status: StatusCode,
    refusal: Refusal,
    extras: BTreeMap<String, Value>,
    headers: Vec<(header::HeaderName, HeaderValue)>,
}

impl Denied {
    fn new(status: StatusCode, refusal: Refusal) -> Self {
        Self {
            status,
            refusal,
            extras: BTreeMap::new(),
            headers: Vec::new(),
        }
    }
    /// The status [`codes`] maps onto — `chassis-writes.md` §2.3's table.
    fn from_refusal(r: Refusal, on_write: bool) -> Self {
        let status = match r.code {
            codes::UNKNOWN => StatusCode::NOT_FOUND,
            // HTTP has ONE word for both, so the code is what distinguishes
            // them. A consumer branches on the code, never on the status.
            codes::SUPERSEDED | codes::GONE => StatusCode::GONE,
            codes::FORBIDDEN => StatusCode::FORBIDDEN,
            codes::MALFORMED => StatusCode::BAD_REQUEST,
            codes::RANGE_UNSATISFIABLE => StatusCode::RANGE_NOT_SATISFIABLE,
            codes::DIGEST_MISMATCH => StatusCode::UNPROCESSABLE_ENTITY,
            codes::TOO_LARGE => StatusCode::PAYLOAD_TOO_LARGE,
            codes::BUSY => StatusCode::TOO_MANY_REQUESTS,
            codes::UNAUTHORIZED => StatusCode::UNAUTHORIZED,
            // 405 where the door exists and will not take a WRITE; 501 where
            // this producer serves no resources at all. One code, because the
            // consumer's move is the same in both — stop asking, it is not about
            // your id — and two statuses because HTTP has words for both.
            codes::UNSUPPORTED if on_write => StatusCode::METHOD_NOT_ALLOWED,
            codes::UNSUPPORTED => StatusCode::NOT_IMPLEMENTED,
            // A producer-side defect. `integrity` on the READ side is bytes on
            // disk disagreeing with the producer's own declared digest.
            codes::INTEGRITY => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self::new(status, r)
    }
    fn with(mut self, k: &str, v: impl Into<Value>) -> Self {
        self.extras.insert(k.to_string(), v.into());
        self
    }
    fn header(mut self, k: header::HeaderName, v: HeaderValue) -> Self {
        self.headers.push((k, v));
        self
    }
}

/// **THE SAME BYTES THE WEBSOCKET REFUSAL CARRIES, PLUS THE FIELDS ITS CODE
/// OWES.**
///
/// A `serde_json::Value` round trip would have sorted the keys — `Map` is a
/// `BTreeMap` here — and a consumer reading a refusal off the HTTP door would
/// have seen a different serialisation from one reading it off the websocket.
/// The three fixed fields are declared in [`Outcome::Error`]'s own order and the
/// extras are flattened AFTER them, which is [`crate::envelope::HealthWire`]'s
/// construction for the same reason.
#[derive(Serialize)]
struct DeniedBody<'a> {
    #[serde(rename = "type")]
    ty: &'static str,
    error: &'a str,
    code: &'a str,
    #[serde(flatten)]
    extras: &'a BTreeMap<String, Value>,
}

impl Denied {
    fn body(&self) -> DeniedBody<'_> {
        DeniedBody {
            ty: "error",
            error: &self.refusal.message,
            code: self.refusal.code,
            extras: &self.extras,
        }
    }
}

impl IntoResponse for Denied {
    fn into_response(self) -> Response {
        let mut res = (self.status, axum::Json(self.body())).into_response();
        for (k, v) in self.headers {
            res.headers_mut().insert(k, v);
        }
        res
    }
}

/// The refusal a producer with no resource door gets, once, in one place.
fn no_resources() -> Denied {
    Denied::from_refusal(
        Refusal::new(
            codes::UNSUPPORTED,
            "this producer serves no resources; the route exists on every chassis so \
             that this answer is distinguishable from a missing route and from an \
             unknown id",
        ),
        false,
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// 9. The handlers
// ─────────────────────────────────────────────────────────────────────────────

/// Shared preamble: authenticate, then mint the id. In that order, so an
/// unauthenticated caller learns nothing about which ids parse.
fn admit<P: Producer>(
    chassis: &Chassis<P>,
    headers: &HeaderMap,
    segment: String,
    query: Option<String>,
) -> Result<ResourceId, Denied> {
    chassis
        .bearers()
        .verify(headers)
        .map_err(|r| Denied::from_refusal(r, false))?;
    opaque_id(segment, query).map_err(|r| Denied::from_refusal(r, false))
}

/// `GET /resource/{id}` — the whole resource, or one `Range` of it.
pub(crate) async fn get_resource<P: Producer>(
    State(chassis): State<Chassis<P>>,
    Path(segment): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    match serve(&chassis, &headers, segment, query, true).await {
        Ok(r) => r,
        Err(d) => d.into_response(),
    }
}

/// `HEAD /resource/{id}` — the GET's headers, and no read.
///
/// Registered explicitly rather than left to axum's HEAD-falls-back-to-GET,
/// because that fallback would run the whole pump and throw the bytes away: a
/// probe would cost a full read of the resource.
pub(crate) async fn head_resource<P: Producer>(
    State(chassis): State<Chassis<P>>,
    Path(segment): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    match serve(&chassis, &headers, segment, query, false).await {
        Ok(r) => r,
        Err(d) => d.into_response(),
    }
}

async fn serve<P: Producer>(
    chassis: &Chassis<P>,
    headers: &HeaderMap,
    segment: String,
    query: Option<String>,
    with_body: bool,
) -> Result<Response, Denied> {
    let id = admit(chassis, headers, segment, query)?;
    let Some(resources) = chassis.producer().resources() else {
        return Err(no_resources());
    };

    let range = ByteRange::parse(headers).map_err(|r| Denied::from_refusal(r, false))?;
    let found = resources
        .fetch(&id, range)
        .map_err(|r| Denied::from_refusal(r, false))?;

    // ── THE ETAG IS THE ID ────────────────────────────────────────────────
    //
    // Tautologically, not by obligation: an id names one blob, so the strong
    // validator for that blob IS the id. That is what makes content-addressed
    // caching free — there is nothing to compute and nothing that can disagree.
    let etag = format!("\"{}\"", id.as_str());
    let cache = found.retention.cache_control();

    // ── 304, EVALUATED BEFORE THE RANGE ───────────────────────────────────
    //
    // RFC 9110 §13.2.2's order, and it matters: a conditional ranged GET whose
    // validator still matches is a 304, never a 206. The reader is dropped
    // unread, so a 304 costs the producer's `open` and not one byte of I/O.
    if if_none_match_matches(headers, &etag) {
        drop(found.body);
        let mut res = StatusCode::NOT_MODIFIED.into_response();
        // No body, and none of a body's headers. A 304 that carried
        // `Content-Length` would be describing a body it is forbidden to send.
        res.headers_mut().insert(header::ETAG, hv(&etag)?);
        res.headers_mut().insert(header::CACHE_CONTROL, hv(cache)?);
        return Ok(res);
    }

    let ct = HeaderValue::from_str(&found.content_type).map_err(|_| {
        // A producer-side defect, refused rather than silently replaced with
        // `application/octet-stream`: a corrected media type is a second claim
        // about the bytes.
        Denied::from_refusal(
            Refusal::new(
                codes::INTEGRITY,
                format!(
                    "producer declared a content type that is not a header value: {:?}",
                    found.content_type
                ),
            ),
            false,
        )
    })?;

    let (status, first, last) = match range {
        None => (StatusCode::OK, 0, found.len.saturating_sub(1)),
        Some(r) => {
            let (first, last) = r
                .resolve(found.len)
                .map_err(|e| Denied::from_refusal(e, false))?;
            (StatusCode::PARTIAL_CONTENT, first, last)
        }
    };
    let served = if found.len == 0 { 0 } else { last.saturating_sub(first) + 1 };

    if found.offset > first {
        return Err(Denied::from_refusal(
            Refusal::new(
                codes::INTEGRITY,
                format!(
                    "producer positioned its reader at byte {} for a range starting at \
                     {first}; the bytes behind an id are not seekable backwards here and \
                     serving from the wrong offset under a Content-Range that claims \
                     otherwise is the second source rule 2.4 forbids",
                    found.offset
                ),
            ),
            false,
        ));
    }
    let skip = first.saturating_sub(found.offset);

    let mut res = Response::builder().status(status);
    {
        let h = res.headers_mut().expect("a fresh builder has headers");
        h.insert(header::CONTENT_TYPE, ct);
        h.insert(header::ETAG, hv(&etag)?);
        h.insert(header::CACHE_CONTROL, hv(cache)?);
        h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        // `Content-Length` is the range's length, computed by the chassis from
        // the length the range was validated against — never a number the
        // producer reports about the body it handed over. A producer that
        // clamped a range therefore produces a short body against an honest
        // Content-Length, which every HTTP client reports as truncated.
        h.insert(header::CONTENT_LENGTH, hv(&served.to_string())?);
        if status == StatusCode::PARTIAL_CONTENT {
            h.insert(
                header::CONTENT_RANGE,
                hv(&format!("bytes {first}-{last}/{}", found.len))?,
            );
        }
    }

    if !with_body {
        drop(found.body);
        return Ok(res.body(Body::empty()).expect("an empty body"));
    }

    // ── THE CAP, RESERVED BEFORE THE READ ─────────────────────────────────
    let Some(permit) = chassis.reads().reserve() else {
        drop(found.body);
        return Err(Denied::from_refusal(
            Refusal::new(
                codes::BUSY,
                format!(
                    "{MAX_INFLIGHT_RESOURCE_READS} resource reads are already in flight on \
                     this daemon"
                ),
            ),
            false,
        )
        .header(header::RETRY_AFTER, HeaderValue::from_static("1")));
    };

    Ok(res
        .body(stream_body(found.body, skip, served, permit))
        .expect("a stream body"))
}

/// **STREAMING, AND EXACTLY WHAT IS BOUNDED.**
///
/// A blocking reader pumped on `spawn_blocking` into a channel of
/// [`RESOURCE_CHUNKS_IN_FLIGHT`] chunks of [`RESOURCE_CHUNK_BYTES`]. Resident
/// memory per in-flight GET is that product plus one chunk in hand — 320 KiB —
/// for a resource of ANY size, and the whole resource is never materialised.
///
/// **The backpressure is the channel bound, and it is the right way round.** A
/// consumer that stops draining blocks its own reader on a full channel and
/// nothing else: no shape task waits, no other connection's frames queue. That
/// is rule 2.3's "a slow consumer can only ever hurt itself", and the cost it
/// does impose — one blocking-pool thread held for as long as the consumer takes
/// — is what [`MAX_INFLIGHT_RESOURCE_READS`] bounds.
///
/// **A read error mid-stream ends the body short.** There is no way to un-send a
/// response, so the only refusal available after the first byte is truncation
/// against an honest `Content-Length`, which is what an HTTP client reports. A
/// partial body of a refused resource is never valid, and the ledger says so.
fn stream_body(
    mut reader: Box<dyn Read + Send + 'static>,
    skip: u64,
    want: u64,
    permit: ReadPermit,
) -> Body {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(
        RESOURCE_CHUNKS_IN_FLIGHT,
    );
    tokio::task::spawn_blocking(move || {
        // The permit lives exactly as long as this pump, under every exit —
        // including the consumer going away, which closes the channel and breaks
        // the loop below.
        let _permit = permit;
        let mut buf = vec![0u8; RESOURCE_CHUNK_BYTES];

        // Skip forward only as far as the producer did not. A producer that
        // seeked exactly skips nothing here.
        let mut left = skip;
        while left > 0 {
            let n = std::cmp::min(left, buf.len() as u64) as usize;
            match reader.read(&mut buf[..n]) {
                Ok(0) => {
                    let _ = tx.blocking_send(Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "resource ended inside the range the producer said it covered",
                    )));
                    return;
                }
                Ok(got) => left -= got as u64,
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            }
        }

        let mut left = want;
        while left > 0 {
            let n = std::cmp::min(left, buf.len() as u64) as usize;
            match reader.read(&mut buf[..n]) {
                Ok(0) => {
                    let _ = tx.blocking_send(Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "resource is shorter than the length its producer declared",
                    )));
                    return;
                }
                Ok(got) => {
                    left -= got as u64;
                    if tx.blocking_send(Ok(buf[..got].to_vec())).is_err() {
                        return; // the consumer went away; stop reading.
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    return;
                }
            }
        }
    });
    Body::from_stream(futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    }))
}

/// `PUT /resource/{digest}` — idempotent bulk upload, keyed by content digest.
pub(crate) async fn put_resource<P: Producer>(
    State(chassis): State<Chassis<P>>,
    Path(segment): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Body,
) -> Response {
    match accept(&chassis, &headers, segment, query, body).await {
        Ok(r) => r,
        Err(d) => d.into_response(),
    }
}

/// **WHY AN EARLY REFUSAL STILL READS A LITTLE OF THE BODY.**
///
/// HTTP/1.1: a server that answers before reading the request body and then
/// closes makes the client's pending write fail, and the RST discards the
/// response it was about to read — the consumer gets a connection error instead
/// of the typed refusal that says what to fix. So every pre-body refusal drains
/// a bounded amount first.
///
/// **Bounded, and the bound is not [`MAX_UPLOAD_BYTES`], deliberately.** Reading
/// a whole 8 MiB upload before refusing an UNAUTHENTICATED request would let
/// anyone who can reach the port make the daemon buffer megabytes; the checks
/// stay ahead of the body for that reason. One chunk is enough for every refusal
/// a small request can earn, and a consumer sending more than that before its
/// request has been accepted is what `Expect: 100-continue` exists for.
async fn drain_briefly(body: Body) {
    let _ = axum::body::to_bytes(body, RESOURCE_CHUNK_BYTES).await;
}

async fn accept<P: Producer>(
    chassis: &Chassis<P>,
    headers: &HeaderMap,
    segment: String,
    query: Option<String>,
    body: Body,
) -> Result<Response, Denied> {
    let id = match admit(chassis, headers, segment, query) {
        Ok(id) => id,
        Err(d) => {
            drain_briefly(body).await;
            return Err(d);
        }
    };
    let Some(resources) = chassis.producer().resources() else {
        drain_briefly(body).await;
        return Err(no_resources());
    };
    // **405, NOT 501.** The door exists and reads; this producer's ids are not
    // digests, so a consumer cannot compute one and must not choose one.
    let Some(uploads) = resources.uploads() else {
        drain_briefly(body).await;
        return Err(Denied::from_refusal(
            Refusal::new(
                codes::UNSUPPORTED,
                "this producer's ids are issued, not computed: an id is read off a \
                 shape's summary or derived from the bytes, and never chosen by a \
                 consumer. It accepts no uploads",
            ),
            true,
        )
        .header(header::ALLOW, HeaderValue::from_static("GET, HEAD")));
    };

    let digest = uploads.digest();
    // The id must be a digest UNDER THE DECLARED FUNCTION before any byte is
    // read: a 64-hex id is the only thing that can name a sha256 blob, and
    // buffering megabytes to discover the name was never a digest is work done
    // for a refusal already available.
    if id.as_str().len() != digest.hex_len()
        || !id
            .as_str()
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        drain_briefly(body).await;
        return Err(Denied::from_refusal(
            Refusal::malformed(format!(
                "a PUT id must be {} lowercase hex characters — this producer's ids are \
                 {} digests",
                digest.hex_len(),
                digest.name()
            )),
            true,
        ));
    }

    let bytes = axum::body::to_bytes(body, MAX_UPLOAD_BYTES)
        .await
        .map_err(|_| {
            Denied::from_refusal(
                Refusal::new(
                    codes::TOO_LARGE,
                    format!(
                        "an upload is buffered whole so its digest can be verified before \
                         anything is stored; the limit is {MAX_UPLOAD_BYTES} bytes and a \
                         larger blob is uploaded as chunks"
                    ),
                ),
                true,
            )
            .with("limit", MAX_UPLOAD_BYTES)
        })?;

    // ── VERIFIED BEFORE IT ACKS, AND BEFORE THE PRODUCER SEES IT ──────────
    //
    // One pass over the buffer on a blocking thread: CPU only, no I/O, and the
    // bytes are already in hand. The cost is one hash of the upload — at SHA-256
    // memory speed, single-digit milliseconds at the 8 MiB cap — and what it
    // buys is that `Uploads::put` cannot be reached with bytes that do not hash
    // to the name they were sent under. A store keyed by a digest it did not
    // check is a store whose ids lie.
    let claimed = id.as_str().to_string();
    let got = {
        let bytes = bytes.clone();
        tokio::task::spawn_blocking(move || digest.hex(&bytes))
            .await
            .map_err(|e| {
                Denied::from_refusal(
                    Refusal::new(codes::INTEGRITY, format!("digest task failed: {e}")),
                    true,
                )
            })?
    };
    if got != claimed {
        // **NOT `integrity`.** `integrity` is the producer's bytes disagreeing
        // with the producer's own declared digest, which a consumer cannot
        // repair. This one the consumer repairs by re-sending — a different
        // actionability, so a different code.
        return Err(Denied::from_refusal(
            Refusal::new(
                codes::DIGEST_MISMATCH,
                "the body does not hash to the digest it was PUT under",
            ),
            true,
        )
        .with("expected", claimed)
        .with("got", got));
    }

    let stored = uploads
        .put(&claimed, bytes)
        .map_err(|r| Denied::from_refusal(r, true))?;
    let status = match stored {
        Stored::Created => StatusCode::CREATED,
        Stored::AlreadyHeld => StatusCode::OK,
    };
    // ── WHAT THIS ACK MEANS, UNDER RULE 2.7 ───────────────────────────────
    //
    // **`accepted`, and deliberately not more.** The bytes were received and
    // VERIFIED against the digest they were sent under; that is the whole claim.
    // It says nothing about what references them — the command that re-points a
    // shape at a digest is a typed websocket command and is not this door's —
    // and it says nothing about surviving a crash. `durable` is OPT-IN through a
    // barrier another dispatch builds, and a door that fsynced here would be
    // promising a stage nobody asked for, which is the accident rule 2.7
    // reverses (`threads`' `store.rs` pays `synchronous=FULL` on every command
    // whether it asked or not). **So this door performs no fsync, and the stage
    // is on the wire so the claim cannot be inferred wrongly from a 201.**
    Ok((
        status,
        axum::Json(serde_json::json!({ "digest": claimed, "stage": "accepted" })),
    )
        .into_response())
}

// ─────────────────────────────────────────────────────────────────────────────
// 10. Plumbing
// ─────────────────────────────────────────────────────────────────────────────

fn hv(s: &str) -> Result<HeaderValue, Denied> {
    HeaderValue::from_str(s).map_err(|_| {
        Denied::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            Refusal::new(codes::INTEGRITY, format!("cannot form a header from {s:?}")),
        )
    })
}

/// `If-None-Match` against a strong validator.
///
/// `*` matches anything that exists; otherwise an entity-tag list is compared
/// verbatim. A `W/`-prefixed tag never matches — the comparison a 304 needs is
/// the strong one, and this door's validator is the id.
fn if_none_match_matches(headers: &HeaderMap, etag: &str) -> bool {
    let Some(raw) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    raw.split(',')
        .map(str::trim)
        .any(|t| t == "*" || t == etag)
}

// ─────────────────────────────────────────────────────────────────────────────
// 10. The SECOND door: the same opaque id, inline over the websocket
// ─────────────────────────────────────────────────────────────────────────────

/// **THE PARAMS SIDE OF THE ONE GATE.**
///
/// `resource/{id}/read` arrives as a params object rather than a URI, so the
/// thing that would smuggle a parallel namespace in is a SECOND KEY rather than
/// a query string — `{"id": "x", "path": "/foo/bar"}` is `?path=` with different
/// punctuation. So every key but `id` is refused, and the refusal is spelled by
/// handing the leftovers to [`opaque_id`] as the query it is: one gate, one
/// message, and no second place for the rule to be forgotten.
///
/// This is why [`ResourceId`] still has exactly one constructor even though it
/// now has two doors.
fn opaque_id_from_params(params: &Value) -> Result<ResourceId, Refusal> {
    let Some(obj) = params.as_object() else {
        return Err(Refusal::malformed(
            "`resource/{id}/read` takes the id on the path and no params",
        ));
    };
    let extra: Vec<&str> = obj
        .keys()
        .filter(|k| k.as_str() != "id")
        .map(String::as_str)
        .collect();
    let query = if extra.is_empty() {
        None
    } else {
        Some(extra.join("&"))
    };
    let segment = match obj.get("id").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => String::new(),
    };
    opaque_id(segment, query)
}

/// Lowercase base64, standard alphabet, padded. Hand-written rather than
/// depended on: the floor's dependency list is asserted by `check-features.sh`
/// and twenty lines is cheaper than defending a new crate in it.
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for c in bytes.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// **`resource/{id}/read` — THE WHOLE RESOURCE, INLINE, ON THE OPEN SOCKET.**
///
/// The second door the ruling needs, and deliberately the SMALLER one. It has no
/// `Range`, no conditional request, no `Cache-Control` and no streaming: a
/// websocket frame is atomic, so a partial inline read would be a second body
/// format for one resource, which is the rendering dial `?as=json` is refused
/// for. A consumer that wants a range wants [`get_resource`], and the consuming
/// chassis is what chooses between them — never the application.
///
/// **It answers the SAME typed refusals as the HTTP door**, out of the same
/// [`Resources::fetch`] call, because a consumer that cannot tell the two doors
/// apart must not be able to tell them apart by how they fail either. The one
/// refusal only this door can produce is [`codes::TOO_LARGE`] against
/// [`MAX_WS_INLINE_BYTES`], and it is reached BEFORE the read: a cap discovered
/// after the bytes are in hand is a cap that cost the memory it exists to save.
///
/// The payload is `{bytes, content_type, base64}`. `base64` and not raw text:
/// the bytes behind an id are arbitrary (a message body today, a KV-cache blob
/// tomorrow), and a JSON string cannot hold a non-UTF-8 byte at all. The name
/// says the encoding so no consumer has to guess, and the chassis's own
/// `Mirror::read` decodes it before the application sees anything.
pub(crate) fn read_resource_inline<P: Producer>(
    producer: &P,
    params: &Value,
) -> Result<Value, Refusal> {
    let id = opaque_id_from_params(params)?;
    let Some(resources) = producer.resources() else {
        return Err(Refusal::new(
            codes::UNSUPPORTED,
            "this producer serves no resources; the method exists on every chassis so \
             that this answer is distinguishable from an unknown method and from an \
             unknown id",
        ));
    };
    // `range: None`, always — see the doc above. The producer's own `fetch` is
    // the single resolution path, so an id that 404s on one door 404s on both.
    let found = resources.fetch(&id, None)?;
    if found.len > MAX_WS_INLINE_BYTES {
        drop(found.body);
        return Err(Refusal::new(
            codes::TOO_LARGE,
            format!(
                "resource {id} is {} bytes; this door answers at most {MAX_WS_INLINE_BYTES} \
                 inline. The id is good — read it through the by-id door instead",
                found.len
            ),
        ));
    }
    let mut buf = Vec::with_capacity(found.len as usize);
    let mut body = found.body;
    if let Err(e) = body.read_to_end(&mut buf) {
        return Err(Refusal::new(
            codes::INTEGRITY,
            format!("the producer's reader failed partway through resource {id}: {e}"),
        ));
    }
    // **THE LENGTH THE PRODUCER DECLARED IS CHECKED, NOT TRUSTED**, for the
    // reason `Found::offset` is checked on the other door: the summary that
    // issued this id states the length, and a door that answers a different
    // number of bytes under it makes the summary wrong by implication. The
    // consuming chassis refuses on this too (`mirror::ContentError::Integrity`),
    // and both ends checking is deliberate — this one names the PRODUCER's
    // defect, where the consumer's names a disagreement it cannot attribute.
    if buf.len() as u64 != found.len {
        return Err(Refusal::new(
            codes::INTEGRITY,
            format!(
                "the producer declared resource {id} as {} bytes and its reader yielded {}",
                found.len,
                buf.len()
            ),
        ));
    }
    Ok(serde_json::json!({
        "bytes": buf.len(),
        "content_type": found.content_type,
        "base64": base64(&buf),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::Outcome;

    // ── THE NO-QUERY RULE ────────────────────────────────────────────────

    /// **Forced red** by `if !q.is_empty()` → `if false`, i.e. by IGNORING the
    /// query the way every accidental namespace begins:
    /// `a query must be refused, not ignored: ResourceId("abc")`.
    #[test]
    fn a_query_is_refused_and_never_ignored() {
        for q in [
            "path=/foo/bar",
            "thread=x&seq=y",
            "as=json",
            "inline=1",
            "bearer=deadbeef",
        ] {
            let r = opaque_id("abc".into(), Some(q.to_string()))
                .expect_err("a query must be refused, not ignored");
            assert_eq!(r.code, codes::MALFORMED, "{q}");
            assert!(r.message.contains(q), "the refusal names what was sent: {r}");
        }
        // An empty query string (a bare trailing `?`) is not a query.
        assert!(opaque_id("abc".into(), Some(String::new())).is_ok());
        assert!(opaque_id("abc".into(), None).is_ok());
    }

    /// The six endpoints `chassis-writes.md` §2.2 refuses, spelled as ids. None
    /// of them can be one.
    ///
    /// **Forced red** by adding `b'/' | b'?' | b'&' | b'=' | b'#' | b'%'` to
    /// [`is_id_byte`]: `/foo/bar` and `abc/versions` become legal ids and the
    /// parallel namespace is back —
    /// ``/foo/bar` must not be a legal resource id`.
    #[test]
    fn a_path_a_subresource_and_a_selector_cannot_be_spelled_as_an_id() {
        for bad in [
            "/foo/bar",
            "abc/versions",
            "abc/meta",
            "latest/lineage-1",
            "abc?as=json",
            "abc%2Fversions",
            "abc#frag",
            "a&b=c",
            "",
        ] {
            assert!(
                opaque_id(bad.into(), None).is_err(),
                "`{bad}` must not be a legal resource id"
            );
        }
        for good in ["abcDEF012", "t-1.17", "sha256:abc", "a+b~c@d", "_x-1"] {
            assert!(
                opaque_id(good.into(), None).is_ok(),
                "`{good}` is one opaque segment"
            );
        }
        assert!(opaque_id("a".repeat(MAX_RESOURCE_ID_BYTES + 1), None).is_err());
    }

    // ── RANGES ───────────────────────────────────────────────────────────

    fn hdr(k: header::HeaderName, v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(k, HeaderValue::from_str(v).unwrap());
        h
    }

    #[test]
    fn a_range_parses_in_its_three_legal_forms_and_no_others() {
        let p = |v: &str| ByteRange::parse(&hdr(header::RANGE, v));
        assert_eq!(
            p("bytes=0-9").unwrap(),
            Some(ByteRange::From {
                first: 0,
                last: Some(9)
            })
        );
        assert_eq!(
            p("bytes=5-").unwrap(),
            Some(ByteRange::From {
                first: 5,
                last: None
            })
        );
        assert_eq!(p("bytes=-4").unwrap(), Some(ByteRange::Suffix { len: 4 }));
        assert_eq!(ByteRange::parse(&HeaderMap::new()).unwrap(), None);
        for bad in ["items=0-9", "bytes=9-0", "bytes=-", "bytes=x-y", "bytes=0-1,5-6"] {
            assert_eq!(
                p(bad).unwrap_err().code,
                codes::MALFORMED,
                "`{bad}` must be refused"
            );
        }
        // ── A MULTI-RANGE REFUSAL MUST BE THE DELIBERATE ONE ──────────────
        //
        // The first cut of this test asserted only that `bytes=0-1,5-6` was
        // `malformed`, and it **STAYED GREEN** when the comma check was deleted:
        // `"1,5".parse::<u64>()` fails, so the refusal arrived by accident from
        // the number parser. A test that passes for a reason the code does not
        // hold is the class of false evidence this file exists not to add to, so
        // the SENTENCE is asserted — only the deliberate branch writes it.
        //
        // **Forced red** by deleting the `rest.contains(',')` branch:
        // `assertion failed: a multi-range request must be refused as one, not
        // as a number that would not parse: malformed Range: `bytes=0-1,5-6``
        for multi in ["bytes=0-1,5-6", "bytes=0-1, 5-6", "bytes=0-,-1"] {
            let e = p(multi).unwrap_err();
            assert!(
                e.message.contains("one range per request"),
                "a multi-range request must be refused as one, not as a number that \
                 would not parse: {e}"
            );
        }
    }

    /// **THE RULE: REFUSED, NEVER CLAMPED.**
    ///
    /// **Forced red** by disarming the `Some(l) if l >= len` arm so the range
    /// falls through to RFC 9110's clamp:
    /// `a range outside the resource must be refused: (0, 10)`.
    #[test]
    fn a_range_outside_the_resource_is_refused_rather_than_clamped() {
        let len = 10;
        // Past the end, both ends.
        for r in [
            ByteRange::From {
                first: 0,
                last: Some(10),
            },
            ByteRange::From {
                first: 0,
                last: Some(99),
            },
            ByteRange::From {
                first: 10,
                last: None,
            },
            ByteRange::Suffix { len: 11 },
            ByteRange::Suffix { len: 0 },
        ] {
            let e = r
                .resolve(len)
                .expect_err("a range outside the resource must be refused");
            assert_eq!(e.code, codes::RANGE_UNSATISFIABLE, "{r:?}");
        }
        // And the satisfiable ones are served EXACTLY as asked.
        assert_eq!(
            ByteRange::From {
                first: 0,
                last: Some(9)
            }
            .resolve(len)
            .unwrap(),
            (0, 9)
        );
        assert_eq!(
            ByteRange::From {
                first: 3,
                last: None
            }
            .resolve(len)
            .unwrap(),
            (3, 9)
        );
        assert_eq!(ByteRange::Suffix { len: 4 }.resolve(len).unwrap(), (6, 9));
        // An empty resource has no satisfiable range at all.
        assert_eq!(
            ByteRange::Suffix { len: 1 }.resolve(0).unwrap_err().code,
            codes::RANGE_UNSATISFIABLE
        );
    }

    // ── ETAG / IF-NONE-MATCH ─────────────────────────────────────────────

    /// **Forced red** by stripping a `W/` prefix before comparing
    /// (`t.trim_start_matches("W/") == etag`): `assertion failed:
    /// !if_none_match_matches(&hdr(header::IF_NONE_MATCH, "W/\"abc\""), etag)`.
    #[test]
    fn if_none_match_is_a_strong_comparison_against_the_id() {
        let etag = "\"abc\"";
        assert!(if_none_match_matches(&hdr(header::IF_NONE_MATCH, "\"abc\""), etag));
        assert!(if_none_match_matches(&hdr(header::IF_NONE_MATCH, "*"), etag));
        assert!(if_none_match_matches(
            &hdr(header::IF_NONE_MATCH, "\"zzz\", \"abc\""),
            etag
        ));
        assert!(!if_none_match_matches(&hdr(header::IF_NONE_MATCH, "\"zzz\""), etag));
        assert!(!if_none_match_matches(&hdr(header::IF_NONE_MATCH, "W/\"abc\""), etag));
        assert!(!if_none_match_matches(&HeaderMap::new(), etag));
    }

    // ── RETENTION IS NOT DECORATIVE ──────────────────────────────────────

    /// **Forced red** by returning [`CACHE_IMMUTABLE`] for every arm:
    /// `a mutable id must be revalidated: a cache told `immutable` never learns
    /// the id was superseded, which makes it a second source for the pointer
    /// left: "public, max-age=31536000, immutable"  right: "no-cache"`.
    #[test]
    fn only_a_mutable_id_is_told_to_revalidate() {
        assert_eq!(Retention::Fixed.cache_control(), CACHE_IMMUTABLE);
        assert_eq!(Retention::All.cache_control(), CACHE_IMMUTABLE);
        assert_eq!(
            Retention::Latest.cache_control(),
            CACHE_REVALIDATE,
            "a mutable id must be revalidated: a cache told `immutable` never learns \
             the id was superseded, which makes it a second source for the pointer"
        );
    }

    /// The operator's three names are what ride the wire; the axis is internal.
    ///
    /// **Forced red** by deleting `#[serde(rename = "mutable")]`:
    /// `left: "\"Latest\""  right: "\"mutable\""` — the wire starts carrying
    /// the chassis's internal axis name instead of the operator's.
    #[test]
    fn retention_serialises_under_the_operators_three_names() {
        let n = |r: Retention| serde_json::to_string(&r).unwrap();
        assert_eq!(n(Retention::All), "\"versioned\"");
        assert_eq!(n(Retention::Latest), "\"mutable\"");
        assert_eq!(n(Retention::Fixed), "\"immutable\"");
    }

    // ── DIGEST ───────────────────────────────────────────────────────────

    /// The one vector every SHA-256 implementation agrees on, so a swapped or
    /// mis-fed hasher cannot pass — a PUT verified by a hasher that is not the
    /// one it claims verifies nothing.
    ///
    /// **Forced red** by feeding the hasher nothing (`h.update(&bytes[..0])`):
    /// `left: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    /// right: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"`
    /// — the empty digest, which every empty-bodied PUT would then have matched.
    #[test]
    fn sha256_is_sha256() {
        assert_eq!(
            Digest::Sha256.hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(Digest::Sha256.hex(b"").len(), Digest::Sha256.hex_len());
    }

    // ── THE STATUS MAP ───────────────────────────────────────────────────

    /// **Forced red** by mapping `SUPERSEDED | GONE` to 404: they stop sharing
    /// 410, the "HTTP has one word for both and the code is what distinguishes
    /// them" claim fails — `left: 404  right: 410`.
    #[test]
    fn every_code_maps_to_a_status_a_consumer_can_act_on() {
        let s = |c: &'static str, w: bool| Denied::from_refusal(Refusal::new(c, ""), w).status;
        assert_eq!(s(codes::UNKNOWN, false), StatusCode::NOT_FOUND);
        assert_eq!(s(codes::SUPERSEDED, false), StatusCode::GONE);
        assert_eq!(s(codes::GONE, false), StatusCode::GONE);
        assert_eq!(s(codes::FORBIDDEN, false), StatusCode::FORBIDDEN);
        assert_eq!(s(codes::MALFORMED, false), StatusCode::BAD_REQUEST);
        assert_eq!(
            s(codes::RANGE_UNSATISFIABLE, false),
            StatusCode::RANGE_NOT_SATISFIABLE
        );
        assert_eq!(
            s(codes::DIGEST_MISMATCH, true),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(s(codes::TOO_LARGE, true), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(s(codes::BUSY, false), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(s(codes::UNAUTHORIZED, false), StatusCode::UNAUTHORIZED);
        assert_eq!(s(codes::INTEGRITY, false), StatusCode::INTERNAL_SERVER_ERROR);
        // One code, two statuses, because the consumer's move is the same and
        // HTTP has a word for each door.
        assert_eq!(s(codes::UNSUPPORTED, false), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(
            s(codes::UNSUPPORTED, true),
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    /// **THE REFUSAL BODY IS THE WEBSOCKET ENVELOPE, BYTE FOR BYTE, PLUS THE
    /// FIELDS THE CODE OWES.**
    ///
    /// **Forced red** by building the body through `serde_json::to_value` and
    /// inserting the extras into the `Map` — which sorts, and the assertion comes
    /// back `{"code":…,"error":…,"limit":7,"type":"error"}` against the
    /// websocket's declaration order.
    #[test]
    fn a_refusal_body_is_the_websocket_envelope_plus_the_fields_its_code_carries() {
        let d =
            Denied::from_refusal(Refusal::new(codes::TOO_LARGE, "too big"), true).with("limit", 7);
        let http = serde_json::to_string(&d.body()).unwrap();
        // The websocket's own serialisation of the same refusal, with nothing
        // added — the two must agree on their shared prefix.
        let ws = serde_json::to_string(&Outcome::refused(d.refusal.clone())).unwrap();
        assert_eq!(ws, r#"{"type":"error","error":"too big","code":"too_large"}"#);
        assert_eq!(
            http,
            r#"{"type":"error","error":"too big","code":"too_large","limit":7}"#
        );
        // And the HTTP body still parses as the envelope a consumer already
        // reads: the extras are additive, never a swap.
        let o: Outcome = serde_json::from_str(&http).unwrap();
        assert_eq!(o.code(), Some(codes::TOO_LARGE));
    }

    // ── THE GATE ─────────────────────────────────────────────────────────

    /// **Forced red** by disarming the `cur >= MAX_INFLIGHT_RESOURCE_READS`
    /// branch so `reserve` always succeeds: `the cap is the cap`.
    #[test]
    fn the_read_gate_refuses_past_its_cap_and_releases_on_drop() {
        let g = std::sync::Arc::new(ReadGate::new());
        let held: Vec<ReadPermit> = (0..MAX_INFLIGHT_RESOURCE_READS)
            .map(|_| g.reserve().expect("under the cap"))
            .collect();
        assert_eq!(g.inflight(), MAX_INFLIGHT_RESOURCE_READS);
        assert!(g.reserve().is_none(), "the cap is the cap");
        drop(held);
        assert_eq!(g.inflight(), 0, "every permit releases on drop");
        assert!(g.reserve().is_some());
    }
}
