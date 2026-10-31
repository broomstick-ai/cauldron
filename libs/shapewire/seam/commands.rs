//! `commands.rs` — **THE VERBS, AND NOTHING ELSE GOES OUT THIS DOOR.**
//!
//! Rule 2.4: *a command acknowledges and never answers with the fact it caused.*
//! Every function here answers an [`Ack`] carrying `{promise_id, visible,
//! changed}` plus whatever ADDRESS the act minted (`create` mints a `thread_id`,
//! `send` a `message_id`), and the FACT arrives later, on a frame, through the
//! shape, with the promise id on it.
//!
//! **These are the only methods this seam can speak.** The read methods
//! `threads/status`, `threads/list`, `threads/{id}/get`,
//! `threads/{id}/history`, `accounts/status` and `accounts/list` are not here
//! and have no home above the seam either, so the operator's ruling (*"we
//! should remove the ability to request data"*) is a property of the build and
//! not a habit.
//!
//! ## The method table is HERE, and that is divergence D4 dissolving
//!
//! The design put the verb SET in a fence, because the AUI calls `seen` and
//! `update` and the terminal face does not. But a verb a face never calls is
//! not a policy, it is unused vocabulary: it compiles, it costs nothing, and
//! WHICH verbs a face calls is a property of the face's call sites and not of
//! this file. So the reference carries every verb, no fence, and the faces
//! differ by what they call.
//!
//! The method NAMES live in [`methods`] below rather than in a per-face
//! `methods.rs`, so that `tools/seamdiff --inventory` has one list to route
//! against `inventory.json`'s `WS_METHODS ∪ CHASSIS_METHODS`. A face that
//! smuggles in a method of its own gets a red here, not a typed
//! `unknown_method` at the operator's next keypress — which is how
//! `thread.replace` and `thread.unpend` were found: both were spelled in the
//! AUI, neither routed against the producer's inventory, and the live daemon
//! answered `unknown method` to both (probed on `:3651`, 2026-10-02). They were
//! never in this table.
//!
//! **BOTH ARE NOW DELETED FROM EVERY FACE**, on the operator's ruling —
//! *"remove them"* — together with the `pending` status word their two Options
//! rows were gated on, which no producer on this wire has ever sent. The
//! inventory guard that found them is unchanged and its forcing no longer uses
//! their names as fixtures (`tools/seamdiff/tests/forcings.rs` says which
//! invented names it uses instead, and why those cannot become real).
//!
//! ## Hold before you command
//!
//! A producer answers `visible: true` and emits the frame TO THE HOLDERS OF THAT
//! KEY. A connection that commanded a key it does not hold gets the
//! acknowledgement and never the fact. Rule 2.5's per-key promise retention (120
//! s) means a subscribe issued AFTER the command still gets the id on its export
//! — but only within the window, and only if something subscribes. So every verb
//! here holds its subject FIRST. For `create` the subject does not exist until
//! the reply, so it is held immediately after, and the export re-offers the id
//! carried by `added`.

use serde_json::{json, Value};
use shapewire::envelope::{codes, Refusal};

use super::held::{Pending, THREADS};
// `super::`, not `crate::`: in this reference the seam IS the crate root, and in a
// face it is a module of the face's crate. Every path in this directory is
// relative for that reason, so the same text compiles in both.
use super::inventory::methods as served;
use super::Seam;

/// **EVERY METHOD NAME THE SEAM CAN SEND, AND NOTHING ELSE — NOW AS TOKENS.**
///
/// Each const below IS the generated inventory's const, re-exported under the
/// verb's own name. There is no literal here and there is no `format!`: a method
/// name reaches the wire only as `Method::at`'s output, and `Method` exists only
/// for the templates `seam/inventory.rs` declares, which are exactly the ones
/// `CHASSIS_METHODS u WS_METHODS` dispatches.
///
/// **What that closes.** `thread.replace` and `thread.unpend` were spelled in a
/// face, routed against nothing, and answered `unknown method` by the live daemon
/// (probed on `:3651`, 2026-10-02). They were never in this table — they were
/// issued ABOVE the seam, through `Link::request` with a string literal, which is
/// the shape a compile-time check has to make impossible. It now is, in both
/// directions: there is no `Method` const for a name the producer does not serve
/// (rustc answers `E0425: cannot find value ... in module`), and `request` does
/// not take a string at all, so there is no door beside this one.
///
/// A face that needs a method the producer does not serve has exactly one move:
/// the producer gains the method and the inventory is regenerated. An escape
/// hatch IS the hole.
pub mod methods {
    use shapewire::surface::{lists, Dialed, Method};

    use super::served;

