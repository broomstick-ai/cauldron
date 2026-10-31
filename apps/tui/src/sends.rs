//! **EVERY MUTATION THIS FACE ISSUES, IN ONE MODULE — and the set is ONE.**
//!
//! `grep -n 'Verb::' apps/tui/src/sends.rs` enumerates it in one command, and
//! the answer is `Verb::Send` — `threads/{thread_id}/send`.
//!
//! **THAT GREP HAS NOW MOVED TWICE, AND BOTH MOVES ARE THE LESSON.** It was
//! `'"threads\.'` until the daemon re-pathed its surface, at which point the
//! dotted spelling stopped existing and a guard keyed to a spelling stopped
//! biting. It became `'methods::'`, over a module whose whole job was to be the
//! one place a name was written. That module is now DELETED, because a list of
//! names a face can speak is a weaker thing than a TYPE: `methods.rs` made the
//! surface greppable and [`crate::acts::Verb`] makes it exhaustive. A verb this
//! face has no variant for cannot be constructed, so there is nothing left for
//! a grep to miss.
//!
//! # Why a module of its own
//!
//! `src/reads.rs` stood beside this one and its header was the READ-ONLY claim.
//! It is gone: four of its six reads moved to the mirror and two had no shape
//! to move to, and those two live in [`crate::queue_reads`] with the ruling they
//! are owed written at the top. So the two claims are now: *this face reads
//! nothing but the mirror and two named exceptions*, and *`sends.rs` issues
//! exactly one mutation*, stated here and checked by its own test.
//!
//! **There is no `Handle::call` left to abuse.** The generic door —
//! `call(&method, params)` — is gone with the legacy transport;
//! [`crate::mirror::Handle`] offers three verbs, one resource read and one
//! two-element carve-out, and that is the whole of what this process can say.
//!
//! # The queue verbs are still NOT here
//!
//! `threads/{thread_id}/queue_cancel` and `threads/{thread_id}/queue_reselect`
//! remain uncalled. The QUEUE the composer's `^Q` fills is the CLIENT's —
//! it lives in `compose::Composer::pending` and never reaches the daemon until
//! a send stitches it into one payload. So the queue view stays view-only and
//! the daemon's queue is still nothing this face mutates. That is a smaller
//! surface than it looks: his grammar asks for queue and send-the-whole-payload
//! as two ACTS, not for two RPCs, and the AUI implements exactly the same pair
//! against exactly the same one verb.

use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::event::Ev;
use crate::acts::Verb;
use crate::mirror::Handle;

/// **THE ONE MUTATION.** Send the whole payload as a single user message.
///
/// `intent: "immediate"` is the daemon's default and the AUI's choice
/// (`apps/aui/src/threads_client.rs`): deliver as soon as no parent turn is in
/// flight, never interrupting a running turn mid-flight. The alternative the
/// daemon offers, `turn_end`, waits for every async subagent to go quiet, which
/// is a different promise from the one a person typing into a chat box is
/// making.
///
/// **The composer is NOT emptied by this call.** It is emptied by the `Ev::Sent`
/// that comes back saying the daemon accepted it — and only by that. A box
/// cleared optimistically is a box that eats his words when the socket was down,
/// and the transport refuses a call on a dead link loudly rather than queueing
/// it, so the refusal really does come back.
pub fn spawn_send(
    handle: Handle,
    tx: mpsc::Sender<Ev>,
    thread_id: String,
    text: String,
    parts: usize,
) {
    tokio::spawn(async move {
        tracing::info!(
            %thread_id,
            chars = text.chars().count(),
            queued_parts = parts,
            "issuing the one mutation"
        );
        crate::diag!(
            "send",
            "ISSUE thread={thread_id} bytes={} chars={} queued_parts={parts} text={}",
            text.len(),
            text.chars().count(),
            crate::diag::preview(&text, 40)
        );
        // NO `thread_id` IN THE PARAMS. It is the SUBJECT of
        // `threads/{thread_id}/send` and rides the PATH; the daemon refuses a
        // call that spells it twice, whether the two agree or not. That rule is
        // kept in ONE place now — `seam::commands::send`, which builds the path
        // from the `thread` field below — rather than at every call site.
        let verb = Verb::Send {
            thread: thread_id.clone(),
            text,
            intent: Some("immediate".into()),
            // `bare: false` — this is a real user message and the daemon should
            // treat it as one. `/compact` is the `bare: true` caller, and it
            // builds its own verb in `acts::compact_verb`.
            bare: false,
            // The structure the daemon DOES read and render. This face has
            // nothing to assert about the message that the message does not
            // already say, so it declares none.
            headers: Vec::new(),
            // The blob the daemon STORES and never reads. It carries provenance
            // and nothing else: which face wrote this, and whether it was one
            // message or several stitched into one payload.
            metadata: json!({
                "tui": { "client": "harness-tui", "queued_parts": parts }
            }),
        };
        let result = handle
            .verb(verb)
            .await
            .and_then(|ack| accepted(&ack.payload));
        let _ = tx.send(Ev::Sent { thread_id, result }).await;
    });
}

