//! **THE TRANSCRIPT SCRUB — API-error DETAILS never reach a resumed model.**
//!
//! # Why this module exists (read this before changing anything below)
//!
//! On 2026-08-14 a thread hit a weekly usage limit, failed over to another
//! account, and then read its own limit history back off its transcript. It
//! told three replacement subagents the work had been "cut off by an account
//! limit", and three days later told the operator that an experiment was
//! confounded because *"we'd hit a weekly account limit and six agents had just
//! died mid-task, so I optimized for spend without asking you"* — it had
//! silently downgraded itself to a cheaper model to conserve the operator's
//! quota. Nobody asked it to. Nobody could see that it had.
//!
//! **The leak path was proven empirically, and it is NOT the one everybody
//! assumed.** The synthetic assistant record (`"model":"<synthetic>"`,
//! `isApiErrorMessage:true`, `"error":"rate_limit"`) is dropped by Claude
//! Code's own request normalizer before the API call — verified against the
//! shipped 2.1.220 binary. It never reaches the model *in context*. It does
//! remain on disk, where any tool the model runs (`grep`, `Read`) will find it.
//!
//! What actually reaches the model in-band is the SUBAGENT FAILURE RECORD: a
//! plain `type:"user"` message carrying a `tool_result` content block whose
//! text opens
//!
//! ```text
//! Agent terminated early due to an API error: You've hit your weekly limit · resets 1pm (America/Chicago)
//!
//! Everything below is PARTIAL output recovered from the agent before it was cut off. …
//! ```
//!
//! Those records are `isSidechain:false`, carry no exclusion marker, and are
//! STRUCTURALLY UNDROPPABLE — they pair with `tool_use` blocks the assistant
//! already emitted, so removing them would leave a dangling tool call. They are
//! also ported byte-identically on account failover, which is how a limit
//! followed a thread onto a healthy account.
//!
//! # The operator's spec, which this module implements literally
//!
//! > *"we can have a flag that's set on a per thread basis. And when this flag
//! > is on, we scrub usage limits. So what we do is before ever starting an
//! > agent backup, regardless of if we just switched accounts or not, we go
//! > into the data structure and where it says API error hit usage limit, we
//! > take that whole sentence out and just call it API error. We don't tell it
//! > what the API error was at all. And we can just do that for all API errors
//! > just so it's general, so it's not trying to infer because it'll do it,
//! > it'll infer. So we'll use the flag like scrub API error details. We'll
//! > default it to true. … we're just going to replace the entire details
//! > string like the less than symbol, all caps, scrubbed, and then the greater
//! > than symbol. Or don't even make it all caps because we don't want it to
//! > think about how important it is that it was scrubbed. Make it lowercase.
//! > So just the lowercase word scrubbed."*
//!
//! Three clauses of that are load-bearing and none of them is negotiable:
//!
//! 1. **ALL API errors, not only usage limits.** A model infers from the SHAPE
//!    of what is missing. If only usage limits were blanked, a blank would mean
//!    "usage limit" and the scrub would be a louder signal than the prose.
//! 2. **Lowercase `<scrubbed>`.** Not `<SCRUBBED>`, not `[redacted]`. The
//!    replacement must read as boring machinery, because a model that thinks
//!    the redaction is important will reason about the redaction.
//! 3. **Before EVERY spawn**, whether or not an account switch just happened.
//!    The transcript is a permanent record; a limit hit six turns ago is still
//!    sitting there on turn seven.
//!
//! # What is scrubbed (the four shapes, surveyed against 2,425 real transcripts)
//!
//! Every shape below was found by walking every string of every record in
//! `lib/threads/accounts/*/projects/*/*.jsonl` and its `subagents/` sidecars.
//! The counts are from that survey and are recorded so a later reader can tell
//! a shape that vanished from a shape that was never covered.
//!
//! | # | producer | carrier | how it is cut |
//! |---|----------|---------|---------------|
//! | 1 | the Agent tool | `Agent terminated early due to an API error: <details>` in a `tool_result` text block (and its `toolUseResult` mirror) | keep the prefix through the colon; details → `<scrubbed>`; the `\n\nEverything below is PARTIAL output…` remainder is PRESERVED |
//! | 2 | Claude Code's API-error path | the synthetic `type:"assistant"` record with `isApiErrorMessage:true`, whose whole text IS the vendor error | the entire text → `<scrubbed>` |
//! | 3 | the task-notification path | `<summary>Agent "NAME" failed: Agent terminated early due to an API error: <details></summary>` inside a `queue-operation` record, a plain-string `user` message, or an `attachment` record's `prompt`/`content`/`stdout` | same cut as #1, terminated by `</summary>` |
//! | 4 | the Workflow tool | `<failures>[job] failed: <details>\n…</failures>` | details → `<scrubbed>`, per line, inside the `<failures>` block only |
//!
//! # What is deliberately NOT scrubbed, and why
//!
//! **The model's own prose and its own tool-call inputs.** A `type:"assistant"`
//! record that is not an API-error record holds words the MODEL wrote, and
//! `message.content[].input.command` holds a shell command it actually ran. The
//! survey found the marker string inside a stored `Bash` command (an agent
//! grepping for the leak) and inside `Edit` payloads (an agent writing this
//! very module's doc comment). Rewriting those would make the transcript lie
//! about what was executed, and it would corrupt code the model is mid-edit on.
//! *Correctness at the source*: once the four shapes above are scrubbed from
//! turn one, a model never has the input from which to write such prose.
//!
//! **Residual, stated plainly rather than papered over:** a transcript that
//! ALREADY contains the model's own prose about a limit — written before this
//! module existed — keeps it. This module fixes the producer; it is not a
//! retroactive memory edit, and turning it into one would mean rewriting
//! assistant records, which is a different and much larger decision than the
//! one the operator made.
//!
//! **The structural error fields** — `isApiErrorMessage`, `apiErrorStatus`,
//! `error:"rate_limit"` — are left alone on purpose. `isApiErrorMessage` is the
//! flag Claude Code's own normalizer keys on to DROP the record before the API
//! call. Clearing or renaming those fields would very plausibly stop the drop
//! and push the record back INTO the request, which is strictly worse than
//! leaving a machine-readable tag on a record whose human-readable text is
//! already `<scrubbed>`. If a future reader wants those fields gone, the
//! prerequisite is re-verifying the normalizer, not a one-line edit here.
//!
//! # The four safety properties, and how each is obtained
//!
//! * **Line count is preserved exactly**, and so are the `parentUuid` chain and
//!   the resume leaf pointer: this is a per-line transform that never adds,
//!   removes or reorders a line, and never touches a `uuid`/`parentUuid` field.
//! * **Idempotent.** Re-running on a scrubbed transcript finds the details
//!   already equal to `<scrubbed>`, changes nothing, and performs ZERO writes —
//!   the file is only rewritten when at least one line actually changed.
//! * **Crash-safe.** Write to a dot-prefixed temp in the SAME directory, fsync
//!   it, rename over the target, fsync the directory. This mirrors
//!   [`crate::accounts`]'s `port_into` step-for-step rather than inventing a
//!   second durability pattern; see the section banner there for the argument.
//! * **Cheap.** Every line is prefiltered with plain substring tests before any
//!   JSON parse. Transcripts run to megabytes and the overwhelming majority of
//!   lines match nothing.
//! * **Never corrupts what it does not understand.** A line that fails to parse,
//!   or that parses to a shape no rule matches, is written back BYTE-IDENTICAL
//!   — the original bytes, not a re-serialization.

