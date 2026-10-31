//! **THE API-ERROR SCRUB, against the real leak.**
//!
//! On 2026-08-14 a thread hit a weekly usage limit, failed over to another
//! account, read its own limit history back off its transcript, told three
//! replacement subagents the work had been "cut off by an account limit", and
//! three days later told the operator an experiment was confounded because
//! *"we'd hit a weekly account limit and six agents had just died mid-task, so
//! I optimized for spend without asking you"* — having silently downgraded
//! itself to a cheaper model to conserve his quota.
//!
//! The specimen of that thread is on the operator's machine at
//! `lib/threads/accounts/owner-account/projects/-Users-testuser-code-broomstick/966825e8-1381-49f1-99b3-3068a5a8641e.jsonl`:
//! 887 lines, six `tool_result` records at lines 876–879/883/884 and the
//! synthetic 429 at line 886.
//!
//! **That file is gitignored** (`accounts/` is excluded so a gigabyte of the
//! operator's private conversations never lands in git), so it cannot be the
//! only thing these tests stand on. The split is therefore deliberate:
//!
//! * [`the_checked_in_specimen_is_fully_scrubbed`] runs everywhere, against
//!   `tests/fixtures/limit_leak_specimen.jsonl` — the seven real records from
//!   the specimen, whose error-carrying text is BYTE-IDENTICAL to the original
//!   (only the operator's private subagent task prompts were removed), plus
//!   one record for each remaining shape and three records that must NOT be
//!   touched.
//! * [`the_real_specimen_transcript_is_scrubbed_end_to_end`] runs against the
//!   real 887-line file when it is present, and says so loudly when it is not.
//!
//! Nothing here ever writes to the specimen: every test copies it into a
//! tempdir first.

use std::path::{Path, PathBuf};
use threads::scrub;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture() -> PathBuf {
    crate_dir().join("tests/fixtures/limit_leak_specimen.jsonl")
}

/// The real specimen, if this machine has it.
fn real_specimen() -> Option<PathBuf> {
    let p = crate_dir()
        .join("accounts/owner-account/projects")
        .join("-Users-testuser-code-broomstick")
        .join("966825e8-1381-49f1-99b3-3068a5a8641e.jsonl");
    p.is_file().then_some(p)
}

/// Copy `src` into a fresh tempdir and hand back (dir, path). The tempdir is
/// returned so the caller keeps it alive.
fn staged(src: &Path) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("966825e8-1381-49f1-99b3-3068a5a8641e.jsonl");
    std::fs::copy(src, &dst).unwrap();
    (dir, dst)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// The exact prose the six subagent failures carried. If this string ever
/// survives a scrub, the 2026-08-14 leak is reproducible.
const LEAKED_WEEKLY: &str = "You've hit your weekly limit · resets 1pm (America/Chicago)";

