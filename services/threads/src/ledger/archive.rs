//! # The parser for the captured wire — and the census that checks the design against it
//!
//! `docs/proto-ledger.md` §6. Input: `turns/<thread>/<cookie>.jsonl`, one file
//! per daemon turn, the byte-exact stdout of `claude -p --output-format
//! stream-json`, written at `runtime.rs` ("Durable stdout file (G1)") and never
//! swept — 5,267 threads, 11,148 files, 3.1 GB on 2026-10-08.
//!
//! **Turn order comes from content, not names.** The cookie is a random v4, so
//! a file's position is the minimum `timestamp` among its lines that carry
//! one, falling back to mtime for a file with none (counted). Within a file,
//! line order. Lines without a timestamp (`system/*`, `result`) take the last
//! one seen in the file, so every event has a `ts` and the chain the renderer
//! emits is monotone.
//!
//! **The operator's prompt is not on the wire.** It is recovered from a native
//! copy where one exists ([`prompts_from_native`]) and merged by timestamp
//! under the runtime's own uuid; a thread with no native copy gets no prompt
//! events, and the renderer counts the gap (`prompt_text_unrecoverable` is
//! for a `user` event with null text; a thread with no prompt event at all
//! simply has none, and `Census::prompts` says how many were merged).
//!
//! **The census is the test.** Every line lands in exactly one bucket, the
//! buckets sum to the line count, and `malformed` is zero on the whole
//! corpus. Every `(type/subtype)` that landed as `native` is listed, so a new
//! Claude Code line type is seen the day it appears.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::claude_code::{self, Outcome, SUMMARY_PREFIX};
use super::event::{AppendRefused, Draft, Kind, Ledger, ThreadMeta};

/// What the parser counted. Sums: `lines == events_from_lines + ephemeral + replayed + malformed`,
/// where `events_from_lines` counts LINES that produced events (a line may
/// produce more than one event; `events` counts events).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Census {
    pub threads: usize,
    pub files: usize,
    pub lines: usize,
    /// Lines that became at least one event.
    pub event_lines: usize,
    /// Events, by kind word.
    pub events: BTreeMap<String, usize>,
    /// `type/subtype` labels that landed as `native` — the drift detector.
    pub native_labels: BTreeMap<String, usize>,
    /// Ephemera, by label.
    pub ephemeral: BTreeMap<String, usize>,
    /// Lines whose id was already in the thread.
    pub replayed: usize,
    /// Events whose line carried no `uuid` (expected 0).
    pub id_minted: usize,
    /// `(file, line number, reason)` — expected empty.
    pub malformed: Vec<(PathBuf, usize, String)>,
    /// Wire lines carrying more than one content block.
    pub multi_block_lines: usize,
    /// Files with no timestamped line, ordered by mtime instead.
    pub unordered_by_content: usize,
    /// Lines whose `session_id` is not the directory's thread id.
    pub session_mismatch: usize,
    /// Prompt events merged from native copies.
    pub prompts: usize,
    /// Prompts whose timestamp fell after every wire line (appended at the end).
    pub prompts_trailing: usize,
}

impl Census {
    pub fn merge(&mut self, o: &Census) {
        self.threads += o.threads;
        self.files += o.files;
        self.lines += o.lines;
        self.event_lines += o.event_lines;
        for (k, v) in &o.events {
            *self.events.entry(k.clone()).or_insert(0) += v;
        }
        for (k, v) in &o.native_labels {
            *self.native_labels.entry(k.clone()).or_insert(0) += v;
        }
        for (k, v) in &o.ephemeral {
            *self.ephemeral.entry(k.clone()).or_insert(0) += v;
        }
        self.replayed += o.replayed;
        self.id_minted += o.id_minted;
        self.malformed.extend(o.malformed.iter().cloned());
        self.multi_block_lines += o.multi_block_lines;
        self.unordered_by_content += o.unordered_by_content;
        self.session_mismatch += o.session_mismatch;
        self.prompts += o.prompts;
        self.prompts_trailing += o.prompts_trailing;
    }

