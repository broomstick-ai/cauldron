//! # What this producer's operations TAKE, declared
//!
//! The instances that justify [`shapewire::operation::DType`]'s closed set.
//! Every member of that set is here at least once, and
//! `tests/operation_dtypes.rs::every_dtype_has_a_declared_instance` is what keeps
//! that true — *"if a dtype has no instance, do not add it"* as a failing test
//! rather than a promise.
//!
//! ## This is a SLICE of fifty-two, and the count is the finding
//!
//! `crate::server::WS_METHODS` carries fifty-two method paths (R2.S3a retired
//! three reads; R4.S1 added the `agents` family; R4.S9 / R2.S3c retired ten more
//! reads whose consumers moved onto shapes, `gauge/history` among them, so its
//! declaration went with it; the pin is
//! `ws_protocol::the_ws_method_inventory_is_pinned`). **Eleven are declared
//! here.** They were chosen because between them they need every dtype, not
//! because the other forty-one are different in kind, and the gap is recorded
//! rather than glossed: the remaining forty-one take payloads this set can
//! express, and declaring them is mechanical work with no design left in it.
//! `every_declared_operation_is_a_real_method_path` binds what IS declared to the
//! router, so a declaration cannot drift onto a path that does not exist.
//!
//! ## What is NOT declared, and why not
//!
//! A declaration names what an operation TAKES and not what it answers — see
//! [`shapewire::operation::Operation`] for the argument, which is rule 2.7's:
//! the reply to a write is `Accepted{promise_id, visible, changed?}`, one type for
//! every write here, and the state a read answers with belongs to a SHAPE.
//!
//! ## And what the declaration cannot check
//!
//! That a handler reads the parameters its operation declares. Every handler on
//! this producer takes `params: &serde_json::Value` and hand-parses it, so there
//! is no type for the compiler to compare — the same shape of hole as
//! `Shape::MAX_RETAINED_ITEMS` and `Root::row_at`. What a test CAN do is drive
//! the real door and check that a `Required` parameter is actually required, and
//! `a_required_param_is_required_by_the_live_router` does exactly that.

use shapewire::declare_operation;
use shapewire::operation::{
    DType::{Cursor, Delta, Duration, Enum, List, Opaque, Path, Ref, Scalar as Sc, Struct,
        Timestamp},
    Declared, Param,
    Presence::{Ceiling, Optional, Required},
    Scalar::{Bool, Float, Int, Str},
    TimeEncoding::Rfc3339,
};

// ─────────────────────────────────────────────────────────────────────────────
// `threads`
// ─────────────────────────────────────────────────────────────────────────────

declare_operation!(
    pub Create,
    "threads/create",
    [
        // `struct` — the 25-field `ThreadSettings`, and one of only three params
        // on the whole surface that goes through `serde::Deserialize` into a
        // named type rather than being hand-parsed off a `Value`.
        Param::new("settings", Struct("ThreadSettings"), Required),
        // `ref` — a CALLER-MINTED id in this producer's own address space. The
        // one place a consumer constructs a ref instead of echoing one.
        Param::new("thread_id", Ref("thread"), Optional),
        Param::new("text", Sc(Str), Optional),
        // `opaque` — *"stored and returned, never read"*.
        Param::new("metadata", Opaque, Optional),
        // **THE NAME THE THREAD IS BORN WITH** (TUI-P1a, TC-01) — written as
        // the thread's first `summary_updated`, `manual: true`. Blank is
        // ignored.
        Param::new("name", Sc(Str), Optional),
    ]
);