use crate::error::{Error, Result};
use serde_json::Value;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// The replacement. Lowercase on the operator's explicit instruction: *"we
/// don't want it to think about how important it is that it was scrubbed."*
pub const SCRUBBED: &str = "<scrubbed>";

/// The Agent tool's prefix. Everything up to and including the colon is KEPT —
/// the model is still told an API error happened, it is just never told which.
const AGENT_MARKER: &str = "Agent terminated early due to an API error:";

/// The Agent tool's partial-output wrapper. Everything from here on belongs to
/// the subagent's recovered output, not to the error, and is preserved.
const PARTIAL_WRAPPER: &str = "\n\nEverything below is PARTIAL output";

/// The task-notification wrapper close.
const SUMMARY_OPEN: &str = "<summary>";
const SUMMARY_CLOSE: &str = "</summary>";

/// The Workflow tool's per-job failure block.
const FAILURES_OPEN: &str = "<failures>";
const FAILURES_CLOSE: &str = "</failures>";
/// Inside a `<failures>` block only: `[job-name] failed: <details>`.
const FAILED_MARKER: &str = "] failed:";

/// **THE PREFILTER.** No JSON is parsed unless one of these appears in the raw
/// line. Ordered cheapest-discriminating-first; `contains` on a &str is a
/// memchr-driven scan, so a non-matching megabyte line costs one pass.
fn may_carry_api_error(line: &str) -> bool {
    line.contains(AGENT_MARKER)
        || line.contains("isApiErrorMessage")
        || line.contains(FAILURES_OPEN)
}