    pub const CREATE: Method<0> = served::THREADS_CREATE;
    pub const SEND: Method<1> = served::THREADS_SEND;
    pub const UPDATE: Method<1> = served::THREADS_UPDATE;
    pub const SEEN: Method<1> = served::THREADS_SEEN;
    pub const EVENT: Method<1> = served::THREADS_EVENT;
    pub const CANCEL: Method<1> = served::THREADS_CANCEL;

    /// **WHAT THE GUARD'S SECOND WITNESS WALKS**, in the producer's own spelling.
    ///
    /// It is still literals, deliberately: `tools/seamdiff --inventory` reads this
    /// list out of this file's TEXT, and a list derived from the tokens above
    /// would hand that guard an empty scan — a check that cannot fail, which is
    /// worse than no check. So the two spellings both exist and the `const`
    /// assertions below are what keep them one fact: every token's template is in
    /// this list, and this list is exactly as long as there are tokens.
    pub const TEMPLATES: &[&str] = &[
        "threads/create",
        "threads/{thread_id}/send",
        "threads/{thread_id}/update",
        "threads/{thread_id}/seen",
        "threads/{thread_id}/event",
        "threads/{thread_id}/cancel",
    ];

    // Literal messages, no braces: `assert!` reads its second argument as a
    // format string and a template segment in braces would be parsed as a
    // capture. The same constraint `declare_cells!` writes under.
    const _: () = assert!(
        lists(TEMPLATES, CREATE.template()),
        "`threads/create` has a method token and is not in TEMPLATES, so the inventory \
         guard's second witness would not walk it. WHAT IS MISSING: this template in \
         TEMPLATES, in the producer's own spelling.",
    );
    const _: () = assert!(
        lists(TEMPLATES, SEND.template()),
        "the `send` method token's template is not in TEMPLATES, so the inventory guard's \
         second witness would not walk it. WHAT IS MISSING: this template in TEMPLATES, in \
         the producer's own spelling — which is the one the token carries.",
    );
    const _: () = assert!(
        lists(TEMPLATES, UPDATE.template()),
        "the `update` method token's template is not in TEMPLATES. WHAT IS MISSING: this \
         template in TEMPLATES, in the producer's own spelling.",
    );
    const _: () = assert!(
        lists(TEMPLATES, SEEN.template()),
        "the `seen` method token's template is not in TEMPLATES. WHAT IS MISSING: this \
         template in TEMPLATES, in the producer's own spelling.",
    );
    const _: () = assert!(
        lists(TEMPLATES, EVENT.template()),
        "the `event` method token's template is not in TEMPLATES. WHAT IS MISSING: this \
         template in TEMPLATES, in the producer's own spelling.",
    );
    const _: () = assert!(
        lists(TEMPLATES, CANCEL.template()),
        "the `cancel` method token's template is not in TEMPLATES. WHAT IS MISSING: this \
         template in TEMPLATES, in the producer's own spelling.",
    );
    /// **AND THE OTHER DIRECTION.** Six tokens, six templates: an entry in
    /// `TEMPLATES` with no token above it would be a method the guard walks and
    /// the seam cannot send, which is a list describing a seam that does not
    /// exist.
    const _: () = assert!(
        TEMPLATES.len() == 6,
        "TEMPLATES has an entry with no method token beside it, or is missing one. The \
         list and the tokens are one fact: a template the seam cannot send is a method \
         nobody may read this list as evidence of. WHAT IS MISSING: either the token (a \
         const in this module, re-exported from the generated inventory) or the entry.",
    );

    pub fn send(thread: &str) -> Dialed {
        SEND.at(thread)
    }
    pub fn update(thread: &str) -> Dialed {
        UPDATE.at(thread)
    }
    pub fn seen(thread: &str) -> Dialed {
        SEEN.at(thread)
    }
    pub fn event(thread: &str) -> Dialed {
        EVENT.at(thread)
    }
    pub fn cancel(thread: &str) -> Dialed {
        CANCEL.at(thread)
    }
}

/// What a command answers. **No row, no status, no settings** — rule 2.4.
#[derive(Debug, Clone)]
pub struct Ack {
    pub promise_id: Option<u64>,
    /// `false` means *accepted, applied, and no frame will ever carry this id*.
    /// The face's final word is "accepted"; waiting on it forever is the third
    /// way a naive port gets rule 2.4 wrong.
    pub visible: bool,
    pub changed: Option<bool>,
    /// The addresses the act minted, and only those: `thread_id` off `create`,
    /// `message_id` and `header_warnings` off `send`.
    pub payload: Value,
}