declare_operation!(
    pub Send,
    "threads/{thread_id}/send",
    [
        Param::new("thread_id", Ref("thread"), Required),
        Param::new("text", Sc(Str), Required),
        // `enum` — `SendIntent`, and listing the variants is the entire
        // difference between this and `scalar:Str`.
        Param::new("intent", Enum(&["immediate", "turn_end"]), Optional),
        Param::new("bare", Sc(Bool), Optional),
        Param::new("metadata", Opaque, Optional),
        // `headers` is `opaque` AT THE DOOR and validated afterwards by
        // `context::validate_declarations`, which returns warnings rather than
        // refusing. That is an honest `opaque`: the declaration says the door
        // does not interpret it, and what happens later is the handler's.
        Param::new("headers", Opaque, Optional),
        // **THE TURN-START SETTINGS WRITE** (R4.S7, IS-27 ask 1; the operator,
        // `#H112`: *"maybe include an update_thread_settings={} object"*). The
        // same delta `threads/{thread_id}/update` takes, applied through the
        // same one writer BEFORE the message is queued, so the turn it starts
        // runs under the settings as they now are. `{}` changes nothing.
        Param::new("update_thread_settings", Delta("ThreadSettings"), Optional),
    ]
);

declare_operation!(
    pub Update,
    "threads/{thread_id}/update",
    [
        Param::new("thread_id", Ref("thread"), Required),
        // **`delta`, AND THE SUBTYPE IS LOAD-BEARING.** `SettingsPatch` has six
        // `Option<Option<T>>` fields: absent leaves, `null` clears. That rule
        // belongs to THIS delta and not to deltas, which is why the variant
        // carries a name and an empty one is a compile error.
        Param::new("patch", Delta("ThreadSettings"), Required),
    ]
);

declare_operation!(
    pub Event,
    "threads/{thread_id}/event",
    [
        Param::new("thread_id", Ref("thread"), Required),
        Param::new("event_type", Sc(Str), Required),
        // **THE OPERATION THAT FORCED `opaque` INTO THE SET.** `data` is an
        // arbitrary `Value` that lands verbatim in `ThreadEvent::Custom`, and it
        // is undeclared BY RULE: the general shape must not encode one
        // application's meaning. Declaring it `struct` would claim a shape
        // nobody may rely on.
        Param::new("data", Opaque, Optional),
        Param::new("addressable", Sc(Bool), Optional),
    ]
);

declare_operation!(
    pub History,
    "threads/{thread_id}/history",
    [
        Param::new("thread_id", Ref("thread"), Required),
        // **`cursor`, NOT `scalar`.** You do not compute one, you echo one you
        // were given — and this producer has the incident:
        // `the_unread_count_is_exact_where_the_position_difference_is_not`.
        Param::new("after_seq", Cursor("thread_event_seq"), Optional),
        Param::new("limit", Sc(Int), Optional),
        // `list` — and the element dtype is declared, which is the check
        // `well_formed` reaches inside for.
        Param::new("kinds", List(&Sc(Str)), Optional),
    ]
);

declare_operation!(
    pub Seen,
    "threads/{thread_id}/seen",
    [
        Param::new("thread_id", Ref("thread"), Required),
        Param::new("seq", Cursor("thread_event_seq"), Optional),
    ]
);

declare_operation!(
    pub QueueReselect,
    "threads/{thread_id}/queue_reselect",
    [
        Param::new("thread_id", Ref("thread"), Required),
        // `list<cursor>` — a list whose elements are positions, which is the
        // nesting the two variants together have to express.
        Param::new("message_ids", List(&Cursor("message_id")), Optional),
    ]
);

declare_operation!(
    pub ReconcileTranscript,
    "threads/{thread_id}/reconcile_transcript",
    [
        Param::new("thread_id", Ref("thread"), Required),
        // **THE OPERATION THAT SPLIT `ref` FROM `path`.** A `.jsonl` that must
        // already exist in the host filesystem. Nothing in this producer can
        // resolve it, which is exactly what makes it a different dtype from the
        // `thread_id` sitting beside it.
        Param::new("path", Path, Required),
    ]
);

// ─────────────────────────────────────────────────────────────────────────────
// `accounts` and `budgets`
// ─────────────────────────────────────────────────────────────────────────────

