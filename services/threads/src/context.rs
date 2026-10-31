//! Agent-facing context injection (Q7, §12).
//!
//! The operator locked the header contract down to the minimum: "since the
//! agent already knows its cwd, the agent-facing header needs only: thread id,
//! message id, date + time" (Q7 — this deliberately DROPS AUI's
//! `[SCOPE: project=… path=…]` fields). Two surfaces are built here:
//!
//! 1. **The per-message metadata block** — one `<thread_system_notice
//!    kind="message_metadata">` above each user message in a turn's prompt,
//!    composed HERE. Threads owns these facts, so threads emits them: the AUI
//!    used to build this preamble out of its own private counters, and the
//!    message ids it spoke drifted out of alignment with the ledger that owns
//!    them ("it doesn't make a lick of sense that they can inject these message
//!    numbers in there if they're not aligned with the source of truth" —
//!    INTENT 60). The payload is NOT every field on every post (INTENT 65): see
//!    [`MetadataPolicy`]. The superseded shape, `[#N YYYY-MM-DD HH:MM:SS]`,
//!    survives as [`message_header`] for the citation/replay surfaces. **EVERY
//!    LINE OF THE BLOCK HAS ITS OWN FLAG** on
//!    [`crate::settings::Injection`], and the block itself has none: it is
//!    rendered exactly when at least one of its lines is, so a thread can ask
//!    for one line and get a one-line block, or for none and get no block.
//!    One of those lines is OPT-IN rather than on by default — the
//!    [`context_id`], a retrieval-dedup cache key.
//! 2. **Preambles** — a `[thread <id>]` identity line plus plugin paths on the
//!    first turn ("the agent MUST be told each plugin's path so it can edit the
//!    plugin when asked"), and the FULL post-compaction restatement (§12):
//!    working path, plugin paths, thread id, date/time, and the pre-compaction
//!    message-id range so the agent can recover state via specific prior
//!    messages.
//!
//! Everything injected is also persisted as a `ContextInjected` ledger event
//! by the runtime, so the record shows exactly what the agent was told.

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::{Audience, StoredEvent, ThreadEvent};

/// One queued user message being delivered in a turn.
pub struct PromptSection {
    pub message_id: u64,
    /// When the message was accepted (UTC, from the ledger).
    pub ts: DateTime<Utc>,
    pub text: String,
    /// How long the operator's audio ran, ALREADY FORMATTED, when the caller
    /// supplied enough to compute it. `None` omits the line.
    pub duration: Option<String>,
    /// The APPLICATION's own coordinate for this message — see
    /// [`APP_IDX_KEYS`]. `None` omits the line; threads never invents one.
    pub idx: Option<u64>,
    /// Headers this message's sender DECLARED, already validated at accept
    /// time. Threads renders them; the application never supplies markup.
    pub headers: Vec<HeaderDeclaration>,
}

/// The `[#N YYYY-MM-DD HH:MM:SS]` header for one message (local wall clock —
/// the operator's frame of reference).
///
/// **SUPERSEDED as the delivered shape** by [`message_metadata_notice`]
/// (INTENT 65): the per-message facts now ride an XML envelope threads composes
/// itself. Kept because it is the compact form the ledger-replay and citation
/// surfaces render with, where an XML envelope per line would be noise.
pub fn message_header(message_id: u64, ts: DateTime<Utc>) -> String {
    let local: DateTime<Local> = ts.with_timezone(&Local);
    format!("[#{message_id} {}]", local.format("%Y-%m-%d %H:%M:%S"))
}

/// The per-delivery inputs the metadata policy needs from the thread.
pub struct MetadataPolicy<'a> {
    /// **ONE FLAG PER INJECTED THING** — see [`crate::settings::Injection`].
    /// The per-message metadata block is not among them: it is DERIVED, and
    /// exists exactly when at least one of its line flags is on.
    pub inject: crate::settings::Injection,
    pub thread_id: &'a str,
    /// State the thread id on the FIRST section of this delivery — true on the
    /// first post of the thread and on the first post after a compaction, and
    /// on nothing else (INTENT 65).
    pub state_thread_id: bool,
    /// The CONTEXT ID for the window this delivery lands in — see
    /// [`context_id`]. `Some` exactly when the thread has opted in via
    /// `Injection::context_id`; `None` for everything else, and the line is
    /// then simply absent.
    ///
    /// It is a rendered string rather than a `u64` boundary deliberately: the
    /// derivation lives in ONE function and this struct carries its result,
    /// so there is no second place that could format it differently.
    pub context_id: Option<&'a str>,
    /// State the context id on the FIRST section of this delivery.
    ///
    /// A SECOND predicate beside [`Self::state_thread_id`] rather than the same
    /// one, because the two are no longer the same question. The thread id rides
    /// the boundary posts and nothing else. The context id rides the boundary
    /// posts **and** the first post after the flag was turned on mid-context —
    /// see `ThreadManager::build_prompt`, which computes it, and the INTERIM
    /// note there. `false` omits the line however good the id in
    /// [`Self::context_id`] is.
    pub state_context_id: bool,
    /// The calendar-day key last stated to this thread (`None` = never stated).
    /// A section whose day differs from this restates the date; one that
    /// matches elides it.
    pub last_date: Option<String>,
    /// Now, for [`guard_injection`]. The metadata block is machinery the daemon
    /// injects, so CON-22 applies to it exactly as it does to a preamble.
    pub now: DateTime<Utc>,
    /// The application identity stamped on every application-declared header
    /// (`app="…"`). It comes from the THREAD's settings, never from the
    /// message being delivered — see [`application_header`] for why that
    /// distinction is the security property and not a detail.
    pub app: Option<&'a str>,
}