/// What one scrub pass did. `records_changed == 0` means the file was NOT
/// written — that is the idempotency contract, asserted by
/// `tests/scrub_transcript.rs`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScrubOutcome {
    pub lines: usize,
    pub records_changed: usize,
    pub rewritten: bool,
}

/// Everything one session's scrub touched: the main transcript plus every
/// sidecar subagent transcript.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionScrub {
    pub files_scanned: usize,
    pub files_rewritten: usize,
    pub records_changed: usize,
}

// ═══════════════════════════════════════════════════════════════════════════
// THE STRING TRANSFORM
// ═══════════════════════════════════════════════════════════════════════════

/// Where the details after an `AGENT_MARKER` (or a `FAILED_MARKER`) END.
///
/// The rule has to survive four real terminator shapes seen in the survey, and
/// the precedence between them is the whole subtlety:
///
/// 1. `…API error: You've hit your weekly limit · resets 1pm\n\nEverything
///    below is PARTIAL output…` — the Agent tool's own wrapper. Cut there so
///    the recovered subagent output survives intact.
/// 2. `<summary>Agent "X" failed: …API error: You've hit your session limit ·
///    resets 2:30pm</summary>` — a task notification. Cut at `</summary>`.
/// 3. `<summary>… API error: API Error: Fable 5's safeguards flagged this
///    message (…). …\n\nTry rephrasing…\n\nRequest ID: req_…</summary>` — a
///    MULTI-LINE detail inside a summary. This is why `</summary>` must beat a
///    bare newline: cutting at the first `\n` would leave three more lines of
///    vendor prose behind.
/// 4. `- id=a164… · failed · Agent "X" failed: …API error: You've hit your
///    session limit ... · 2026-08-12T05:50:16.028Z\n      /private/tmp/…` — a
///    task-listing attachment. No wrapper at all; cut at end of line. This
///    eats a trailing ` · <timestamp>` along with the detail, which is
///    accepted: losing a timestamp from a notification listing is harmless,
///    where leaving the detail is the whole defect.
///
/// So: a WRAPPER terminator wins if one is present; otherwise the first
/// newline; otherwise end of string. `</summary>` only counts as a wrapper when
/// a `<summary>` actually opened before the marker — that keeps case 4 (which
/// has no summary tag) from grabbing a `</summary>` belonging to some later
/// entry in the same listing.
///
/// The bare-newline fallback also covers DOUBLY-ENCODED text (a JSON blob
/// stored inside a JSON string), where the newline arrives as the two literal
/// characters `\` `n` rather than as U+000A.
fn details_end(haystack: &str, details_start: usize, summary_opened_before: bool) -> usize {
    let rest = &haystack[details_start..];

    let mut wrapper: Option<usize> = rest.find(PARTIAL_WRAPPER);
    if summary_opened_before {
        if let Some(i) = rest.find(SUMMARY_CLOSE) {
            wrapper = Some(wrapper.map_or(i, |w| w.min(i)));
        }
    }
    if let Some(w) = wrapper {
        return details_start + w;
    }

    let nl = rest.find('\n');
    // The literal two-character `\n` of a nested JSON encoding.
    let esc_nl = rest.find("\\n");
    let cut = match (nl, esc_nl) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    details_start + cut.unwrap_or(rest.len())
}