/// Read the daemon's answer, and REFUSE A SUCCESS IT DID NOT GIVE.
///
/// `threads/{thread_id}/send` answers
/// `{accepted, thread_id, message_id, header_warnings}` — it is one of the
/// commands that did NOT move to a bare acknowledgement, so this guard still
/// reads a real `accepted`.
/// A response missing `accepted: true` is treated as a failure even though the
/// RPC layer called it a success, because the composer empties on this value: a
/// shape we did not expect must not be allowed to look like a delivered
/// message.
fn accepted(v: &Value) -> Result<Option<u64>, String> {
    if v.get("accepted").and_then(Value::as_bool) != Some(true) {
        return Err(format!(
            "the daemon answered the send without accepting it: {v}"
        ));
    }
    Ok(v.get("message_id").and_then(Value::as_u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **THE MUTATION SURFACE, AS A TEST — and it is a DIFFERENT KIND of test
    /// now, because the thing it guards changed shape.**
    ///
    /// What stood here asserted over `methods::` references and over two
    /// spellings of a hand-written literal. Both halves are retired with
    /// `methods.rs`: there is no name to reference and no `call(&method,
    /// params)` door for a literal to go through. What is left to guard is the
    /// ENUM, so that is what this reads — [`crate::acts::Verb`] has exactly
    /// three variants, this module constructs exactly one of them, and a fourth
    /// verb is a red here before it is a call on the wire.
    ///
    /// **THE SENTINEL IS FIRST, BECAUSE THE SCAN IS THE PART THAT DIES
    /// QUIETLY.** A `match_indices` that matches nothing makes every assertion
    /// after it vacuous at once, and this crate has shipped exactly that: seven
    /// negative claims over a spelling that had stopped existing, all trivially
    /// true, all silent. So the scan must FIND `Verb::Send` in this file before
    /// it is believed about the two it does not find.
    #[test]
    fn this_module_issues_exactly_one_verb_and_it_is_send() {
        let whole = include_str!("sends.rs");
        let src = whole
            .split_once("#[cfg(test)]")
            .expect("the test module is the end of this file")
            .0;
        // **WHOLE-LINE COMMENTS ARE DROPPED**, because this module's header has
        // to go on naming the verbs it refuses to call — a prohibition is worth
        // nothing if the reason for it cannot be written beside it. A TRAILING
        // comment on a code line is deliberately still in scope, so the strip
        // cannot be used as a hiding place.
        let code: String = src
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        // ── THE SENTINEL ────────────────────────────────────────────────────
        assert!(
            code.contains("Verb::Send"),
            "this module constructs no verb at all, so every claim below is \
             about a file that sends nothing"
        );
        // ── AND THE CLAIM: ONE, AND IT IS `Send` ────────────────────────────
        for other in ["Verb::Cancel", "Verb::Create"] {
            assert!(
                !code.contains(other),
                "{other} is constructed in the one-mutation module; it belongs \
                 in `acts.rs`, where the act that asks for it lives"
            );
        }
        // No method PATH, in either grammar. Kept even though the door is gone,
        // because `Handle::unshaped_read` exists and takes a leaf: a
        // hand-written path here would be the shape of a read smuggled into the
        // mutation module.
        for needle in ["\"threads/", "\"threads."] {
            assert!(
                !code.contains(needle),
                "a method path is written by hand in the one-mutation module"
            );
        }
        // THE SAME BAN, BY NAME, so a reader sees what is excluded rather than
        // inferring it from a set of one. These are leaves of `threads` that
        // EXIST in the producer's fifty-two and that this face must not speak.
        //
        // `queue_retry` and `delete` used to be on this list and are GONE from
        // it: neither appears in the producer's table, and the live daemon
        // answers `unknown method` for both. **A ban on a method that does not
        // exist is not a guard, it is a sentence that can never fail.**
        for never in [
            "queue_cancel",
            "queue_reselect",
            "update",
            "seen",
            "create",
            "event",
        ] {
            assert!(
                !code.contains(&format!("/{never}")),
                "{never} is named in this module's code — it is not in this \
                 face's surface, and a hand-built path is not a loophole"
            );
        }
    }

    /// **A RESPONSE THAT DID NOT SAY `accepted` IS A FAILURE**, even though the
    /// RPC succeeded. The composer empties on this value, so anything short of
    /// an explicit acceptance has to come back as an error he can see.
    #[test]
    fn only_an_explicit_acceptance_empties_the_composer() {
        assert_eq!(
            accepted(&json!({"accepted": true, "message_id": 1311})),
            Ok(Some(1311))
        );
        // Accepted with no id: legal, and still an acceptance.
        assert_eq!(accepted(&json!({"accepted": true})), Ok(None));
        for refused in [
            json!({}),
            json!({"message_id": 7}),
            json!({"accepted": false}),
            json!({"accepted": "yes"}),
            Value::Null,
        ] {
            assert!(
                accepted(&refused).is_err(),
                "{refused} was read as a delivered message"
            );
        }
    }
}