declare_operation!(
    pub PoolShareSet,
    "accounts/{account}/pool_share_set",
    [
        Param::new("account", Ref("account"), Required),
        // `scalar:Float` — a magnitude, bounded (0, 1] and REFUSED rather than
        // clamped, which is rule 2.8 arriving on an operation instead of a
        // subscribe. JSON `null`, PRESENT, CLEARS the override row (R2.S3g);
        // absent is still a refusal. `Presence` has no "nullable" word, so the
        // declaration cannot say that yet — the handler's doc does.
        Param::new("share", Sc(Float), Required),
        Param::new("note", Sc(Str), Optional),
    ]
);

declare_operation!(
    pub BudgetSet,
    "budgets/{key}/set",
    [
        Param::new("key", Ref("budget"), Required),
        Param::new("session_pct", Sc(Float), Optional),
        Param::new("accounts", List(&Ref("account")), Optional),
        Param::new("on_trip", Enum(&["kill", "finish"]), Optional),
        Param::new("disabled", Sc(Bool), Optional),
        // A ref that is STORED AND NEVER READ. Declaring it is what makes that
        // visible; leaving it out would have hidden it.
        Param::new("parent_key", Ref("budget"), Optional),
    ]
);

// ─────────────────────────────────────────────────────────────────────────────
// `shapes` — the one operation where `Presence::Ceiling` is the whole point
// ─────────────────────────────────────────────────────────────────────────────

declare_operation!(
    pub Subscribe,
    "shapes/{shape}/subscribe",
    [
        Param::new("shape", Ref("shape"), Required),
        Param::new("key", Ref("shape_key"), Optional),
        // **THREE `Ceiling` PARAMETERS, AND THAT IS WHY `Presence` HAS THREE
        // VALUES.** Absent here is not "use a default" — it is an ASK, for the
        // most the producer serves. A consumer reading `Optional` would conclude
        // it could leave them out safely and be right by accident; reading
        // `Ceiling` it knows what leaving them out requests.
        Param::new("detail", Enum(&["keys", "rows"]), Ceiling),
        // `retain` and `beat` are each `max | <n>`, which is an enum and an int
        // in one field. Declared as the int they are bounded by, because the
        // string `"max"` is the OMISSION spelled out loud and `Ceiling` already
        // says what omission means.
        Param::new("retain", Sc(Int), Ceiling),
        Param::new("beat", Duration(shapewire::operation::TimeUnit::Millis), Ceiling),
        Param::new("at", Timestamp(Rfc3339), Optional),
        // **`Required`, SINCE THE OPERATOR'S RULING OF 2026-10-08** — *"what I
        // want is consumers to be forced to say who they are so we can track
        // consumers."* It was `Optional`, and this declaration is what a
        // consumer READS to decide whether it may leave the param out; leaving
        // it `Optional` while `ConsumerIdentity::parse` refuses absence would
        // publish a promise the door breaks, which is worse than either answer
        // alone. The two are now the same statement.
        Param::new("consumer", Struct("ConsumerIdentity"), Required),
    ]
);

/// **EVERY DECLARATION ON THIS PRODUCER**, for the census.
///
/// Hand-assembled, and that is a known weakness: an operation declared and left
/// out of this list is invisible to `every_dtype_has_a_declared_instance`. A
/// linkme-style inventory would close it and would be the first dependency this
/// crate takes for a test's benefit, which is why it is written down instead.
pub const DECLARED: &[Declared] = &[
    Create::DECLARED,
    Send::DECLARED,
    Update::DECLARED,
    Event::DECLARED,
    History::DECLARED,
    Seen::DECLARED,
    QueueReselect::DECLARED,
    ReconcileTranscript::DECLARED,
    PoolShareSet::DECLARED,
    BudgetSet::DECLARED,
    Subscribe::DECLARED,
];