/// Replace the details following every `Agent terminated early due to an API
/// error:` in `s`, and every `[job] failed:` inside a `<failures>` block.
///
/// Returns `None` when nothing changed — the caller uses that to keep the
/// original bytes and to skip the write.
fn scrub_error_prose(s: &str) -> Option<String> {
    // SECOND PREFILTER, at the string rather than the line. `walk_strings`
    // visits every string of a matching record — including 40 KB subagent
    // prompts — and without this each visit would clone one. Keep it in step
    // with `may_carry_api_error`.
    if !s.contains(AGENT_MARKER) && !s.contains(FAILURES_OPEN) {
        return None;
    }
    let mut out: Option<String> = None;
    let mut work = s.to_string();

    // ── shape 1 + 3: the Agent tool's marker, wherever it sits ───────────
    loop {
        // Re-scan from the top each pass: the string shortens as details are
        // replaced, so index bookkeeping across passes would be wrong. The
        // loop terminates because every pass either changes nothing (break) or
        // replaces a detail with `<scrubbed>`, which the "already scrubbed"
        // test in `find_unscrubbed_marker` then skips forever after.
        let Some((start, end)) = find_unscrubbed_marker(&work, AGENT_MARKER) else {
            break;
        };
        work = format!("{}{SCRUBBED}{}", &work[..start], &work[end..]);
        out = Some(work.clone());
    }

    // ── shape 4: the Workflow tool's `<failures>` block ──────────────────
    // Scoped to the block so a `] failed:` in ordinary prose is never touched.
    if let (Some(open), Some(close)) = (work.find(FAILURES_OPEN), work.find(FAILURES_CLOSE)) {
        if open < close {
            let block_start = open + FAILURES_OPEN.len();
            let block = &work[block_start..close];
            if let Some(new_block) = scrub_failures_block(block) {
                work = format!("{}{new_block}{}", &work[..block_start], &work[close..]);
                out = Some(work.clone());
            }
        }
    }

    out
}

/// Find the next `marker` in `s` whose details are NOT already `<scrubbed>`.
/// Returns the byte range of those details.
fn find_unscrubbed_marker(s: &str, marker: &str) -> Option<(usize, usize)> {
    let mut from = 0usize;
    while let Some(rel) = s[from..].find(marker) {
        let at = from + rel;
        let after_marker = at + marker.len();
        // Skip the single space the producer writes; keep any other run of
        // whitespace out of the details so the prefix renders identically.
        let details_start = if s[after_marker..].starts_with(' ') {
            after_marker + 1
        } else {
            after_marker
        };
        let opened = s[..at].rfind(SUMMARY_OPEN).is_some_and(|o| {
            // The open must not be closed again before the marker.
            !s[o..at].contains(SUMMARY_CLOSE)
        });
        let end = details_end(s, details_start, opened);
        if &s[details_start..end] != SCRUBBED {
            return Some((details_start, end));
        }
        from = end.max(at + marker.len());
        if from >= s.len() {
            break;
        }
    }
    None
}