/// Build the user-visible body of a turn prompt from its delivered sections,
/// each preceded by its metadata block. Multiple sections (a queue drain) keep
/// the `Queued messages:` lead-in.
///
/// Returns the body and the calendar-day key the caller must PERSIST, so the
/// date elision survives a daemon restart — which matters, because the daemon
/// now restarts itself on its own binary change.
///
/// The date rule runs ACROSS sections in order, so a queue that spans midnight
/// restates the date mid-block on exactly the section that crossed it.
///
/// **THE BLOCK ORDER, DECIDED AND FIXED HERE** (INTENT 66a — headers are
/// plural, so their order is now a contract and not an accident). Per section,
/// top to bottom:
///
/// 1. threads' own `message_metadata` notice — what THREADS asserts;
/// 2. every application-declared header, in the order the application
///    declared them — what an application ASKED to have asserted;
/// 3. threads' `body_scrutiny` notice, present only when scrutiny actually
///    fired;
/// 4. the operator's own words, with any forged control structure neutralised.
///
/// The rule is: **each block sits immediately above what it describes, and the
/// least trusted text is last.** (1) and (2) describe the message as a whole,
/// threads-asserted before application-asked-for, so a reader meets the
/// trusted frame first. (3) is deliberately NOT hoisted up with (1) even
/// though threads asserts it too: it is a statement about the literal
/// characters that follow it, and adjacency is what makes it unambiguous which
/// text it is talking about. Its tag still says `thread_system_notice
/// source="threads"`, so its provenance never depends on its position.
pub fn build_prompt_body(
    sections: &[PromptSection],
    policy: &MetadataPolicy,
) -> (String, Option<String>) {
    let mut last_date = policy.last_date.clone();
    let mut rendered: Vec<String> = Vec::with_capacity(sections.len());
    for (i, s) in sections.iter().enumerate() {
        let mut blocks: Vec<String> = Vec::new();

        // ── THE PER-MESSAGE METADATA BLOCK, LINE BY LINE ─────────────────
        //
        // There is no flag for the BLOCK. Each line answers to its own flag and
        // the envelope is DERIVED: `message_metadata_notice` returns `None` when
        // every line is absent, and nothing is pushed. That is what makes "ask
        // for the context id and nothing else" a coherent request and leaves no
        // combination of these flags that means nothing — which is why the old
        // `ensure_context_id_coherent` had nothing left to refuse.
        //
        // The date elision state advances only when the `sent:` line is
        // actually being stated: with the timestamp off there is nothing for a
        // later delivery to elide against, so recording a day would be
        // recording a statement that was never made.
        let sent = if policy.inject.timestamp {
            let key = local_date_key(s.ts);
            let show_date = last_date.as_deref() != Some(key.as_str());
            if show_date {
                last_date = Some(key);
            }
            Some(SentStamp {
                ts: s.ts,
                show_date,
            })
        } else {
            None
        };
        // A BOUNDARY POST: the first section of a delivery that is the first
        // post of the thread or the first after a compaction. A queue drain is
        // one arrival, so only section 0 can be one — repeating the id under
        // every queued message is the token waste the policy exists to avoid.
        let boundary = policy.state_thread_id && i == 0;
        let meta = MessageMeta {
            thread_id: if policy.inject.thread_id && boundary {
                Some(policy.thread_id)
            } else {
                None
            },
            // THE SAME THREE CONDITIONS AS EVERY OTHER LINE: the flag (is this
            // line wanted), the value (what it says), the position (which post
            // states it). The flag is tested HERE and not only where the value
            // is computed, so there is exactly one place that decides whether
            // the line exists — a renderer that trusted `context_id.is_some()`
            // alone would be a second representation of the flag, which is the
            // drift class this crate keeps getting bitten by.
            //
            // The context id's POSITION has its own predicate because it no
            // longer rides the same posts as the thread id in every case — see
            // `MetadataPolicy::state_context_id`.
            context_id: if policy.inject.context_id && policy.state_context_id && i == 0 {
                policy.context_id
            } else {
                None
            },
            message_id: if policy.inject.message_id {
                Some(s.message_id)
            } else {
                None
            },
            idx: if policy.inject.app_idx { s.idx } else { None },
            sent,
            // The two-number gloss rides exactly where the thread id rides:
            // once at the top of a thread and once after a compaction. Both are
            // "the model has no history here", which is the only moment the
            // gloss buys anything.
            gloss: boundary,
            duration: if policy.inject.duration {
                s.duration.clone()
            } else {
                None
            },
        };
        if let Some(block) = message_metadata_notice(&meta) {
            blocks.push(block);
        }

        // THE APPLICATION'S DECLARATIONS, RENDERED BY THREADS. Every one of
        // these was validated at accept time by `validate_declarations`; the
        // application supplied names and values, and this line is the only
        // place markup for them has ever existed.
        if policy.inject.application_headers {
            for decl in &s.headers {
                blocks.push(application_header(policy.app, decl));
            }
        }

        // BODY SCRUTINY (INTENT 66b). The operator's own words are examined
        // for structure that would read as one of the control blocks above,
        // and any such structure is neutralised — visibly and reversibly, and
        // announced rather than done in silence.
        //
        // It fires REGARDLESS of `level`, and the notice rides even on a
        // thread that injects nothing else. Turning injection off says "tell
        // the model nothing extra"; it does not say "edit his words without
        // saying so". Announcing an edit is part of making it, not context.
        let (safe_text, neutralised) = guard_body_structure(&s.text);
        if neutralised > 0 {
            blocks.push(body_scrutiny_notice(neutralised));
        }

        // CON-22 ON THE BLOCKS, AND ONLY ON THE BLOCKS. Everything threads
        // injects passes the same guard every preamble does — line by line, so
        // a single bad line costs that line and not the message id underneath
        // it. The operator's own text is deliberately NOT passed through
        // `guard_injection`: his words are history, they are not the daemon
        // predicting capacity, and silently dropping a line of what he said
        // would be a far worse failure than the one CON-22 prevents. Body
        // scrutiny above is the narrow, structure-only exception, and it
        // rewrites markup rather than deleting a line.
        //
        // `guard_injection` on an EMPTY block list yields an empty string and
        // the `trim().is_empty()` arm below pushes the body alone — no envelope,
        // no blank line. That is what `metadata_level: none` emitted and it is
        // what every-flag-off emits, which is why the derived block is parity
        // rather than a change.
        let guarded = guard_injection(&blocks.join("\n"), policy.now);
        rendered.push(if guarded.trim().is_empty() {
            safe_text
        } else {
            format!("{guarded}\n{safe_text}")
        });
    }
    // The `Queued messages:` lead-in is a thing threads injects, so it has a
    // flag of its own rather than riding the metadata block's — under flat flags
    // there is no block flag for it to ride. A single-section delivery never
    // carries it whatever the flag says, because there is no queue to lead.
    let body = match (rendered.len(), policy.inject.queue_lead_in) {
        (1, _) => rendered.remove(0),
        (_, false) => rendered.join("\n\n"),
        _ => format!("Queued messages:\n{}", rendered.join("\n\n")),
    };
    (body, last_date)
}

/// The first-turn preamble: thread identity + plugin paths.
pub fn first_turn_preamble(thread_id: &str, plugin_dirs: &[String]) -> String {
    let mut out = format!("[thread {thread_id}]");
    if !plugin_dirs.is_empty() {
        out.push_str("\nPlugins attached to this thread (editable at these paths):");
        for d in plugin_dirs {
            out.push_str(&format!("\n- {d}"));
        }
    }
    out
}

/// The post-compaction restatement (§12): assume ALL id/metadata context was
/// lost in the compacted summary and re-state it, including the message-id
/// range the summary swallowed.
pub fn compaction_restatement(
    thread_id: &str,
    cwd: &str,
    plugin_dirs: &[String],
    pre_range: Option<(u64, u64)>,
    now_utc: DateTime<Utc>,
) -> String {
    let local: DateTime<Local> = now_utc.with_timezone(&Local);
    let mut out = String::from(
        "Context restatement (this thread's conversation was just compacted; \
         the summary above may have lost the following):",
    );
    out.push_str(&format!("\n- Thread id: {thread_id}"));
    out.push_str(&format!("\n- Working path: {cwd}"));
    out.push_str(&format!(
        "\n- Now: {} (local)",
        local.format("%Y-%m-%d %H:%M:%S")
    ));
    if let Some((first, last)) = pre_range {
        out.push_str(&format!(
            "\n- The compacted chunk covered message ids #{first}–#{last}; \
             retrieve specific messages by id from the threads ledger to recover detail."
        ));
    }
    if !plugin_dirs.is_empty() {
        out.push_str("\n- Plugins attached to this thread (editable at these paths):");
        for d in plugin_dirs {
            out.push_str(&format!("\n  - {d}"));
        }
    }
    out
}