#[cfg(test)]
mod tests {
    //! **THE HALF A `const` CANNOT CHECK.**
    //!
    //! [`shapewire::operation::well_formed`] checks that a declaration is
    //! well formed, at compile time. Nothing checks that it is TRUE of the
    //! handler, because every handler here takes `params: &Value` and hand-parses
    //! it. These two tests are the narrower thing a test can do about that, and
    //! they go through `crate::server::route_request` for the reason that
    //! function's own docstring gives: a test that called `handle` directly would
    //! be addressing a handler by a name no consumer can send.

    use super::*;
    use shapewire::operation::Presence;
    use serde_json::json;

    fn config_at(dir: &std::path::Path) -> crate::Config {
        // ONE registered account with its own config dir: a thread runs on a
        // placeable account and never on the machine default (CON-27), so a
        // config with an empty roster cannot create one.
        let mut accounts = std::collections::BTreeMap::new();
        accounts.insert(
            "aa".to_string(),
            dir.join("aa").to_string_lossy().to_string(),
        );
        crate::Config {
            data_dir: Some(dir.to_string_lossy().to_string()),
            accounts,
            calibrate: false,
            edge_poll: false,
            probe_interval_secs: 0,
            ..Default::default()
        }
    }

    /// **A PARAMETER DECLARED `Required` IS REFUSED WHEN ABSENT, BY THE LIVE
    /// DOOR.**
    ///
    /// The declaration is a claim about the handler, and this is the only kind of
    /// evidence available for it here. Two operations, chosen because their
    /// required parameters are of two different dtypes and are enforced in two
    /// different places: `send`'s `text` is a `scalar` the handler demands, and
    /// `reconcile_transcript`'s `path` is a `path` that must exist on disk.
    ///
    /// **A DECLARATION NOBODY DRIVES IS THE STUB THAT PRETENDS TO WORK**, which
    /// is why this asserts against the router and not against `TAKES`.
    ///
    /// **IT NEEDS A REAL THREAD AND A POSITIVE CONTROL, AND THE FIRST CUT HAD
    /// NEITHER.** Written against `threads/t-1/send` on a thread that does not
    /// exist, the call errors because of the THREAD and the assertion passes
    /// whatever `text` does — the mutation below came back GREEN and that is how
    /// this was found. So the thread is created first, and the same call WITH
    /// `text` must succeed: the only difference between the two calls is the
    /// declared-required parameter, which is the only way the assertion can be
    /// about it.
    ///
    /// **THE MUTATION.** In `server.rs`'s `threads/{thread_id}/send` arm, replace
    /// `req_str(&params, "text")?` with
    /// `params.get("text").and_then(Value::as_str).unwrap_or("").to_string()` —
    /// a required parameter quietly defaulted, which is the drift this test
    /// exists to catch.
    ///
    /// Forced red:
    /// ```text
    /// `text` is declared Required on threads/{thread_id}/send, so the live door
    /// refuses it absent — and this thread EXISTS, so nothing else can be the
    /// reason: Ok(Object {"accepted": Bool(true), ...})
    /// ```
    #[tokio::test]
    async fn a_required_param_is_required_by_the_live_router() {
        let tmp = tempfile::tempdir().unwrap();
        let m = crate::build_manager(config_at(tmp.path())).unwrap();
        let created = crate::server::route_request(
            &m,
            "threads/create",
            json!({"settings": {"cwd": tmp.path(), "model": "claude-opus"}}),
        )
        .await
        .expect("a thread");
        let tid = created["thread_id"].as_str().unwrap().to_string();

        // The declarations under test, read off the declaration rather than
        // retyped: if somebody demotes `text` to Optional, this assertion goes
        // with it and the test below stops being about anything.
        assert!(
            Send::DECLARED
                .takes
                .iter()
                .any(|p| p.name == "text" && p.presence == Presence::Required),
            "the declaration says `text` is required — that is what is being \
             checked against the door"
        );

        // THE POSITIVE CONTROL, first: this call differs from the one below by
        // `text` and by nothing else.
        let ok =
            crate::server::route_request(&m, &format!("threads/{tid}/send"), json!({"text": "hi"}))
                .await;
        assert!(
            ok.is_ok(),
            "the same call WITH `text` succeeds, so the refusal below is about \
             `text` and not about the thread: {ok:?}"
        );
        let out = crate::server::route_request(&m, &format!("threads/{tid}/send"), json!({})).await;
        assert!(
            out.is_err(),
            "`text` is declared Required on threads/{{thread_id}}/send, so the \
             live door refuses it absent — and this thread EXISTS, so nothing \
             else can be the reason: {out:?}"
        );

        assert!(
            ReconcileTranscript::DECLARED
                .takes
                .iter()
                .any(|p| p.name == "path" && p.presence == Presence::Required),
            "…and `path` likewise"
        );
        // No positive control is available here — a successful reconcile needs a
        // real transcript on disk and an account to port it to — so the refusal
        // is pinned by NAMING the parameter instead. That is weaker, and it is
        // written down rather than dressed up.
        let out = crate::server::route_request(
            &m,
            &format!("threads/{tid}/reconcile_transcript"),
            json!({}),
        )
        .await;
        let why = format!("{out:?}");
        assert!(
            out.is_err() && why.contains("path"),
            "`path` is declared Required on \
             threads/{{thread_id}}/reconcile_transcript, and the refusal names \
             it: {why}"
        );
    }