    /// The accounting identity every census must satisfy.
    pub fn accounted(&self) -> bool {
        self.lines == self.event_lines + self.ephemeral.values().sum::<usize>() + self.replayed + self.malformed.len()
    }

    pub fn report(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "threads {}  files {}  lines {}  event_lines {}  events {}  replayed {}  id_minted {}  malformed {}  multi_block {}  unordered_by_content {}  session_mismatch {}  prompts {} (trailing {})\n",
            self.threads,
            self.files,
            self.lines,
            self.event_lines,
            self.events.values().sum::<usize>(),
            self.replayed,
            self.id_minted,
            self.malformed.len(),
            self.multi_block_lines,
            self.unordered_by_content,
            self.session_mismatch,
            self.prompts,
            self.prompts_trailing,
        ));
        s.push_str("events by kind:\n");
        for (k, v) in &self.events {
            s.push_str(&format!("  {v:>8}  {k}\n"));
        }
        s.push_str("ephemera:\n");
        for (k, v) in &self.ephemeral {
            s.push_str(&format!("  {v:>8}  {k}\n"));
        }
        s.push_str("landed as native (type/subtype):\n");
        for (k, v) in &self.native_labels {
            s.push_str(&format!("  {v:>8}  {k}\n"));
        }
        for (p, n, why) in self.malformed.iter().take(20) {
            s.push_str(&format!("MALFORMED {}:{n}: {why}\n", p.display()));
        }
        s
    }
}

/// A prompt recovered from a native copy: the runtime's uuid, its timestamp,
/// and the text as delivered.
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    pub uuid: String,
    pub ts: String,
    pub text: String,
}

/// **THE PROMPTS A NATIVE FILE HOLDS**: `user` lines that are not tool
/// results, not `isMeta`, and not a compaction summary. The one thing the
/// wire never carries.
pub fn prompts_from_native(path: &Path) -> std::io::Result<Vec<Prompt>> {
    let bytes = std::fs::read(path)?;
    let mut out = Vec::new();
    for raw in bytes.split(|b| *b == b'\n') {
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(raw) else { continue };
        if v.get("type").and_then(Value::as_str) != Some("user")
            || v.get("isMeta").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let text = match v.get("message").and_then(|m| m.get("content")) {
            Some(Value::String(t)) => t.clone(),
            Some(Value::Array(a)) => {
                if a.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")) {
                    continue;
                }
                a.iter()
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => continue,
        };
        if text.starts_with(SUMMARY_PREFIX) {
            continue;
        }
        let (Some(uuid), Some(ts)) = (
            v.get("uuid").and_then(Value::as_str),
            v.get("timestamp").and_then(Value::as_str),
        ) else {
            continue;
        };
        out.push(Prompt {
            uuid: uuid.to_string(),
            ts: ts.to_string(),
            text,
        });
    }
    out.sort_by(|a, b| a.ts.cmp(&b.ts));
    Ok(out)
}