/// `[stub:org] failed: You've hit your session limit · resets 7:20am` → the
/// same line with the detail replaced. One entry per line. `None` = unchanged.
fn scrub_failures_block(block: &str) -> Option<String> {
    let mut changed = false;
    let mut lines: Vec<String> = Vec::new();
    for line in block.split('\n') {
        match line.find(FAILED_MARKER) {
            Some(i) => {
                let after = i + FAILED_MARKER.len();
                let start = if line[after..].starts_with(' ') {
                    after + 1
                } else {
                    after
                };
                if &line[start..] == SCRUBBED {
                    lines.push(line.to_string());
                } else {
                    changed = true;
                    lines.push(format!("{}{SCRUBBED}", &line[..start]));
                }
            }
            None => lines.push(line.to_string()),
        }
    }
    changed.then(|| lines.join("\n"))
}

// ═══════════════════════════════════════════════════════════════════════════
// THE RECORD TRANSFORM
// ═══════════════════════════════════════════════════════════════════════════

/// Is this the synthetic API-error assistant record (shape 2)?
fn is_api_error_record(v: &Value) -> bool {
    v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
}

/// Apply the prose transform to every string reachable in `v`. Returns true if
/// anything changed.
fn walk_strings(v: &mut Value, changed: &mut bool) {
    match v {
        Value::String(s) => {
            if let Some(new) = scrub_error_prose(s) {
                *s = new;
                *changed = true;
            }
        }
        Value::Array(a) => {
            for item in a {
                walk_strings(item, changed);
            }
        }
        Value::Object(o) => {
            for (_, item) in o.iter_mut() {
                walk_strings(item, changed);
            }
        }
        _ => {}
    }
}