// ═══════════════════════════════════════════════════════════════════════════
// 1. THE SPECIMEN
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn the_checked_in_specimen_is_fully_scrubbed() {
    let (_dir, path) = staged(&fixture());
    let before = read(&path);
    let before_lines = before.lines().count();

    let out = scrub::scrub_transcript(&path).unwrap();
    let after = read(&path);

    // ── line count, exactly ──────────────────────────────────────────────
    assert_eq!(
        after.lines().count(),
        before_lines,
        "the scrub is line-in/line-out; it may never add or drop a record"
    );
    assert_eq!(out.lines, before_lines);
    assert!(out.rewritten);

    // ── the six real tool_results ────────────────────────────────────────
    let marker = "Agent terminated early due to an API error:";
    assert_eq!(
        before.matches(LEAKED_WEEKLY).count(),
        13,
        "the fixture must carry the real prose TWICE per failed subagent — the \
         `tool_result` block and its `toolUseResult` mirror, which is a second, \
         separately-reachable copy on the same record — plus once on the \
         synthetic 429. Six subagents: 12 + 1."
    );
    // Every line the scrub could READ must be clean. The one line it could not
    // parse is deliberately excluded here and asserted byte-identical below:
    // "never corrupt what you do not understand" and "leave nothing behind" are
    // in genuine tension on a corrupt record, and this crate resolves it in
    // favour of not corrupting. Say so rather than hide it behind a loose match.
    let parseable: String = after
        .lines()
        .filter(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !parseable.contains(LEAKED_WEEKLY),
        "THE LEAK SURVIVED THE SCRUB:\n{parseable}"
    );
    assert!(
        !parseable.contains("weekly limit") && !parseable.contains("session limit"),
        "no limit prose of any shape may survive on a record the scrub can read"
    );
    assert!(
        after.contains(marker),
        "the PREFIX stays — the model is still told an API error happened"
    );
    assert!(
        after.contains("Agent terminated early due to an API error: <scrubbed>"),
        "prefix, colon, then the lowercase placeholder and nothing else"
    );

    // ── the partial-output remainder is PRESERVED ────────────────────────
    assert!(
        after.contains(
            "<scrubbed>\\n\\nEverything below is PARTIAL output recovered from the agent \
             before it was cut off."
        ),
        "the Agent tool's partial-output wrapper must survive intact: {after}"
    );

    // ── every other content block of those records is untouched ──────────
    assert!(after.contains("Now the Showdown controller"), "sibling text block");
    assert!(after.contains("subagent_tokens: 258002"), "the usage block");
    assert!(after.contains("a80fd4e9b609e03a4"), "the agent id");

    // ── the synthetic 429 ────────────────────────────────────────────────
    let synthetic: Vec<serde_json::Value> = after
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("isApiErrorMessage").and_then(|b| b.as_bool()) == Some(true))
        .collect();
    assert_eq!(synthetic.len(), 1, "the fixture holds exactly one");
    let s = &synthetic[0];
    assert_eq!(s["message"]["content"][0]["text"], "<scrubbed>");
    // …and its STRUCTURAL fields survive, because `isApiErrorMessage` is what
    // Claude Code's own normalizer keys on to drop the record before the API
    // call. Clearing it would push the record back INTO the request.
    assert_eq!(s["error"], "rate_limit");
    assert_eq!(s["isApiErrorMessage"], true);
    assert_eq!(s["apiErrorStatus"], 429);
    assert_eq!(s["message"]["model"], "<synthetic>");

    // ── shape 3: the task notification, in all three carriers ────────────
    assert!(!after.contains("2:30pm"), "queue-operation summary");
    assert!(!after.contains("safeguards flagged"), "multi-line detail in a summary");
    assert!(!after.contains("req_011Cdx7StGzdino2AxEW9Vb3"), "…including its request id");
    assert!(after.contains("<note>keep me</note>"), "the note after the summary survives");
    assert!(!after.contains("Aug 8 at 1pm"), "attachment.prompt");
    assert!(
        after.contains("/private/tmp/keep-this-path.output"),
        "an attachment listing keeps everything after the entry's own line"
    );

    // ── shape 4: the Workflow tool's <failures> block ────────────────────
    assert!(!after.contains("7:20am"), "the failures block");
    assert!(
        after.contains("[stub:org] failed: <scrubbed>"),
        "per-entry, inside the block"
    );
    assert!(
        after.contains("[note] failed: keep this line, it is outside the block"),
        "a `] failed:` OUTSIDE the block is ordinary prose and is not touched"
    );

    // ── what must NOT change ─────────────────────────────────────────────
    assert!(
        after.contains("grep -c 'Agent terminated early due to an API error: You.*weekly' t.jsonl"),
        "a stored Bash command is a record of what was RUN; rewriting it would \
         make the transcript lie"
    );
    let malformed = "{\"type\":\"user\",\"message\":{\"content\":[{\"text\":\"Agent terminated \
                     early due to an API error: You've hit your weekly limit\"";
    assert!(
        after.contains(malformed),
        "a line that does not parse is written back BYTE-IDENTICAL, leak and all — \
         we do not repair what we cannot read"
    );

    // ── the chain ────────────────────────────────────────────────────────
    assert_chain_preserved(&before, &after);
}

/// `uuid` / `parentUuid` / `timestamp` / `sessionId` on every parseable line,
/// in order, must be identical before and after. This is the resume chain and
/// the leaf pointer `--resume` follows.
fn assert_chain_preserved(before: &str, after: &str) {
    let chain = |s: &str| -> Vec<String> {
        s.lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .map(|v| {
                format!(
                    "{:?}|{:?}|{:?}|{:?}",
                    v.get("uuid"),
                    v.get("parentUuid"),
                    v.get("timestamp"),
                    v.get("sessionId")
                )
            })
            .collect()
    };
    assert_eq!(
        chain(before),
        chain(after),
        "the parent-uuid chain and the resume leaf pointer must be untouched"
    );
}