    /// **AN `opaque` PAYLOAD COMES BACK BYTE FOR BYTE, AND THAT IS WHAT THE DTYPE
    /// PROMISES.**
    ///
    /// `threads/{thread_id}/event`'s `data` is the operation that forced `opaque`
    /// into the set. The declaration carries a PROHIBITION — the producer stores
    /// and returns it and may not branch on it — and the evidence for a
    /// prohibition is that a structure this crate has no vocabulary for survives
    /// the round trip unchanged.
    ///
    /// The fixture is deliberately nested, deliberately mixed-type, and
    /// deliberately contains a key (`event_type`) that this producer DOES have a
    /// meaning for at the top level — so a handler that reached into `data`
    /// looking for something it understood would have somewhere to go wrong.
    ///
    /// **THE MUTATION.** In `server.rs`'s `threads/{thread_id}/event` arm, wrap
    /// the payload: `let data = json!({ "data": data });` — the producer touching
    /// a value it declared it would not read.
    ///
    /// Forced red:
    /// ```text
    /// an `opaque` payload is returned exactly as it was sent — the producer
    /// stores it and does not read it
    /// ```
    #[tokio::test]
    async fn an_opaque_payload_is_returned_exactly_as_it_was_sent() {
        let tmp = tempfile::tempdir().unwrap();
        let m = crate::build_manager(config_at(tmp.path())).unwrap();

        let created = crate::server::route_request(
            &m,
            "threads/create",
            json!({"settings": {"cwd": tmp.path(), "model": "claude-opus"}}),
        )
        .await
        .expect("a thread");
        let tid = created["thread_id"].as_str().unwrap().to_string();

        let payload = json!({
            "event_type": "not-the-outer-one",
            "nested": {"a": [1, 2, {"b": true}], "c": null},
            "z": "last"
        });
        crate::server::route_request(
            &m,
            &format!("threads/{tid}/event"),
            json!({"event_type": "custom.thing", "data": payload}),
        )
        .await
        .expect("the event lands");

        let hist =
            crate::server::route_request(&m, &format!("threads/{tid}/history"), json!({}))
                .await
                .expect("history");
        let back = hist["events"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|e| {
                let ev = &e["event"];
                (ev["event_type"] == "custom.thing").then(|| ev["data"].clone())
            })
            .expect("the custom event is in the log");

        assert_eq!(
            back, payload,
            "an `opaque` payload is returned exactly as it was sent — the \
             producer stores it and does not read it"
        );
    }
}