impl Ack {
    /// **REGISTER THE PROMISE, OR RECORD THAT THERE IS NOTHING TO WAIT FOR.**
    /// The one place a command's id enters the book, so a verb cannot forget to
    /// do it and a face cannot be left polling an id nobody is holding.
    fn of(verb: &'static str, subject: Option<String>, payload: Value, s: &Seam) -> Self {
        let promise_id = payload.get("promise_id").and_then(Value::as_u64);
        let visible = payload
            .get("visible")
            .and_then(Value::as_bool)
            // An older producer that answers no `visible` is treated as
            // VISIBLE: waiting for a frame that never comes is recoverable
            // (`lose_unoffered` settles it `Lost` on the next complete export),
            // and reporting "accepted" for something that then changed on
            // screen is not.
            .unwrap_or(true);
        if let Some(id) = promise_id {
            let mut g = s.lock();
            if visible {
                let boot_id = g.link.boot_id().unwrap_or_default().to_string();
                g.promises.open(
                    id,
                    Pending {
                        verb,
                        subject: subject.clone(),
                        asked_at: chrono::Utc::now(),
                        boot_id,
                    },
                );
            } else {
                g.promises.never_visible(id);
            }
        }
        Ack {
            promise_id,
            visible,
            changed: payload.get("changed").and_then(Value::as_bool),
            payload,
        }
    }
}

/// Hold the subject's pool key, so the frame this command causes reaches this
/// connection. Idempotent and free when the key is already held — rule 2.5 makes
/// re-subscribe the remedy for every doubt a consumer can have, and the
/// producer's reservation is a SET.
async fn hold(s: &Seam, thread: &str) {
    let fresh = s.lock().book.hold(THREADS, Some(thread));
    if fresh {
        s.link.subscribe(THREADS, Some(thread.to_string())).await;
    }
}

pub async fn send(
    s: &Seam,
    thread: &str,
    text: &str,
    intent: Option<&str>,
    bare: bool,
    headers: &[Value],
    metadata: Value,
) -> Result<Ack, Refusal> {
    hold(s, thread).await;
    let mut params = json!({ "text": text, "metadata": metadata, "bare": bare });
    if let Some(i) = intent {
        params["intent"] = Value::String(i.to_string());
    }
    if !headers.is_empty() {
        params["headers"] = Value::Array(headers.to_vec());
    }
    let payload = s.link.request(methods::send(thread), params).await?;
    Ok(Ack::of("send", Some(thread.to_string()), payload, s))
}

pub async fn create(s: &Seam, thread: &str, settings: Value, name: Option<&str>) -> Result<Ack, Refusal> {
    let mut params = json!({ "thread_id": thread, "settings": settings });
    // The name the thread is born with (TUI-P1a), written by the producer as
    // its first `summary_updated`. Absent rather than empty when there is none.
    if let Some(n) = name {
        params["name"] = json!(n);
    }
    let payload = s.link.request(methods::CREATE.at(), params).await?;
    // **AFTER, not before**: the subject did not exist to hold. The export the
    // subscribe triggers re-offers the id carried by `added`.
    hold(s, thread).await;
    Ok(Ack::of("create", Some(thread.to_string()), payload, s))
}

pub async fn update(s: &Seam, thread: &str, patch: Value) -> Result<Ack, Refusal> {
    hold(s, thread).await;
    let payload = s
        .link
        .request(methods::update(thread), json!({ "patch": patch }))
        .await?;
    Ok(Ack::of("update", Some(thread.to_string()), payload, s))
}

pub async fn seen(s: &Seam, thread: &str, seq: u64) -> Result<Ack, Refusal> {
    hold(s, thread).await;
    let payload = s
        .link
        .request(methods::seen(thread), json!({ "seq": seq }))
        .await?;
    Ok(Ack::of("seen", Some(thread.to_string()), payload, s))
}

pub async fn cancel(s: &Seam, thread: &str) -> Result<Ack, Refusal> {
    hold(s, thread).await;
    let payload = s.link.request(methods::cancel(thread), json!({})).await?;
    Ok(Ack::of("cancel", Some(thread.to_string()), payload, s))
}

/// A face's OWN custom event, folded into the `threads` row's `app` cell and
/// echoed back to every holder of the key. The payload is opaque to the seam
/// and to the producer: `app.{event_type}` carries the newest one and
/// `view::app_payload` reads it back.
pub async fn event(
    s: &Seam,
    thread: &str,
    event_type: &str,
    data: Value,
    addressable: bool,
) -> Result<Ack, Refusal> {
    hold(s, thread).await;
    let payload = s
        .link
        .request(
            methods::event(thread),
            json!({ "event_type": event_type, "data": data, "addressable": addressable }),
        )
        .await?;
    Ok(Ack::of("event", Some(thread.to_string()), payload, s))
}

/// A refusal this face cannot do anything about but must be able to NAME. The
/// branch is on `code`, never on prose — the whole reason the seam moved to the
/// chassis door.
pub fn is_unsupported(r: &Refusal) -> bool {
    r.code == codes::UNKNOWN_METHOD
}

/// `busy` is RETURNED, never retried here. A retry policy in the consumer half
/// is a second opinion about the producer's load; the face decides.
pub fn is_busy(r: &Refusal) -> bool {
    r.code == codes::BUSY
}