/// Blank the text of a synthetic API-error record ENTIRELY: unlike shape 1
/// there is no prefix to keep — the whole text IS the vendor's error string
/// (`"You've hit your session limit · resets 7:20am (America/Chicago)"`,
/// `"API Error: Unable to connect to API (ENOTFOUND)"`, `"Prompt is too long"`).
fn blank_api_error_text(v: &mut Value, changed: &mut bool) {
    let Some(content) = v
        .get_mut("message")
        .and_then(|m| m.get_mut("content"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        if let Some(Value::String(text)) = block.get_mut("text") {
            if text != SCRUBBED {
                *text = SCRUBBED.to_string();
                *changed = true;
            }
        }
    }
}

/// Transform ONE parsed record. `None` = nothing to do (the caller then keeps
/// the original bytes verbatim).
///
/// The `assistant` split is the judgment call this module is most likely to be
/// second-guessed on, so it is stated here as well as in the module header: an
/// assistant record is rewritten ONLY when it is the synthetic API-error
/// record. Every other assistant record holds the model's own words and its own
/// tool-call inputs — including live `Bash` commands and `Edit` payloads — and
/// rewriting those would make the transcript misreport what was executed.
pub fn scrub_record(v: &mut Value) -> bool {
    let mut changed = false;
    let is_assistant = v.get("type").and_then(Value::as_str) == Some("assistant");
    if is_assistant {
        if !is_api_error_record(v) {
            return false;
        }
        blank_api_error_text(v, &mut changed);
        return changed;
    }
    walk_strings(v, &mut changed);
    changed
}

/// Scrub ONE transcript line. `None` = leave the original bytes alone.
///
/// A line that does not parse as JSON returns `None` — never a repair attempt,
/// never a partial write. A half-written tail line (Claude Code appends while
/// we read) is exactly that case, and the correct answer is to leave it.
///
/// **Re-serialization, and why it is acceptable on the lines that change.**
/// A changed line is rebuilt with `serde_json::to_string`, which sorts object
/// keys (the crate does not enable `preserve_order`) and normalizes escapes.
/// Claude Code reads these lines with `JSON.parse`, so key order carries no
/// meaning; every VALUE — `uuid`, `parentUuid`, `timestamp`, `sessionId`, the
/// whole resume chain — round-trips exactly. Unchanged lines are never
/// re-serialized at all, so a transcript with no API error in it is left
/// byte-for-byte identical.
pub fn scrub_line(line: &str) -> Option<String> {
    if !may_carry_api_error(line) {
        return None;
    }
    let mut v: Value = serde_json::from_str(line).ok()?;
    if !scrub_record(&mut v) {
        return None;
    }
    serde_json::to_string(&v).ok()
}

// ═══════════════════════════════════════════════════════════════════════════
// THE FILE TRANSFORM — crash-safe, and a NO-OP when nothing changed
// ═══════════════════════════════════════════════════════════════════════════

/// Scrub one `.jsonl` transcript in place.
///
/// Copy-verify-then-activate, the same shape as
/// `crate::accounts::port_into`: build the new content in a dot-prefixed temp
/// in the SAME directory (so the rename is atomic), fsync it, rename it over
/// the target, fsync the directory. A crash at any point leaves either the old
/// complete file or the new complete file, never a half-scrubbed one.
///
/// **Zero writes when nothing changed.** The whole file is transformed in
/// memory first; if no record changed, the function returns without creating a
/// temp file at all. That is what makes a second run — and the third, and the
/// pre-spawn run on every subsequent turn — free.
pub fn scrub_transcript(path: &Path) -> Result<ScrubOutcome> {
    let file = std::fs::File::open(path)
        .map_err(|e| Error::Config(format!("reading {}: {e}", path.display())))?;
    let mut out = ScrubOutcome::default();
    // Line-in / line-out, so the count is preserved by construction.
    let mut lines: Vec<String> = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        let line = line.map_err(|e| Error::Config(format!("reading {}: {e}", path.display())))?;
        out.lines += 1;
        match scrub_line(&line) {
            Some(new) => {
                out.records_changed += 1;
                lines.push(new);
            }
            None => lines.push(line),
        }
    }
    if out.records_changed == 0 {
        return Ok(out);
    }

    // PRESERVE THE TRAILING-NEWLINE PROPERTY EXACTLY. `lines()` cannot tell
    // "…}\n" from "…}", and inventing a trailing newline is not cosmetic: a
    // transcript whose last line is a record Claude Code is still writing would
    // have that record split in two by the next append. We only ever run before
    // a spawn, when the file is quiescent, so this should never fire — which is
    // exactly why it must not silently do the wrong thing when it does.
    let ends_with_newline = last_byte_is_newline(path).unwrap_or(true);

    let dir = path
        .parent()
        .ok_or_else(|| Error::Config(format!("{} has no parent directory", path.display())))?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "transcript".to_string());
    let temp_prefix = format!(".{stem}.jsonl.scrub-");
    // Sweep temps a crashed earlier scrub left behind. Inert (dot-prefixed,
    // not `.jsonl`), and swept exactly when a scrub is proven to run again.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&temp_prefix) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let temp = dir.join(format!(
        "{temp_prefix}{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));

    let write = (|| -> std::io::Result<()> {
        let f = std::fs::File::create(&temp)?;
        let mut w = std::io::BufWriter::new(f);
        let last = lines.len().saturating_sub(1);
        for (i, line) in lines.iter().enumerate() {
            w.write_all(line.as_bytes())?;
            if i < last || ends_with_newline {
                w.write_all(b"\n")?;
            }
        }
        w.flush()?;
        w.into_inner()
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .sync_all()
    })();
    if let Err(e) = write {
        let _ = std::fs::remove_file(&temp);
        return Err(Error::Config(format!(
            "writing the scrubbed copy of {} to {}: {e} — the original is untouched",
            path.display(),
            temp.display()
        )));
    }

    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        Error::Config(format!(
            "activating the scrubbed {} (the original is untouched): {e}",
            path.display()
        ))
    })?;
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    out.rewritten = true;
    Ok(out)
}