fn first_timestamp(path: &Path) -> std::io::Result<Option<String>> {
    let bytes = std::fs::read(path)?;
    let mut min: Option<String> = None;
    for raw in bytes.split(|b| *b == b'\n') {
        // Cheap scan: only lines that mention a timestamp are parsed.
        if !raw.windows(11).any(|w| w == b"\"timestamp\"") {
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(raw) {
            if let Some(t) = v.get("timestamp").and_then(Value::as_str) {
                if min.as_deref().map(|m| t < m).unwrap_or(true) {
                    min = Some(t.to_string());
                }
            }
        }
    }
    Ok(min)
}

/// The turn files of one thread directory, in content order. The second
/// element is `false` where mtime decided.
pub fn turn_files(thread_dir: &Path) -> std::io::Result<Vec<(PathBuf, bool)>> {
    let mut files: Vec<(PathBuf, String, bool)> = Vec::new();
    for entry in std::fs::read_dir(thread_dir)? {
        let p = entry?.path();
        if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        match first_timestamp(&p)? {
            Some(t) => files.push((p, t, true)),
            None => {
                let m = std::fs::metadata(&p)?.modified()?;
                let secs = m.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                // An mtime key sorts after every RFC3339 key of the same era
                // only by accident; it is used only among files with no content
                // timestamp, which the census counts.
                files.push((p, format!("~mtime:{secs:020}"), false));
            }
        }
    }
    files.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    Ok(files.into_iter().map(|(p, _, by_content)| (p, by_content)).collect())
}

/// **INGEST ONE THREAD'S CAPTURED TURNS INTO A LEDGER**, merging `prompts` by
/// timestamp. The thread id is the directory name; the cookie is the file
/// stem.
pub fn ingest_thread(thread_dir: &Path, prompts: &[Prompt]) -> std::io::Result<(Ledger, Census)> {
    let thread_id = thread_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string();
    let files = turn_files(thread_dir)?;
    let mut census = Census {
        threads: 1,
        files: files.len(),
        ..Default::default()
    };
    let created_at = files
        .first()
        .and_then(|(p, _)| first_timestamp(p).ok().flatten())
        .unwrap_or_default();
    let mut ledger = Ledger::new(ThreadMeta::root(
        thread_id.clone(),
        claude_code::RUNTIME,
        created_at,
    ));
    let mut next_prompt = 0usize;
    let mut flush_prompts = |upto: &str, ledger: &mut Ledger, census: &mut Census, trailing: bool| {
        while next_prompt < prompts.len() && (trailing || prompts[next_prompt].ts.as_str() <= upto) {
            let p = &prompts[next_prompt];
            let d = Draft::new(Kind::User, p.ts.clone(), serde_json::json!({"text": p.text, "intent": "immediate"}))
                .with_id(p.uuid.clone());
            match ledger.append(d) {
                Ok(_) => {
                    census.prompts += 1;
                    if trailing {
                        census.prompts_trailing += 1;
                    }
                    *census.events.entry("user".into()).or_insert(0) += 1;
                }
                Err(AppendRefused::Replay { .. }) => census.replayed += 1,
                Err(AppendRefused::NoKind) => unreachable!(),
            }
            next_prompt += 1;
        }
    };

    for (path, by_content) in &files {
        if !by_content {
            census.unordered_by_content += 1;
        }
        let cookie = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let bytes = std::fs::read(path)?;
        let mut last_ts = String::new();
        for (i, raw) in bytes.split(|b| *b == b'\n').enumerate() {
            if raw.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            census.lines += 1;
            let v: Value = match serde_json::from_slice(raw) {
                Ok(v) => v,
                Err(e) => {
                    census.malformed.push((path.clone(), i + 1, e.to_string()));
                    continue;
                }
            };
            if let Some(sid) = v.get("session_id").and_then(Value::as_str) {
                if sid != thread_id {
                    census.session_mismatch += 1;
                }
            }
            if let Some(t) = v.get("timestamp").and_then(Value::as_str) {
                last_ts = t.to_string();
                flush_prompts(t, &mut ledger, &mut census, false);
            }
            match claude_code::translate(&v, Some(&cookie)) {
                Outcome::Malformed(why) => census.malformed.push((path.clone(), i + 1, why)),
                Outcome::Ephemeral(label) => *census.ephemeral.entry(label.to_string()).or_insert(0) += 1,
                Outcome::Events(drafts) => {
                    let n = drafts.len();
                    if n > 1 && drafts.iter().filter(|d| d.kind != Some(Kind::Usage)).count() > 1 {
                        census.multi_block_lines += 1;
                    }
                    let mut landed = 0;
                    let mut replay = false;
                    for mut d in drafts {
                        if d.ts.is_empty() {
                            d.ts = last_ts.clone();
                        }
                        let kind = d.kind.unwrap();
                        let minted = d.id.is_none();
                        match ledger.append(d) {
                            Ok(_) => {
                                landed += 1;
                                if minted {
                                    census.id_minted += 1;
                                }
                                *census.events.entry(kind.word().to_string()).or_insert(0) += 1;
                                if kind == Kind::Native {
                                    *census.native_labels.entry(claude_code::label(&v)).or_insert(0) += 1;
                                }
                            }
                            Err(AppendRefused::Replay { .. }) => replay = true,
                            Err(AppendRefused::NoKind) => unreachable!("translate always sets a kind"),
                        }
                    }
                    if landed > 0 {
                        census.event_lines += 1;
                    } else if replay {
                        census.replayed += 1;
                    }
                }
            }
        }
    }
    flush_prompts("", &mut ledger, &mut census, true);
    Ok((ledger, census))
}

/// **INGEST A WHOLE `turns/` ROOT**, each thread into its own ledger, calling
/// `each` with the result. `natives` answers a thread id with the path of a
/// native copy to recover prompts from, if any. Returns the merged census.
pub fn ingest_archive(
    turns_root: &Path,
    natives: impl Fn(&str) -> Option<PathBuf>,
    mut each: impl FnMut(&Ledger, &Census),
) -> std::io::Result<Census> {
    let mut total = Census::default();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(turns_root)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let id = dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let prompts = match natives(&id) {
            Some(p) => prompts_from_native(&p)?,
            None => Vec::new(),
        };
        let (ledger, census) = ingest_thread(&dir, &prompts)?;
        each(&ledger, &census);
        total.merge(&census);
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn write_lines(path: &Path, lines: &[Value]) {
        let mut f = std::fs::File::create(path).unwrap();
        for l in lines {
            serde_json::to_writer(&mut f, l).unwrap();
            f.write_all(b"\n").unwrap();
        }
    }

    /// A two-turn thread whose later turn has the alphabetically EARLIER
    /// cookie, so name order and content order disagree. Returns the turns
    /// root and the native copy.
    fn fixture(dir: &Path) -> (PathBuf, PathBuf) {
        let turns = dir.join("turns");
        let t = turns.join("thread-1");
        std::fs::create_dir_all(&t).unwrap();
        let asst = |uuid: &str, ts: &str, text: &str| {
            json!({"type": "assistant", "uuid": uuid, "timestamp": ts, "session_id": "thread-1", "parent_tool_use_id": null,
                "request_id": "req", "message": {"id": "msg", "model": "m", "content": [{"type": "text", "text": text}]}})
        };
        // Turn 2 (earlier cookie "a…", later content).
        write_lines(
            &t.join("aaaa.jsonl"),
            &[
                json!({"type": "system", "subtype": "init", "uuid": "i2", "session_id": "thread-1", "model": "m", "tools": ["Read"]}),
                json!({"type": "stream_event", "uuid": "s2", "session_id": "thread-1", "event": {"type": "message_start"}}),
                asst("a2", "2026-10-08T00:01:10Z", "second answer"),
                json!({"type": "system", "subtype": "vcs_state_changed", "uuid": "v2", "session_id": "thread-1"}),
                json!({"type": "result", "subtype": "success", "uuid": "r2", "session_id": "thread-1", "is_error": false, "usage": {"input_tokens": 1, "output_tokens": 1}, "modelUsage": {"m": {}}}),
            ],
        );
        // Turn 1 (later cookie "zzzz", earlier content), with a replayed line and one with no uuid.
        write_lines(
            &t.join("zzzz.jsonl"),
            &[
                json!({"type": "system", "subtype": "init", "uuid": "i1", "session_id": "thread-1", "model": "m", "tools": []}),
                json!({"type": "system", "subtype": "thinking_tokens", "uuid": "k1", "session_id": "thread-1"}),
                asst("a1", "2026-10-08T00:00:10Z", "first answer"),
                asst("a1", "2026-10-08T00:00:10Z", "first answer"),
                json!({"type": "user", "timestamp": "2026-10-08T00:00:11Z", "session_id": "other-thread",
                    "message": {"content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}]}}),
                json!({"type": "result", "subtype": "success", "uuid": "r1", "session_id": "thread-1", "is_error": false, "usage": {}, "modelUsage": {"m": {}}}),
            ],
        );
        // A third file with no timestamp at all: ordered by mtime, counted.
        write_lines(&t.join("mmmm.jsonl"), &[json!({"type": "system", "subtype": "init", "uuid": "i3", "session_id": "thread-1"})]);
        // The native copy, holding the two prompts the wire never carried, a meta line and a tool result.
        let native = dir.join("thread-1.jsonl");
        write_lines(
            &native,
            &[
                json!({"type": "user", "uuid": "p1", "timestamp": "2026-10-08T00:00:01Z", "message": {"role": "user", "content": "first prompt"}}),
                json!({"type": "user", "uuid": "m1", "timestamp": "2026-10-08T00:00:02Z", "isMeta": true, "message": {"role": "user", "content": [{"type": "text", "text": "skill"}]}}),
                json!({"type": "user", "uuid": "t1", "timestamp": "2026-10-08T00:00:11Z", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}]}}),
                json!({"type": "user", "uuid": "p2", "timestamp": "2026-10-08T00:01:01Z", "message": {"role": "user", "content": [{"type": "text", "text": "second prompt"}]}}),
                json!({"type": "user", "uuid": "p3", "timestamp": "2026-10-08T00:09:00Z", "message": {"role": "user", "content": "a prompt after every wire line"}}),
            ],
        );
        (turns, native)
    }

    /// **TURN ORDER COMES FROM CONTENT, PROMPTS MERGE BY TIMESTAMP, AND EVERY
    /// LINE IS ACCOUNTED.** The fixture's cookies sort against its timestamps;
    /// the ledger must come out in time order, with each prompt before the
    /// turn it started, the replay dropped, the uuid-less line minted, the
    /// unknown subtype listed under `native`, and the census summing to the
    /// line count.
    ///
    /// **THE MUTATION.** In `turn_files`, sort by path instead of key
    /// (`a.0.cmp(&b.0)` alone).
    ///
    /// Forced red:
    /// ```text
    /// turn order comes from content, not from the cookie
    /// ```
    #[test]
    fn turn_order_is_content_order_and_every_line_is_accounted() {
        let dir = tempfile::tempdir().unwrap();
        let (turns, native) = fixture(dir.path());
        let prompts = prompts_from_native(&native).unwrap();
        assert_eq!(
            prompts.iter().map(|p| p.uuid.as_str()).collect::<Vec<_>>(),
            vec!["p1", "p2", "p3"],
            "prompts are the user lines that are neither meta nor tool results"
        );
        let (ledger, census) = ingest_thread(&turns.join("thread-1"), &prompts).unwrap();
        let order: Vec<(&str, &str)> = ledger.events().iter().map(|e| (e.id.as_str(), e.kind.word())).collect();
        let answers: Vec<&str> = order.iter().filter(|(_, k)| *k == "assistant").map(|(id, _)| *id).collect();
        assert_eq!(answers, vec!["a1", "a2"], "turn order comes from content, not from the cookie");
        let p1 = order.iter().position(|(id, _)| *id == "p1").unwrap();
        let a1 = order.iter().position(|(id, _)| *id == "a1").unwrap();
        let p2 = order.iter().position(|(id, _)| *id == "p2").unwrap();
        let a2 = order.iter().position(|(id, _)| *id == "a2").unwrap();
        assert!(p1 < a1 && a1 < p2 && p2 < a2, "each prompt lands before the turn it started: {order:?}");
        assert_eq!(order.last().map(|(id, _)| *id), Some("p3"), "a trailing prompt lands last");
        assert_eq!(census.prompts, 3);
        assert_eq!(census.prompts_trailing, 1);
        assert_eq!(census.replayed, 1, "the repeated a1 line is a replay");
        assert_eq!(census.id_minted, 1, "the uuid-less tool_result line was minted an id");
        assert_eq!(census.session_mismatch, 1);
        assert_eq!(census.unordered_by_content, 1, "the one file with no timestamped line was ordered by mtime, and counted");
        assert_eq!(census.native_labels.get("system/vcs_state_changed"), Some(&1), "an unknown subtype is listed");
        assert_eq!(census.ephemeral.get("stream_event"), Some(&1));
        assert_eq!(census.ephemeral.get("system/thinking_tokens"), Some(&1));
        assert!(census.malformed.is_empty());
        assert_eq!(census.lines, 12);
        assert!(census.accounted(), "every line lands in exactly one bucket: {}", census.report());
        // Lines with no timestamp took the last one seen in their file.
        let r2 = ledger.events().iter().find(|e| e.id == "r2").unwrap();
        assert_eq!(r2.ts, "2026-10-08T00:01:10Z");
        assert_eq!(r2.turn.as_deref(), Some("aaaa"), "an event knows the cookie it was captured in");
        assert_eq!(ledger.thread_id(), "thread-1");
    }

    /// **A MALFORMED LINE IS COUNTED, NAMED BY FILE AND LINE, AND BREAKS THE
    /// ACCOUNTING ONLY IF IT IS NOT COUNTED.**
    ///
    /// **THE MUTATION.** In `ingest_thread`, on a JSON parse error `continue`
    /// without pushing to `census.malformed`.
    ///
    /// Forced red:
    /// ```text
    /// a malformed line is named by file and line
    /// ```
    #[test]
    fn a_malformed_line_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("turns").join("t");
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join("c.jsonl"), "{\"type\":\"system\",\"subtype\":\"init\",\"uuid\":\"i\"}\nnot json\n{\"uuid\":\"no-type\"}\n").unwrap();
        let (_, census) = ingest_thread(&t, &[]).unwrap();
        assert_eq!(census.malformed.len(), 2, "a malformed line is named by file and line");
        assert_eq!(census.malformed[0].1, 2);
        assert!(census.malformed[1].2.contains("no `type`"));
        assert!(census.accounted());
    }

    /// **THE REAL ARCHIVE, WHEN A COPY IS NAMED.** `PROTO_LEDGER_CORPUS` points
    /// at a directory holding `turns/` (a copy of `lib/threads/turns/`, never the
    /// live one) and optionally `native/<thread>.jsonl`. Prints the census and
    /// refuses a corpus with a malformed line, an unaccounted line, or a thread
    /// whose ledger does not round-trip through its file form.
    #[test]
    #[ignore = "opt-in: set PROTO_LEDGER_CORPUS to a COPY of the archive (never the live one) and run with --ignored"]
    fn the_real_archive_census_when_a_copy_is_named() {
        let Ok(root) = std::env::var("PROTO_LEDGER_CORPUS") else {
            panic!("PROTO_LEDGER_CORPUS not set; the real-archive census did not run — this test is opt-in and must not report ok having read nothing");
        };
        let root = PathBuf::from(root);
        let native_dir = root.join("native");
        let out = root.join("ledgers");
        std::fs::create_dir_all(&out).unwrap();
        let mut threads = 0;
        let mut largest = (0usize, String::new());
        let total = ingest_archive(
            &root.join("turns"),
            |id| {
                let p = native_dir.join(format!("{id}.jsonl"));
                p.exists().then_some(p)
            },
            |ledger, _| {
                threads += 1;
                let path = out.join(format!("{}.jsonl", ledger.thread_id()));
                ledger.write_jsonl(&path).unwrap();
                let back = Ledger::read_jsonl(&path).unwrap();
                assert_eq!(back.events().len(), ledger.len(), "{} round-trips through its file form", ledger.thread_id());
                if ledger.len() > largest.0 {
                    largest = (ledger.len(), ledger.thread_id().to_string());
                }
            },
        )
        .unwrap();
        eprintln!("{}", total.report());
        eprintln!("largest ledger: {} events ({})", largest.0, largest.1);
        assert_eq!(threads, total.threads);
        assert!(total.malformed.is_empty(), "malformed lines in the corpus: {:?}", &total.malformed[..total.malformed.len().min(5)]);
        assert!(total.accounted(), "every line lands in exactly one bucket");
    }

    /// **THE REAL ROUND-TRIP, WHEN A COPY IS NAMED.** For every corpus thread
    /// with a native copy: ingest the wire (prompts merged from the copy),
    /// render, read the render back, and compare with the copy the runtime
    /// wrote. Asserts: every render is a walkable chain; every non-conversation
    /// line type in the real file is a NAMED loss (`NATIVE_NOT_PROJECTED`); the
    /// `projected` record lands. Reports, per thread and in total, the
    /// conversation ids the real file has that the render lacks and the
    /// reverse — the measured distance, printed rather than asserted, because
    /// the archive and the copy do not cover the same turns (the copy holds
    /// turns from before the capture began and from other accounts).
    #[test]
    #[ignore = "opt-in: set PROTO_LEDGER_CORPUS to a COPY of the archive (never the live one) and run with --ignored"]
    fn the_real_round_trip_when_a_copy_is_named() {
        use crate::ledger::claude_code::{
            chain_break, read_native, record_projected, render_to, unlisted_native_types, RenderEnv,
        };
        use std::collections::BTreeSet;
        let Ok(root) = std::env::var("PROTO_LEDGER_CORPUS") else {
            panic!("PROTO_LEDGER_CORPUS not set; the real round-trip did not run — this test is opt-in and must not report ok having read nothing");
        };
        let root = PathBuf::from(root);
        let out = root.join("renders");
        std::fs::create_dir_all(&out).unwrap();
        let env = RenderEnv { cwd: "/".into(), version: "2.1.280".into(), git_branch: None };
        let mut natives: Vec<PathBuf> = std::fs::read_dir(root.join("native"))
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        natives.sort();
        let (mut threads, mut missing_total, mut extra_total, mut real_total, mut rendered_total) = (0, 0, 0, 0, 0);
        let mut unlisted_all: BTreeSet<String> = BTreeSet::new();
        let mut losses_all: BTreeMap<String, usize> = BTreeMap::new();
        let mut other_all: BTreeMap<String, usize> = BTreeMap::new();
        for native in natives {
            let id = native.file_stem().unwrap().to_str().unwrap().to_string();
            let dir = root.join("turns").join(&id);
            if !dir.is_dir() {
                continue;
            }
            threads += 1;
            let prompts = prompts_from_native(&native).unwrap();
            let (mut ledger, _) = ingest_thread(&dir, &prompts).unwrap();
            let path = out.join(format!("{id}.jsonl"));
            let r = render_to(&ledger, &env, &path).unwrap();
            record_projected(&mut ledger, &r, "2026-10-08T00:00:00Z", None);
            assert_eq!(ledger.events().last().unwrap().body["what"], "projected");
            for (k, v) in &r.losses {
                *losses_all.entry(k.to_string()).or_insert(0) += v;
            }
            let back = read_native(&path).unwrap();
            assert_eq!(chain_break(&back.chain), None, "{id}: the render is a walkable chain");
            assert_eq!(back.malformed, 0);
            let real = read_native(&native).unwrap();
            for t in unlisted_native_types(&real.other) {
                unlisted_all.insert(t);
            }
            for (k, v) in &real.other {
                *other_all.entry(k.clone()).or_insert(0) += v;
            }
            let real_ids: BTreeSet<&str> = real.chain.iter().filter(|c| !c.is_meta).map(|c| c.uuid.as_str()).collect();
            let rendered_ids: BTreeSet<&str> = back.chain.iter().filter(|c| !c.is_meta).map(|c| c.uuid.as_str()).collect();
            let missing = real_ids.difference(&rendered_ids).count();
            let extra = rendered_ids.difference(&real_ids).count();
            eprintln!(
                "{id}: real {} conversation lines, rendered {}, missing from render {missing}, not in real {extra}; losses {:?}",
                real_ids.len(),
                rendered_ids.len(),
                r.losses
            );
            missing_total += missing;
            extra_total += extra;
            real_total += real_ids.len();
            rendered_total += rendered_ids.len();
        }
        eprintln!(
            "round-trip over {threads} threads: real {real_total} conversation lines, rendered {rendered_total}, missing from render {missing_total}, not in real {extra_total}"
        );
        eprintln!("losses met: {losses_all:?}");
        eprintln!("real-file non-conversation line types: {other_all:?}");
        assert!(threads > 0, "the corpus names a native copy for at least one archived thread");
        assert!(unlisted_all.is_empty(), "native line types not named as a loss: {unlisted_all:?}");
    }
}