/// A plugin-set-changed restatement (hot reload notice for the agent).
pub fn plugins_changed_preamble(plugin_dirs: &[String]) -> String {
    let mut out = String::from("The plugin set attached to this thread changed. Current plugins:");
    for d in plugin_dirs {
        out.push_str(&format!("\n- {d}"));
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════════
// THE SYSTEM NOTICE — Q-38, answered (DEC-55 category (b) delivery)
// ═══════════════════════════════════════════════════════════════════════════

/// **THE ENVELOPE TAG. This is contract.**
///
/// Q-38 was dealer's choice ("prefix the user message with some XML that's
/// like application info or system info… but you choose"). The choice made and
/// recorded here: threads' own system events arrive as a PREFIX on the next
/// user message, wrapped in
///
/// ```text
/// <thread_system_notice source="threads" kind="resumed_after_interrupt">
///   …one short factual block…
/// </thread_system_notice>
/// ```
///
/// The tag is deliberately NOT `<system-reminder>`, `<system>` or
/// `<system-notification>`: the host harness injects `<system-reminder>` into
/// the same context, and a model must never have to guess whether a notice
/// came from Claude Code or from the thread service. `thread_system_notice` can
/// collide with neither, and it says its own provenance in its name.
pub const NOTICE_TAG: &str = "thread_system_notice";

/// The `source` attribute — always `threads`, so the element is
/// self-describing even quoted out of context.
pub const NOTICE_SOURCE: &str = "threads";

/// `kind` attribute: the thread was cut off and is being told so at resume.
pub const NOTICE_KIND_RESUMED: &str = "resumed_after_interrupt";
/// `kind` attribute: this turn waited between the operator's intent and its
/// delivery (DEC-57).
pub const NOTICE_KIND_QUEUED: &str = "queued_turn";
/// `kind` attribute: a hard break happened (used when the notice is rendered
/// from the interrupt event itself rather than from the resume).
pub const NOTICE_KIND_INTERRUPTED: &str = "interrupted";
/// `kind` attribute: the PER-MESSAGE metadata block (INTENT 65 / DIR-56 /
/// DEC-70). One per delivered user message, sitting immediately above that
/// message's text — it replaces the bare `[#N date time]` header.
pub const NOTICE_KIND_MESSAGE_META: &str = "message_metadata";

/// `kind` attribute: threads NEUTRALISED forged control structure in the
/// user's own message body (INTENT 66b). Emitted only when scrutiny fired.
pub const NOTICE_KIND_BODY_SCRUTINY: &str = "body_scrutiny";

/// Wrap one factual block in the envelope. `body` is indented two spaces so
/// the element reads as machinery at a glance.
pub fn system_notice(kind: &str, body: &str) -> String {
    format!(
        "<{NOTICE_TAG} source=\"{NOTICE_SOURCE}\" kind=\"{kind}\">\n{}\n</{NOTICE_TAG}>",
        indent_two(body)
    )
}

/// Two-space body indent, shared by every envelope in this module so threads'
/// notices and application headers cannot drift apart visually.
fn indent_two(body: &str) -> String {
    body.lines()
        .map(|l| {
            if l.is_empty() {
                String::new()
            } else {
                format!("  {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ═══════════════════════════════════════════════════════════════════════════
// THE HEADER PROTOCOL — INTENT 66a (headers are plural) and 66b (an
// application DECLARES; threads COMPOSES; the user's body is SCRUTINISED)
// ═══════════════════════════════════════════════════════════════════════════

/// **THE APPLICATION-DECLARED HEADER ENVELOPE. This is contract.**
///
/// A DIFFERENT TAG from [`NOTICE_TAG`], deliberately, and this is the whole
/// security value of the feature rather than a cosmetic choice. The operator's
/// framing (INTENT 66b): a header is a control block sitting inside untrusted
/// text, so "we treat these message headers as a potential security
/// vulnerability, a position of potential prompt injection."
///
/// A model reading a prompt must be able to answer one question with no
/// ambiguity: *did the thread service assert this, or did an application ask
/// for it to be asserted?* `<thread_system_notice source="threads">` is the
/// first. `<application_header app="…">` is the second. They cannot be
/// confused for one another, they cannot be produced by each other's code
/// path, and an application can never emit the first — the `kind` an
/// application declares is validated against [`RESERVED_KINDS`], and even a
/// permitted `kind` lands inside THIS tag, never that one.
pub const APP_HEADER_TAG: &str = "application_header";

/// The tags [`guard_body_structure`] neutralises when it finds them written
/// out in the user's own message body. Both control-block tags: a body that
/// speaks one of these is a body trying to forge structure.
pub const RESERVED_TAGS: &[&str] = &[NOTICE_TAG, APP_HEADER_TAG];

/// `kind` values threads reserves for its OWN notices. A declaration naming
/// one of these is refused outright — belt to the tag's braces, so a reader
/// who filters on `kind` alone is still not fooled.
pub const RESERVED_KINDS: &[&str] = &[
    NOTICE_KIND_RESUMED,
    NOTICE_KIND_QUEUED,
    NOTICE_KIND_INTERRUPTED,
    NOTICE_KIND_MESSAGE_META,
    NOTICE_KIND_BODY_SCRUTINY,
];

/// Bounds on a declaration. An application shares the operator's context
/// window; unbounded declared headers are a denial-of-attention channel even
/// when every character in them is escaped.
pub const MAX_DECLARED_HEADERS: usize = 4;
/// Fields per declared header.
pub const MAX_DECLARED_FIELDS: usize = 16;
/// Characters per declared value, after coercion.
pub const MAX_DECLARED_VALUE_CHARS: usize = 512;
/// Characters in a declared `kind` or field `name`.
pub const MAX_DECLARED_NAME_CHARS: usize = 48;

/// One name/value pair inside a declared header. **A value, never markup.**
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderField {
    pub name: String,
    pub value: String,
}

/// **WHAT AN APPLICATION DECLARES.** Not a string, not a template, not a tag:
/// a `kind` out of a fixed charset plus ordered name/value pairs. There is no
/// field on this type through which markup can be supplied, which is the
/// point — the application cannot write a header even if it wants to, because
/// the type it hands over has nowhere to put one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderDeclaration {
    pub kind: String,
    /// ORDERED — an array on the wire, not a map, so the rendered order is the
    /// declared order and a test can pin it.
    pub fields: Vec<HeaderField>,
}

/// The charset a declared `kind` or field `name` must match: an ASCII
/// lowercase letter, then lowercase letters, digits and underscores.
///
/// Narrow on purpose. A name is an identifier a model reads, not free text; a
/// charset with no `<`, `>`, `"`, `'`, `&`, whitespace or control characters
/// in it cannot express markup at all, so validation here is a proof rather
/// than a filter that has to be kept ahead of an attacker.
fn valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    s.chars().count() <= MAX_DECLARED_NAME_CHARS
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// **THE DECLARATION GATE.** Turn a caller's raw JSON into declarations
/// threads is willing to render, plus a plain-English list of what it refused
/// and why.
///
/// **A refusal never costs the operator his message.** Callers run this at
/// ACCEPT time and hand the warnings back in the send response, so an
/// application learns its declaration was malformed while the operator's words
/// go through untouched. Validating once at accept and storing only the
/// accepted result also means the delivery path renders without re-deciding
/// anything — there is exactly one place a declaration can be judged.
///
/// Accepted shape: `null` or `[]` (no headers), or an array of
/// `{"kind": "<identifier>", "fields": [{"name": "<identifier>",
/// "value": <string|number|bool>}]}`.
pub fn validate_declarations(raw: &Value) -> (Vec<HeaderDeclaration>, Vec<String>) {
    let mut out: Vec<HeaderDeclaration> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    if raw.is_null() {
        return (out, warnings);
    }
    let Some(items) = raw.as_array() else {
        warnings.push("headers: expected an array of declarations; ignored".into());
        return (out, warnings);
    };
    for (i, item) in items.iter().enumerate() {
        if out.len() >= MAX_DECLARED_HEADERS {
            warnings.push(format!(
                "headers[{i}]: refused, at most {MAX_DECLARED_HEADERS} declared headers per message"
            ));
            continue;
        }
        let Some(obj) = item.as_object() else {
            warnings.push(format!("headers[{i}]: expected an object; refused"));
            continue;
        };
        let kind = obj.get("kind").and_then(Value::as_str).unwrap_or_default();
        if !valid_identifier(kind) {
            warnings.push(format!(
                "headers[{i}]: kind {kind:?} is not a lowercase identifier; refused"
            ));
            continue;
        }
        if RESERVED_KINDS.contains(&kind) {
            warnings.push(format!(
                "headers[{i}]: kind {kind:?} is reserved by threads; refused"
            ));
            continue;
        }
        let raw_fields = match obj.get("fields") {
            Some(Value::Array(a)) => a.clone(),
            None => Vec::new(),
            Some(_) => {
                warnings.push(format!("headers[{i}]: fields must be an array; refused"));
                continue;
            }
        };
        let mut fields: Vec<HeaderField> = Vec::new();
        for (j, f) in raw_fields.iter().enumerate() {
            if fields.len() >= MAX_DECLARED_FIELDS {
                warnings.push(format!(
                    "headers[{i}].fields[{j}]: refused, at most {MAX_DECLARED_FIELDS} fields"
                ));
                continue;
            }
            let name = f.get("name").and_then(Value::as_str).unwrap_or_default();
            if !valid_identifier(name) {
                warnings.push(format!(
                    "headers[{i}].fields[{j}]: name {name:?} is not a lowercase identifier; refused"
                ));
                continue;
            }
            // SCALARS ONLY. A nested object or array has no one-line rendering,
            // and inventing one would be threads guessing at an application's
            // structure — exactly the improvisation this protocol removes.
            let value = match f.get("value") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::Bool(b)) => b.to_string(),
                _ => {
                    warnings.push(format!(
                        "headers[{i}].fields[{j}]: value must be a string, number or boolean; refused"
                    ));
                    continue;
                }
            };
            if value.chars().count() > MAX_DECLARED_VALUE_CHARS {
                warnings.push(format!(
                    "headers[{i}].fields[{j}]: value longer than {MAX_DECLARED_VALUE_CHARS} characters; refused"
                ));
                continue;
            }
            fields.push(HeaderField {
                name: name.to_string(),
                // FLATTENED HERE, not at render time. A newline inside a value
                // would let one declared field become two rendered lines — the
                // one structural escape an XML-escaped value still has, since
                // the envelope's body is line-oriented. Control characters
                // collapse to a single space, which preserves the words and
                // destroys the structure.
                value: flatten_value(&value),
            });
        }
        out.push(HeaderDeclaration {
            kind: kind.to_string(),
            fields,
        });
    }
    (out, warnings)
}

/// Collapse every control character (newline, carriage return, tab, and the
/// rest of C0/C1) to a single space, so a declared value occupies exactly the
/// one line it was rendered onto.
fn flatten_value(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Render one declared header. **THREADS writes this; the application never
/// did and never can.**
///
/// `app` is the thread's declared application identity, taken from the THREAD
/// SETTINGS rather than from the message. That is deliberate: settings changes
/// are audited in the ledger as `SettingsChanged` events, so an identity
/// change is a visible act, whereas a per-message `app` field would let
/// message N claim to be one application and message N+1 another with nothing
/// recording the switch. An unset identity renders `app="unidentified"` — a
/// plainly wrong-looking value beats a plausible default.
pub fn application_header(app: Option<&str>, decl: &HeaderDeclaration) -> String {
    let body: String = decl
        .fields
        .iter()
        .map(|f| format!("{}: {}", f.name, xml_escape(&f.value)))
        .collect::<Vec<_>>()
        .join("\n");
    // The attributes are escaped even though both are charset-validated
    // identifiers and cannot contain a quote. Escaping something already
    // provably safe costs nothing and means the guarantee survives whoever
    // relaxes `valid_identifier` in two years without reading this comment.
    format!(
        "<{APP_HEADER_TAG} app=\"{}\" kind=\"{}\">\n{}\n</{APP_HEADER_TAG}>",
        xml_escape(app.unwrap_or("unidentified")),
        xml_escape(&decl.kind),
        indent_two(&body)
    )
}

/// The notice threads emits when [`guard_body_structure`] actually fired.
///
/// **Neutralising in silence was the other option and it was refused.** The
/// operator's standing rule is that his words are never quietly altered; a
/// reader who sees `&lt;thread_system_notice` in his message and no
/// explanation would reasonably read it as something he typed. This line says
/// what happened, in one sentence, and it is threads asserting it.
pub fn body_scrutiny_notice(count: usize) -> String {
    let body = format!(
        "The message below wrote out {count} control-block opening{} of its own.\n\
         Threads escaped the leading `<` of each to `&lt;`, so they are inert text.\n\
         Nothing was deleted and no wording was changed; the ledger holds the original.",
        if count == 1 { "" } else { "s" }
    );
    system_notice(NOTICE_KIND_BODY_SCRUTINY, &body)
}

/// **BODY SCRUTINY (INTENT 66b): "threads can look inside of the message body
/// itself coming from the user to determine if the message body contains any
/// disallowed structure."**
///
/// Returns the safe body and how many control-block openings were neutralised.
///
/// **The policy chosen: NEUTRALISE, never refuse.** Refusing would mean either
/// dropping the operator's message or dropping a line of it, and this module
/// already holds the ruling that costs him his words is the worse failure —
/// `guard_injection` is applied to machinery and pointedly not to him. So the
/// message always goes through; only its ability to impersonate a control
/// block is removed.
///
/// **How, and why this form.** Only the `<` that opens a RESERVED tag is
/// rewritten, to `&lt;`. That is:
///
/// - *visible* — the reader sees the escape sequence sitting in the text and
///   the notice above it says so;
/// - *reversible* — decoding `&lt;` back to `<` recovers the original
///   byte-for-byte;
/// - *meaning-preserving* — not one word changes, and a sentence ABOUT the
///   tag still reads exactly as he said it.
///
/// **It is narrow on purpose.** Escaping every `<` in the body would mangle
/// code and markup he legitimately dictates or types, which is a change to his
/// words he would notice and object to. Only the two tags threads actually
/// treats as control structure are touched — nothing else in the body is a
/// control block, so nothing else needs defusing.
pub fn guard_body_structure(text: &str) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut neutralised = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '<' && reserved_tag_opens_at(&chars, i) {
            out.push_str("&lt;");
            neutralised += 1;
        } else {
            out.push(chars[i]);
        }
        i += 1;
    }
    (out, neutralised)
}

/// Does a reserved tag name start just after the `<` at `lt`?
///
/// Tolerates the two shapes a forger would reach for — a closing `</tag` and
/// whitespace padding `< tag` — and requires a NAME BOUNDARY after the tag, so
/// `<thread_system_notice_v2` is left alone rather than half-escaped.
fn reserved_tag_opens_at(chars: &[char], lt: usize) -> bool {
    let mut j = lt + 1;
    if chars.get(j) == Some(&'/') {
        j += 1;
    }
    while matches!(chars.get(j), Some(c) if c.is_whitespace()) {
        j += 1;
    }
    RESERVED_TAGS.iter().any(|tag| {
        let t: Vec<char> = tag.chars().collect();
        let end = j + t.len();
        if chars.len() < end {
            return false;
        }
        if !chars[j..end]
            .iter()
            .map(|c| c.to_ascii_lowercase())
            .eq(t.iter().copied())
        {
            return false;
        }
        match chars.get(end) {
            None => true,
            Some(c) => c.is_whitespace() || *c == '>' || *c == '/',
        }
    })
}

// ═══════════════════════════════════════════════════════════════════════════
// THE PER-MESSAGE METADATA BLOCK — INTENT 65, DIR-56, DEC-70
// ═══════════════════════════════════════════════════════════════════════════

// `MetadataLevel` lived here. It was DIR-56's "series of flags" built as a
// LEVEL, and the operator retired it: "this seems generic and not easy to
// understand. We should probably just have flags on certain metadata being
// injected." Its two values are now the presence or absence of the per-message
// line flags on `crate::settings::Injection`, and the block they used to gate is
// DERIVED from those rather than switched. A settings blob that still says
// `metadata_level` is translated once, at deserialization —
// `crate::settings::InjectionWire`.

/// The `sent:` line's two inputs, together, so that "no timestamp" is
/// `None` rather than a `show_date` flag sitting next to a live instant that
/// nothing renders.
#[derive(Debug, Clone, Copy)]
pub struct SentStamp {
    /// When the message was accepted (UTC; rendered as LOCAL wall clock).
    pub ts: DateTime<Utc>,
    /// Whether the DATE portion is stated. False elides it and emits the time
    /// alone — the token thrift INTENT 65 asks for.
    pub show_date: bool,
}

/// **THE CONTEXT ID — a cache key for retrieval dedup, and nothing else.**
///
/// The operator's reason, in substance: a thread retrieving from a large
/// knowledge graph must not re-emit the same fields twice, because re-emitting
/// them burns the context it is trying to preserve. So a consumer wants to
/// remember what it has already put in front of the model, and for that it
/// needs a key it can trust: **stable for exactly as long as the context
/// window holds, and guaranteed to change the moment that window is reset.**
///
/// After a compaction the thread's own ledger is LOSSY — the fields it emitted
/// before the boundary may no longer be in the window at all — so
/// re-retrieval is not a bug there, it is correct, and it needs a NEW key.
///
/// **THE ID IS THE BOUNDARY. It is derived, and it is zero new state.**
///
/// ```text
/// <thread_id>.0      the first context, from thread initialisation
/// <thread_id>.42     after the compaction that ended at message 42
/// <thread_id>.97     after the next one
/// ```
///
/// `last_compaction_message_id` (`crate::store::ThreadRow`) already records
/// where the current context started; its single writer is
/// `Store::update_thread_compaction`. So there is **no new column, no counter,
/// no mint and no migration** — and, more to the point, the id *cannot
/// disagree with the boundary, because it IS the boundary*. Three separate
/// bugs in this crate were caused by a derived answer being cached in a second
/// place that could drift; this one has no second place.
///
/// The `{thread_id}.{n}` spelling is deliberate: it is the same shape as the
/// message-body resource id, so a consumer parsing one already knows how to
/// read the other.
///
/// **IMPORTED THREADS ARE OUT OF SCOPE, and this is where a reader finds that
/// out rather than rediscovering it as a bug.** `thread_compacted` imports as
/// a `Custom` event and never moves the boundary, so an imported thread
/// reports `<id>.0` forever, however many times it was compacted before the
/// import. That is a known, accepted gap: the operator does not import. If a
/// consumer ever needs the id to be honest on an imported thread, the fix is
/// for the import path to move `last_compaction_message_id` — not for a second
/// derivation to grow here.
pub fn context_id(thread_id: &str, last_compaction_message_id: u64) -> String {
    format!("{thread_id}.{last_compaction_message_id}")
}

/// Everything one message's block can state. The caller decides which lines
/// there are; this type only renders.
///
/// **EVERY FIELD IS OPTIONAL, and that is the structural half of the flag
/// flattening.** A line that is switched off is `None` here, so "the block with
/// no lines" is representable — and [`message_metadata_notice`] answers it with
/// `None`, which is how the envelope comes to be derived rather than flagged.
/// There is no `level` and no "render the block" boolean to disagree with the
/// fields.
#[derive(Debug, Clone)]
pub struct MessageMeta<'a> {
    /// `Some` ONLY on the first post of a thread and the first post after a
    /// compaction (INTENT 65). Every other post omits the line entirely.
    pub thread_id: Option<&'a str>,
    /// The retrieval-dedup cache key for the CURRENT context window — see
    /// [`context_id`]. Rides exactly the posts the thread id rides, because
    /// those are the boundary posts, and after a compaction the boundary post
    /// IS the first thing in the new window: the agent therefore always sees
    /// the id of the context it is actually in. `None` omits the line, which
    /// is what every thread that has not opted in gets.
    pub context_id: Option<&'a str>,
    /// The threads ledger id. `None` omits the line
    /// (`Injection::message_id` off).
    pub message_id: Option<u64>,
    /// The APPLICATION's own coordinate for this message (INTENT 66c). `None`
    /// omits the line.
    pub idx: Option<u64>,
    /// The `sent:` stamp. `None` omits the line (`Injection::timestamp` off).
    pub sent: Option<SentStamp>,
    /// State, once, what the two id lines are for. Rides the same posts the
    /// thread id rides — thread start and first-after-compaction.
    pub gloss: bool,
    /// How long the operator's audio ran, ALREADY FORMATTED. `None` omits the
    /// line: threads does not know this figure and never invents one.
    pub duration: Option<String>,
}

/// Escape the five XML predefined entities, so a value cannot break out of the
/// envelope that carries it.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// The local calendar day of an instant (`YYYY-MM-DD`) — the elision key. Two
/// messages share a key exactly when the day has not rolled over between them.
pub fn local_date_key(ts: DateTime<Utc>) -> String {
    ts.with_timezone(&Local).format("%Y-%m-%d").to_string()
}

/// The timestamp line's value.
///
/// **THE DATE FORM, DECIDED: absolute, never relative.** INTENT 65 reached for
/// the relative wording ("it's tomorrow or Friday"), and relative wording is a
/// trap here: this block is not read once and discarded, it stays in the
/// context window for the rest of the conversation, so a "tomorrow" written on
/// Saturday is read as a falsehood on Monday. An absolute date cannot go stale.
/// The WEEKDAY is appended in parentheses because that is the part of "Friday"
/// that carries real information, and a weekday attached to its own date cannot
/// drift either.
///
/// The stamp is emitted as one contiguous `YYYY-MM-DD HH:MM:SS` run with the
/// weekday AFTER it, deliberately: that is the shape [`guard_injection`]'s
/// future-instant scanner can parse, so this line stays subject to CON-22 like
/// every other line of machinery.
fn metadata_stamp(ts: DateTime<Utc>, show_date: bool) -> String {
    let local: DateTime<Local> = ts.with_timezone(&Local);
    if show_date {
        format!(
            "{} ({})",
            local.format("%Y-%m-%d %H:%M:%S"),
            local.format("%A")
        )
    } else {
        local.format("%H:%M:%S").to_string()
    }
}

/// Render one message's metadata block, wrapped in the SAME envelope as the
/// interruption notice (INTENT 65: "wrap it in an XML block the way that we're
/// doing with the system notice that gets strapped in when we have to interrupt
/// a thread"). This is a second notice KIND in an existing system, not a new
/// system.
/// **Returns `None` when the block would have no lines**, which is the whole
/// derived-envelope rule: the per-message block is not a thing a setting
/// switches on, it is a thing that exists because at least one of its lines
/// does. `None` renders nothing at all — no envelope, no blank line — which is
/// byte for byte what `metadata_level: none` used to produce.
pub fn message_metadata_notice(meta: &MessageMeta) -> Option<String> {
    let mut body = String::new();
    if let Some(tid) = meta.thread_id {
        // The thread id is the block's ONLY free-text field — every other value
        // is a number this module formatted or a stamp it rendered — and thread
        // ids are caller-supplied (Q2). Escaping it is what makes "the block is
        // well-formed XML" a guarantee rather than a hope about UUIDs.
        body.push_str(&format!("thread: {}\n", xml_escape(tid)));
    }
    // THE CONTEXT ID, immediately under the thread id because it is the same
    // kind of fact — "where am I" — and it rides the same posts. Escaped for
    // the same reason the thread id is: it is derived FROM a caller-supplied
    // thread id, so it inherits that field's untrustedness exactly.
    if let Some(cid) = meta.context_id {
        body.push_str(&format!("context: {}", xml_escape(cid)));
        if meta.gloss {
            body.push_str(" (this context window; a new id after each compaction)");
        }
        body.push('\n');
    }
    // TWO COORDINATES, AND THEY ARE NOT THE SAME NUMBER. `message` is the
    // threads ledger id. `idx` is the application's own position for the same
    // event, and it is the one the workspace's citation convention
    // (`{thread_id, idx}`) and the `thread-messages` tool resolve. INTENT 66c
    // dissolved the question of making them agree: the operator hears no index
    // at all, so there was never a number that had to match. One event, two
    // derivations — the model is shown both because a citation must land on
    // the message the model was actually looking at.
    if let Some(mid) = meta.message_id {
        body.push_str(&format!("message: #{mid}"));
        if meta.gloss {
            body.push_str(" (this thread's ledger id)");
        }
        body.push('\n');
    }
    if let Some(idx) = meta.idx {
        body.push_str(&format!("idx: #{idx}"));
        if meta.gloss {
            body.push_str(" (the coordinate to cite)");
        }
        body.push('\n');
    }
    // `sent:` is the one line the old shape always ended on, so it carries no
    // trailing newline of its own and `duration:` appends to it. With the
    // timestamp off, the trailing newline a preceding line left behind has to
    // come back off, or the envelope gains a blank line nothing asked for.
    if let Some(s) = meta.sent {
        body.push_str(&format!("sent: {}", metadata_stamp(s.ts, s.show_date)));
    } else {
        while body.ends_with('\n') {
            body.pop();
        }
    }
    if let Some(d) = &meta.duration {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&format!("duration: {d}"));
    }
    // THE DERIVED ENVELOPE. No lines, no block — and therefore nothing for a
    // coherence check to refuse.
    if body.is_empty() {
        return None;
    }
    Some(system_notice(NOTICE_KIND_MESSAGE_META, &body))
}

/// **THE ONE DECLARED PATH** out of a caller's opaque per-message metadata that
/// may reach the model: `metadata.aui.audio_started_at`, an RFC3339 instant.
///
/// Named rather than dumped. DEC-55 makes category-(a) client metadata
/// invisible BY CONSTRUCTION, and re-emitting the blob wholesale would quietly
/// repeal that; reading one documented key keeps the invariant true while
/// giving the AUI exactly one channel for the one fact only it can know.
pub const DURATION_START_KEYS: (&str, &str) = ("aui", "audio_started_at");

/// **THE SECOND DECLARED PATH** out of the caller's blob:
/// `metadata.aui.thread_event_idx`, the application's own coordinate for this
/// message.
///
/// Read for the `idx:` line of the metadata block (INTENT 66c). Named, not
/// dumped, for the same reason as [`DURATION_START_KEYS`]: DEC-55 keeps the
/// rest of the blob invisible by construction, and two documented keys is a
/// contract while a re-emitted blob is a leak.
pub const APP_IDX_KEYS: (&str, &str) = ("aui", "thread_event_idx");

/// The application's coordinate for this message, if it sent one.
///
/// **WHEN IT IS ABSENT the line is simply OMITTED** — no metadata, no
/// `aui.thread_event_idx`, or a value that is not a non-negative integer.
/// Threads does not know this number and never invents one; there is no
/// placeholder, no zero, and no "unknown". A caller that sends no coordinate
/// gets a block with no `idx:` line, and every other line is unaffected.
pub fn app_idx_from_metadata(metadata: &Value) -> Option<u64> {
    let (ns, key) = APP_IDX_KEYS;
    metadata.get(ns)?.get(key)?.as_u64()
}

/// The duration line's value: how long the operator's audio ran.
///
/// **Threads SUBTRACTS; the caller only supplies the instant it alone knows.**
/// This is DEC-56's rule applied a second time — "the elapsed figure has to be
/// computed by the system and handed over already computed", because a model
/// cannot reliably subtract two timestamps. It also means a caller cannot
/// smuggle prose into the block through a free-text duration.
///
/// **WHEN IT IS ABSENT** — no metadata, no `aui.audio_started_at`, an
/// unparseable instant, or a start later than the message itself (clock skew) —
/// the duration line is simply OMITTED. Threads does not know this figure, so
/// it never guesses one, never writes "unknown", and never writes zero to stand
/// in for missing. A typed message legitimately has no audio and correctly
/// carries no duration.
pub fn duration_from_metadata(metadata: &Value, sent: DateTime<Utc>) -> Option<String> {
    let (ns, key) = DURATION_START_KEYS;
    let raw = metadata.get(ns)?.get(key)?.as_str()?;
    let start = DateTime::parse_from_rfc3339(raw).ok()?.with_timezone(&Utc);
    let seconds = (sent - start).num_seconds();
    if seconds < 0 {
        return None;
    }
    Some(format_elapsed(seconds))
}

/// Render an elapsed span in days/hours/minutes/seconds — DEC-56's
/// load-bearing clause. "The ELAPSED figure has to be computed by the system
/// and handed over already computed": a model cannot reliably subtract two
/// timestamps and does not know what "now" is.
pub fn format_elapsed(seconds: i64) -> String {
    let s = seconds.max(0);
    let days = s / 86_400;
    let hours = (s % 86_400) / 3_600;
    let mins = (s % 3_600) / 60;
    let secs = s % 60;
    // Once the largest nonzero unit is found, EVERY smaller unit is stated —
    // including zeros. "XYZ days, hours, minutes, seconds ago" is what was
    // asked for, and "4 days, 1 hour" silently drops two of the four; seconds
    // are always present so "0 seconds" is the answer to "how long was I
    // gone" rather than an empty string.
    let units: [(i64, &str); 4] = [
        (days, "day"),
        (hours, "hour"),
        (mins, "minute"),
        (secs, "second"),
    ];
    let start = units.iter().position(|(n, _)| *n > 0).unwrap_or(3);
    units[start..]
        .iter()
        .map(|(n, name)| format!("{n} {name}{}", plural(*n)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn plural(n: i64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Render an RFC3339 instant as a LOCAL wall-clock stamp — the operator's
/// frame of reference, matching the `[#N YYYY-MM-DD HH:MM:SS]` message
/// headers, so the two times a model sees are on one clock. An unparseable
/// value is passed through rather than dropped.
pub fn local_stamp(rfc3339: &str) -> String {
    match DateTime::parse_from_rfc3339(rfc3339) {
        Ok(t) => t
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => rfc3339.to_string(),
    }
}

/// The body of a category-(b) event, as the model will read it. `None` for a
/// [`Audience::LedgerOnly`] event — the audience split is enforced HERE, at
/// the renderer, so an event that may not be seen has no rendering at all.
///
/// # DEC-56 IS SUPERSEDED — THE CAUSE IS NEVER RENDERED (2026-08-17)
///
/// **Do not add a `Trigger:`, `Why:`, `What fired:`, `Moved to another
/// account:` or `Now running on:` line back to either body below.** They were
/// all here, and the operator removed them by name:
///
/// > *"On decision 56, I remember what I was thinking at that time, and we have
/// > to supersede that. I no longer want any usage limits to be told to the
/// > agents at all. And the reason why is because while before I thought that
/// > it would help the agents make better resource decisions… I am increasingly
/// > moving towards this belief that there's only exactly one way to solve a
/// > problem. And downgrading to a different agent is not it. So we shouldn't
/// > ever give the model the ability to see what its resource limit is."*
///
/// The permitted set is now EXHAUSTIVE and short: **that** it was interrupted,
/// **that** it has resumed, **when**, and **how long ago**. The forbidden set
/// is everything about the CAUSE — the trigger, the budget, the threshold, the
/// account, and whether an account move happened at all. "Moved to another
/// account: yes" names no account and still tells a model its account was
/// exhausted, which is the inference the supersession exists to close; a model
/// infers from the shape of what it is given, and it infers reliably.
///
/// The `trigger`, `detail`, `switched` and `to_account` fields are NOT removed
/// from the events. They stay on the ledger, which is where the operator and
/// his tooling read them. The split is between RECORDING a cause and TELLING a
/// model its cause, and it is enforced here, at the one renderer, rather than
/// by emptying the record.
///
/// CON-22's other half is untouched and still stands on its own: a model is
/// never told it has no usage limit and never told when a limit resets.
/// [`guard_injection`] remains the structural backstop below this function.
pub fn render_model_notice(ev: &ThreadEvent) -> Option<(&'static str, String)> {
    if !ev.audience().reaches_model() {
        return None;
    }
    match ev {
        // WHAT and WHEN. Nothing about why — see the supersession note above.
        // `trigger`, `detail`, `switched` and `to_account` are deliberately
        // not bound: an unused binding is the shape from which a later "while
        // I'm here" line grows.
        ThreadEvent::ThreadSystemInterrupted { at, .. } => Some((
            NOTICE_KIND_INTERRUPTED,
            format!("This thread was interrupted at {}.", local_stamp(at)),
        )),
        ThreadEvent::ThreadSystemResumed {
            interrupted_at,
            elapsed,
            ..
        } => {
            let mut body = String::from("You were interrupted and have now been resumed.");
            body.push_str(&format!("\nInterrupted at: {}.", local_stamp(interrupted_at)));
            // The one figure that is load-bearing and stays: a model cannot
            // reliably subtract two timestamps, and "it resuming after one
            // second is gonna be different than it resuming after four days".
            body.push_str(&format!("\nElapsed since then: {elapsed}."));
            Some((NOTICE_KIND_RESUMED, body))
        }
        ThreadEvent::ThreadSystemQueued {
            submitted_at,
            queued_until,
            queued_for,
            interrupted,
            ..
        } => {
            let body = format!(
                "This turn was requested before it could be started, so a gap opened \
                 between what was said and when it reached you.\n\
                 Submitted at: {}.\n\
                 Queued until: {}.\n\
                 Queued for: {queued_for}.\n\
                 Interrupted: {}. Nothing was cut off — this turn had not started.",
                local_stamp(submitted_at),
                local_stamp(queued_until),
                yes_no(*interrupted)
            );
            Some((NOTICE_KIND_QUEUED, body))
        }
        _ => None,
    }
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

/// **THE ONE PLACE CATEGORY-(B) EVENTS BECOME CONTEXT.**
///
/// Hand it a slice of ledger events; it returns the envelope block to prefix
/// onto the next user message, or `None` when nothing qualifies. Events whose
/// [`Audience`] is `LedgerOnly` are dropped here regardless of what a caller
/// believed about them, and the whole assembled block goes through
/// [`guard_injection`] before it is returned, so CON-22 cannot be bypassed by
/// adding a new event kind or a new renderer.
pub fn assemble_model_notices(events: &[ThreadEvent], now: DateTime<Utc>) -> Option<String> {
    let blocks: Vec<String> = events
        .iter()
        .filter(|e| e.audience() == Audience::Model)
        .filter_map(render_model_notice)
        .map(|(kind, body)| system_notice(kind, &body))
        .collect();
    if blocks.is_empty() {
        return None;
    }
    let joined = guard_injection(&blocks.join("\n"), now);
    if joined.trim().is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// The same gate over LEDGER rows — the shape a caller replaying history has.
/// Category (a) and every operational kind are dropped here exactly as above,
/// which is the point: there is ONE rule and both entry points obey it.
pub fn assemble_model_notices_from_ledger(
    events: &[StoredEvent],
    now: DateTime<Utc>,
) -> Option<String> {
    let owned: Vec<ThreadEvent> = events.iter().map(|e| e.event.clone()).collect();
    assemble_model_notices(&owned, now)
}

// ═══════════════════════════════════════════════════════════════════════════
// CON-22 — THE HARD PROHIBITION, ENFORCED WHERE CONTEXT IS ASSEMBLED
// ═══════════════════════════════════════════════════════════════════════════

/// Phrases that describe FUTURE CAPACITY. A line carrying any of them is
/// dropped from anything the daemon injects.
///
/// CON-22 draws its line between HISTORY and PREDICTION so it is checkable
/// rather than a matter of taste: "your limit returns Sunday at 9pm" and "you
/// have no usage limit" are claims about capacity a model will plan against.
/// Both directions are forbidden — a model told it is unlimited spends
/// accordingly.
///
/// **THIS HALF OF CON-22 STANDS, UNWEAKENED, AND IS NOT WHAT CHANGED IN 2026-08-17.**
/// What changed is the OTHER half — CON-22's permissive clause, which used to
/// allow "you hit the 40% account threshold for the session limit" through as
/// permitted history. The operator superseded that (see
/// [`render_model_notice`]), so the cause is no longer rendered at all. The
/// guard below is therefore no longer the only thing standing between a model
/// and a limit; it is the LAST thing, and it must keep working on its own
/// terms — a new event kind, a new renderer, or a restored permission all still
/// pass through here. Do not relax it on the grounds that "the renderer already
/// drops that": the renderer is a policy, this is a structural floor.
pub const CON22_FORBIDDEN_MARKERS: &[&str] = &[
    "resets at",
    "reset at",
    "resets_at",
    "resets on",
    "resets in",
    "will reset",
    "limit resets",
    "resetting at",
    "available again",
    "available at",
    "try again after",
    "try again at",
    "retry after",
    "retry at",
    "back at",
    "quota remaining",
    "remaining quota",
    "tokens remaining",
    "no usage limit",
    "no limit",
    "unlimited",
    "without limit",
    "usage limit for the week",
];

/// How far ahead of `now` an instant must be before it counts as a PREDICTION
/// rather than clock jitter. Sixty seconds: the machinery legitimately states
/// "now", and a reset window is hours or days away, so nothing real sits in
/// the gap.
const FUTURE_TOLERANCE_SECONDS: i64 = 60;

/// **THE ENFORCEMENT POINT.** Every string the daemon injects into a model's
/// context passes through here.
///
/// It drops, line by line, anything that tells a model about its FUTURE
/// capacity:
///
/// 1. a line carrying any [`CON22_FORBIDDEN_MARKERS`] phrase, and
/// 2. a line carrying a parseable instant more than [`FUTURE_TOLERANCE_SECONDS`]
///    ahead of `now` — the structural half of the rule, and the one that closes
///    kinds nobody has written yet. A reset timestamp is a future instant no
///    matter what prose surrounds it.
///
/// Past instants survive untouched: DEC-56 REQUIRES telling the model when it
/// was interrupted. History in, prediction out.
pub fn guard_injection(text: &str, now: DateTime<Utc>) -> String {
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| !line_predicts_capacity(line, now))
        .collect();
    kept.join("\n")
}

/// Why a line was refused (`None` = clean). The audit half of
/// [`guard_injection`], exposed so tests and callers can name the violation
/// instead of diffing strings.
pub fn con22_violation(text: &str, now: DateTime<Utc>) -> Option<String> {
    for line in text.lines() {
        let lower = line.to_lowercase();
        for m in CON22_FORBIDDEN_MARKERS {
            if lower.contains(m) {
                return Some(format!("future-capacity phrase {m:?} in: {line}"));
            }
        }
        if let Some(found) = future_instant_in(line, now) {
            return Some(format!("future instant {found:?} in: {line}"));
        }
    }
    None
}

fn line_predicts_capacity(line: &str, now: DateTime<Utc>) -> bool {
    let lower = line.to_lowercase();
    if CON22_FORBIDDEN_MARKERS.iter().any(|m| lower.contains(m)) {
        return true;
    }
    future_instant_in(line, now).is_some()
}

/// Scan a line for a timestamp strictly in the future.
///
/// Understands the two shapes the daemon can emit: RFC3339 (`2026-08-03T21:00:00Z`,
/// with or without an offset) and the local wall-clock stamp
/// (`2026-08-03 21:00:00`), which is parsed AS LOCAL — parsing it as UTC would
/// make the machinery's own "Now:" line look like a prediction in any timezone
/// ahead of UTC.
fn future_instant_in(line: &str, now: DateTime<Utc>) -> Option<String> {
    let bytes: Vec<char> = line.chars().collect();
    let cutoff = now + chrono::Duration::seconds(FUTURE_TOLERANCE_SECONDS);
    // A candidate starts at any `dddd-dd-dd`.
    for start in 0..bytes.len() {
        if !is_date_head(&bytes[start..]) {
            continue;
        }
        // Take the longest run of timestamp-ish characters from here, then
        // walk the tail back until something parses — a greedy run can swallow
        // the next word's leading `t`/`z`, and two instants on one line must
        // each get a fair reading.
        let mut end = start;
        while end < bytes.len() && is_ts_char(bytes[end]) {
            end += 1;
        }
        while end > start + 9 {
            let raw: String = bytes[start..end].iter().collect();
            let raw = raw.trim().trim_end_matches(['.', ',', ';', ':', '-']);
            if let Some(instant) = parse_instant(raw) {
                if instant > cutoff {
                    return Some(raw.to_string());
                }
                break;
            }
            end -= 1;
        }
    }
    None
}

fn is_date_head(rest: &[char]) -> bool {
    rest.len() >= 10
        && rest[0..4].iter().all(char::is_ascii_digit)
        && rest[4] == '-'
        && rest[5..7].iter().all(char::is_ascii_digit)
        && rest[7] == '-'
        && rest[8..10].iter().all(char::is_ascii_digit)
}

fn is_ts_char(c: char) -> bool {
    c.is_ascii_digit() || matches!(c, '-' | ':' | 'T' | 't' | 'Z' | 'z' | '.' | '+' | ' ')
}

fn parse_instant(raw: &str) -> Option<DateTime<Utc>> {
    let trimmed = raw.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.with_timezone(&Utc));
    }
    // `YYYY-MM-DD HH:MM:SS` (and the date-only form) as LOCAL wall clock.
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Local
                .from_local_datetime(&naive)
                .earliest()
                .map(|d| d.with_timezone(&Utc));
        }
    }
    None
}