/// **THE SPAWN-PATH ENTRY POINT.** Scrub the transcript a child is about to
/// `--resume` from, and every SIDECAR subagent transcript beside it.
///
/// The sidecar matters as much as the main file: `<session>/subagents/*.jsonl`
/// carries the identical string, it is what a subagent resumes from, and it is
/// readable by any tool the model calls. `crate::accounts::port_sidecar`
/// already moves the directory on failover, so an unscrubbed sidecar would
/// simply follow the thread onto its new account.
///
/// Failure is REPORTED, never fatal: a transcript that could not be scrubbed
/// must not prevent the operator's turn from running. The caller logs it.
pub fn scrub_session(
    config_dir: Option<&str>,
    home: &Path,
    session: &str,
) -> Result<SessionScrub> {
    let Some(transcript) = crate::accounts::locate_transcript(config_dir, home, session) else {
        return Ok(SessionScrub::default());
    };
    let mut total = SessionScrub::default();
    let mut fold = |outcome: ScrubOutcome| {
        total.files_scanned += 1;
        total.records_changed += outcome.records_changed;
        if outcome.rewritten {
            total.files_rewritten += 1;
        }
    };
    fold(scrub_transcript(&transcript)?);

    for path in sidecar_transcripts(&transcript) {
        match scrub_transcript(&path) {
            Ok(o) => fold(o),
            // One unreadable sidecar must not silence the rest of the sweep —
            // the same rule `port_sidecar` follows for the same reason.
            Err(e) => tracing::warn!(
                session, path = %path.display(), error = %e,
                "a sidecar subagent transcript could not be scrubbed"
            ),
        }
    }
    Ok(total)
}

/// Does the file's last byte end a line? `None` when it cannot be read (the
/// caller then assumes yes, which is what Claude Code always writes).
fn last_byte_is_newline(path: &Path) -> Option<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len == 0 {
        return Some(true);
    }
    f.seek(SeekFrom::Start(len - 1)).ok()?;
    let mut b = [0u8; 1];
    f.read_exact(&mut b).ok()?;
    Some(b[0] == b'\n')
}