/// The real thing, when this machine has it. 887 lines; six `tool_result`
/// records; one synthetic 429.
#[test]
fn the_real_specimen_transcript_is_scrubbed_end_to_end() {
    let Some(src) = real_specimen() else {
        // Not a silent skip: the file is gitignored by design, and the
        // always-on guard above carries the contract.
        eprintln!(
            "SKIPPED: the real specimen is not on this machine (it is gitignored). \
             `the_checked_in_specimen_is_fully_scrubbed` covers the same contract."
        );
        return;
    };
    let (_dir, path) = staged(&src);
    let before = read(&path);
    assert_eq!(before.lines().count(), 887, "the specimen is 887 lines");

    let out = scrub::scrub_transcript(&path).unwrap();
    let after = read(&path);

    assert_eq!(after.lines().count(), 887, "line count preserved exactly");
    assert_eq!(out.lines, 887);
    assert!(out.records_changed >= 7, "six tool_results + the synthetic 429");

    // The six failure records, at their known line numbers.
    for n in [876usize, 877, 878, 879, 883, 884] {
        let line = after.lines().nth(n - 1).unwrap();
        assert!(
            line.contains("Agent terminated early due to an API error: <scrubbed>"),
            "line {n} was not scrubbed"
        );
        assert!(!line.contains("weekly limit"), "line {n} still leaks");
    }
    // The synthetic 429.
    let line886: serde_json::Value = serde_json::from_str(after.lines().nth(885).unwrap()).unwrap();
    assert_eq!(line886["isApiErrorMessage"], true);
    assert_eq!(line886["message"]["content"][0]["text"], "<scrubbed>");

    assert!(
        !after.contains(LEAKED_WEEKLY),
        "the exact string that leaked on 2026-08-14 must not survive anywhere in the file"
    );
    assert_chain_preserved(&before, &after);
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. IDEMPOTENCE — a second pass is a no-op with ZERO writes
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn the_second_pass_changes_nothing_and_writes_nothing() {
    let (_dir, path) = staged(&fixture());

    let first = scrub::scrub_transcript(&path).unwrap();
    assert!(first.rewritten && first.records_changed > 0);
    let after_first = read(&path);
    let mtime_first = std::fs::metadata(&path).unwrap().modified().unwrap();

    // Enough of a gap that a rewrite would be visible in the mtime on any
    // filesystem this runs on.
    std::thread::sleep(std::time::Duration::from_millis(50));

    let second = scrub::scrub_transcript(&path).unwrap();
    assert_eq!(second.records_changed, 0, "nothing left to scrub");
    assert!(!second.rewritten, "ZERO WRITES on an already-scrubbed transcript");
    assert_eq!(second.lines, first.lines);
    assert_eq!(read(&path), after_first, "byte-identical");
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        mtime_first,
        "the file was not touched at all"
    );

    // …and no temp file was left behind by either pass.
    let litter: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".scrub-"))
        .collect();
    assert!(litter.is_empty(), "temp files left behind: {litter:?}");
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. NEVER CORRUPT WHAT IT DOES NOT UNDERSTAND
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn a_line_that_cannot_be_parsed_is_left_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");

    // Three lines that all trip the prefilter and none of which is valid JSON,
    // around one that is. Only the valid one may change.
    let truncated = "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\
                     \"content\":[{\"type\":\"text\",\"text\":\"Agent terminated early due to an \
                     API error: You've hit your weekly limit";
    let not_json = "Agent terminated early due to an API error: You've hit your weekly limit";
    let bad_escape = "{\"type\":\"user\",\"x\":\"Agent terminated early due to an API error: \\q\"}";
    let good = "{\"type\":\"user\",\"x\":\"Agent terminated early due to an API error: \
                You've hit your session limit\"}";

    let original = format!("{truncated}\n{not_json}\n{bad_escape}\n{good}\n");
    std::fs::write(&path, &original).unwrap();

    let out = scrub::scrub_transcript(&path).unwrap();
    assert_eq!(out.lines, 4);
    assert_eq!(out.records_changed, 1, "only the parseable line changed");

    let after = read(&path);
    let lines: Vec<&str> = after.lines().collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0], truncated, "a truncated record is untouched");
    assert_eq!(lines[1], not_json, "a non-JSON line is untouched");
    assert_eq!(lines[2], bad_escape, "an invalid escape is untouched");
    assert!(
        lines[3].contains("Agent terminated early due to an API error: <scrubbed>"),
        "…and the one line it DID understand was scrubbed: {}",
        lines[3]
    );
}