/// Every `.jsonl` under `<session>/subagents/`, recursively — Claude Code nests
/// workflow runs (`subagents/workflows/<run>/journal.jsonl`) under the same
/// root.
fn sidecar_transcripts(transcript: &Path) -> Vec<PathBuf> {
    let (Some(parent), Some(stem)) = (transcript.parent(), transcript.file_stem()) else {
        return Vec::new();
    };
    let root = parent.join(stem).join("subagents");
    let mut out = Vec::new();
    collect_jsonl(&root, &mut out);
    out.sort();
    out
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_jsonl(&path, out),
            Ok(t) if t.is_file() => {
                if path.extension().is_some_and(|e| e == "jsonl") {
                    out.push(path);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_tool_prefix_survives_and_the_detail_does_not() {
        let s = "Agent terminated early due to an API error: You've hit your weekly limit · \
                 resets 1pm (America/Chicago)\n\nEverything below is PARTIAL output recovered \
                 from the agent before it was cut off.";
        let got = scrub_error_prose(s).expect("changed");
        assert_eq!(
            got,
            "Agent terminated early due to an API error: <scrubbed>\n\nEverything below is \
             PARTIAL output recovered from the agent before it was cut off."
        );
    }

    #[test]
    fn a_summary_terminated_detail_cuts_at_the_summary_close() {
        let s = "<status>failed</status>\n<summary>Agent \"X\" failed: Agent terminated early \
                 due to an API error: You've hit your session limit · resets 2:30pm \
                 (America/Chicago)</summary>\n<note>keep me</note>";
        let got = scrub_error_prose(s).expect("changed");
        assert!(got.contains("<summary>Agent \"X\" failed: Agent terminated early due to an API \
                              error: <scrubbed></summary>"));
        assert!(got.contains("<note>keep me</note>"));
        assert!(!got.contains("2:30pm"));
    }

    #[test]
    fn a_multiline_detail_inside_a_summary_is_taken_whole() {
        let s = "<summary>Agent \"X\" failed: Agent terminated early due to an API error: API \
                 Error: safeguards flagged this message.\n\nTry rephrasing.\n\nRequest ID: \
                 req_1</summary>";
        let got = scrub_error_prose(s).expect("changed");
        assert!(!got.contains("Try rephrasing"), "the whole detail goes: {got}");
        assert!(!got.contains("req_1"));
        assert!(got.ends_with("<scrubbed></summary>"));
    }

    #[test]
    fn a_listing_entry_with_no_wrapper_cuts_at_end_of_line() {
        let s = "  - id=a1 · failed · Agent \"Y\" failed: Agent terminated early due to an API \
                 error: You've hit your session limit ... · 2026-08-12T05:50:16.028Z\n      \
                 /private/tmp/keep-me.output";
        let got = scrub_error_prose(s).expect("changed");
        assert!(got.contains("/private/tmp/keep-me.output"), "{got}");
        assert!(!got.contains("session limit"));
    }

    #[test]
    fn the_failures_block_is_scrubbed_per_line_and_only_inside_the_block() {
        let s = "<diagnostics>[note] failed: keep this, it is not in the block</diagnostics>\n\
                 <failures>[stub:org] failed: You've hit your session limit · resets 7:20am\n\
                 [commit] failed: You've hit your session limit · resets 7:20am</failures>\n\
                 <usage>6</usage>";
        let got = scrub_error_prose(s).expect("changed");
        assert!(got.contains("[note] failed: keep this, it is not in the block"));
        assert!(got.contains("<failures>[stub:org] failed: <scrubbed>\n[commit] failed: <scrubbed></failures>"));
        assert!(got.contains("<usage>6</usage>"));
    }

    #[test]
    fn the_transform_is_idempotent_at_the_string_level() {
        let s = "Agent terminated early due to an API error: You've hit your weekly limit\n\n\
                 Everything below is PARTIAL output.";
        let once = scrub_error_prose(s).expect("changed");
        assert!(scrub_error_prose(&once).is_none(), "second pass must be a no-op");
    }

    #[test]
    fn several_markers_in_one_string_are_all_scrubbed() {
        let s = "a: Agent terminated early due to an API error: one\nb: Agent terminated early \
                 due to an API error: two\n";
        let got = scrub_error_prose(s).expect("changed");
        assert_eq!(got.matches(SCRUBBED).count(), 2, "{got}");
        assert!(!got.contains("one") && !got.contains("two"));
    }

    #[test]
    fn an_assistant_record_that_is_not_an_api_error_is_never_touched() {
        // A stored Bash command that quotes the marker. Rewriting it would make
        // the transcript lie about what was run.
        let raw = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","input":{"command":"grep 'Agent terminated early due to an API error: x'"}}]}}"#;
        assert_eq!(scrub_line(raw), None);
    }

    #[test]
    fn the_synthetic_record_keeps_its_structural_error_fields() {
        let raw = r#"{"type":"assistant","error":"rate_limit","isApiErrorMessage":true,"apiErrorStatus":429,"message":{"model":"<synthetic>","content":[{"type":"text","text":"You've hit your weekly limit · resets 1pm (America/Chicago)"}]}}"#;
        let out = scrub_line(raw).expect("changed");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"], "rate_limit");
        assert_eq!(v["isApiErrorMessage"], true);
        assert_eq!(v["apiErrorStatus"], 429);
        assert_eq!(v["message"]["content"][0]["text"], SCRUBBED);
        // …and a second pass does nothing.
        assert_eq!(scrub_line(&out), None);
    }

    #[test]
    fn a_line_with_no_api_error_never_reaches_the_parser() {
        assert_eq!(scrub_line("{not json at all"), None);
        assert_eq!(scrub_line(r#"{"type":"user","message":{"content":"hello"}}"#), None);
    }
}