#[test]
fn a_transcript_with_no_api_error_is_never_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let original = "{\"type\":\"user\",\"message\":{\"content\":\"hello\"}}\n\
                    {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\
                    \"text\":\"hi\"}]}}\n";
    std::fs::write(&path, original).unwrap();
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    let out = scrub::scrub_transcript(&path).unwrap();
    assert_eq!(out.records_changed, 0);
    assert!(!out.rewritten);
    assert_eq!(read(&path), original, "byte-for-byte identical");
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before);
}

/// A transcript that does not end in a newline must not grow one — the last
/// line could be a record still being written.
#[test]
fn a_missing_trailing_newline_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let original = "{\"type\":\"user\",\"x\":\"Agent terminated early due to an API error: \
                    boom\"}";
    std::fs::write(&path, original).unwrap();
    scrub::scrub_transcript(&path).unwrap();
    let after = read(&path);
    assert!(!after.ends_with('\n'), "no newline was invented: {after:?}");
    assert!(after.contains("<scrubbed>"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. THE SIDECAR — subagent transcripts carry the same string
// ═══════════════════════════════════════════════════════════════════════════

/// `scrub_session` is the spawn-path entry point. It must reach
/// `<session>/subagents/**.jsonl` as well as `<session>.jsonl`: those files
/// carry the identical prose, they are what a subagent resumes from, and
/// `accounts::port_sidecar` carries them onto the new account on failover.
#[test]
fn the_sidecar_subagent_transcripts_are_scrubbed_too() {
    let home = tempfile::tempdir().unwrap();
    let session = "966825e8-1381-49f1-99b3-3068a5a8641e";
    let projects = home.path().join(".claude/projects/-Users-testuser-code-broomstick");
    let sub = projects.join(session).join("subagents");
    std::fs::create_dir_all(sub.join("workflows/wf_1")).unwrap();

    let main = projects.join(format!("{session}.jsonl"));
    std::fs::copy(fixture(), &main).unwrap();

    let leak = "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\
                \"content\":[{\"type\":\"text\",\"text\":\"Agent terminated early due to an API \
                error: You've hit your weekly limit · resets 1pm (America/Chicago)\\n\\n\
                Everything below is PARTIAL output.\"}]}]}}\n";
    let a = sub.join("agent-a1.jsonl");
    let nested = sub.join("workflows/wf_1/journal.jsonl");
    std::fs::write(&a, leak).unwrap();
    std::fs::write(&nested, leak).unwrap();
    // A non-transcript file in the same tree must be ignored.
    std::fs::write(sub.join("notes.txt"), leak).unwrap();

    let out = scrub::scrub_session(None, home.path(), session).unwrap();
    assert_eq!(out.files_scanned, 3, "main + two sidecar transcripts");
    assert_eq!(out.files_rewritten, 3);

    for p in [&main, &a, &nested] {
        let s = read(p);
        assert!(!s.contains(LEAKED_WEEKLY), "{} still leaks", p.display());
        assert!(s.contains("<scrubbed>"), "{} was not scrubbed", p.display());
    }
    assert!(
        read(&sub.join("notes.txt")).contains(LEAKED_WEEKLY),
        "only .jsonl transcripts are in scope"
    );

    // Idempotent across the whole session, not just per file.
    let again = scrub::scrub_session(None, home.path(), session).unwrap();
    assert_eq!(again.records_changed, 0);
    assert_eq!(again.files_rewritten, 0);
}

/// A session with no transcript at all (the first turn of a brand-new thread)
/// is a clean no-op, not an error — the spawn path calls this unconditionally.
#[test]
fn a_session_with_no_transcript_is_a_no_op() {
    let home = tempfile::tempdir().unwrap();
    let out = scrub::scrub_session(None, home.path(), "no-such-session").unwrap();
    assert_eq!(out.files_scanned, 0);
    assert_eq!(out.records_changed, 0);
}
