//! Full import of AUI's existing thread stores (Q8) + delta reconciliation.
//!
//! Source format (harness `libs/aui-nav/src/threads.rs`): per project,
//! `.mind/threads.json` (a `ThreadIndex` of headers) plus one append-only
//! `.mind/threads/<thread_id>.jsonl` of `ThreadEvent` lines, each carrying a
//! stable 0-based `idx` (its line position) — the spoken message id.
//!
//! Mapping (the operator ordered a FULL import, not IDs-only — §12 Q8):
//!
//! - The ledger thread id is the header's **`session_id`** (the claude uuid) —
//!   the same join key the daemon adopts on `threads.create` (Q2), so imported
//!   threads resume seamlessly. The AUI-side `thread_id` and the full header
//!   are preserved under the thread settings' `metadata.aui_import`.
//! - Line `i` → `seq = i + 1`, `message_id = i` — **ids are preserved
//!   verbatim** (and legacy pre-`idx` rows, which all parse as `idx = 0` in
//!   AUI, are REPAIRED by position — design doc §8.4's strongest argument).
//! - `user` → `UserMessage` (full text; `wav_path` moves into metadata),
//!   `assistant_speech` → `AssistantText`, legacy `human_turn`/`agent_turn` →
//!   the same two. Every other kind (`summary_updated`, `failed_input`,
//!   `unprocessed_input`, `assistant_suggestions`, `thread_compacted`, and
//!   anything unknown) → `Custom { event_type: <original kind>, data:
//!   <original line verbatim> }` — full fidelity by construction.
//! - An UNPARSEABLE line → `Custom { event_type: "unparsed_line" }`, still
//!   occupying its position, so later ids never shift (this fixes the
//!   idx-vs-loaded-events divergence AUI has today).
//!
//! **Delta reconciliation** (Q8: "a small delta of all the actions that
//! happened from the last migration"): every import records a per-thread line
//! watermark in `import_watermarks`; re-running skips already-imported lines
//! and appends only the newer ones. `append_event` is idempotent on
//! `(thread_id, seq)` anyway, so overlap is harmless.
//!
//! Run with the daemon STOPPED (or before it first serves these threads):
//! the daemon self-heals its in-memory counters from the ledger on thread
//! rehydrate, but an actively-running thread's counters could race an import.
//!
//! ## The three things an import must get right to be RESUMABLE
//!
//! An imported thread is only worth anything if the daemon's next spawn finds
//! the conversation the operator has already had. `resumable` is derived from
//! `max_seq > 0`, so every imported thread spawns with `--resume <session>` —
//! and `claude --resume` resolves inside `<CLAUDE_CONFIG_DIR>/projects/<slug of
//! the cwd>/`. Three coordinates therefore have to be true, not merely
//! plausible, and each of them is DERIVED from the conversation on disk rather
//! than assumed from the AUI files:
//!
//! 1. **Account** — the thread is placed on the account whose config dir
//!    actually holds the transcript ([`locate_transcript`]), and that account is
//!    also written to `thread_accounts` so the admission gate's
//!    "moving here resumes / restarts" question has the same answer placement
//!    does. A thread with no conversation anywhere goes to the machine default
//!    (where the AUI direct path has always run it) with NO `thread_accounts`
//!    row — it is genuinely free to be placed.
//! 2. **Working directory** — read out of the transcript's own `cwd` field, not
//!    guessed from the project root. Every AUI transcript on this machine sits
//!    under the harness slug because the direct path never set a working
//!    directory and inherited the ui-server's; the project root would have
//!    missed all of them.
//! 3. **Identity of the session** — a session id appearing in more than one
//!    project index is ONE conversation, not several. The importer resolves the
//!    collision deterministically (richest source wins) and RECORDS every
//!    source it saw; it never lets "whichever project was read last" decide.
//!
//! If a conversation is found but the cwd it records does not slug to the
//! directory it lives in, the thread is **not imported at all** — it stays on
//! the direct path, where it already works. Continuity outranks the migration.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::accounts::{config_dir_of, Registry, DEFAULT_ACCOUNT};
use crate::error::{Error, Result};
use crate::event::{SendIntent, StoredEvent, ThreadEvent};
use crate::settings::ThreadSettings;
use crate::store::{now_rfc3339, Store, ThreadRow};

// ──────────────────────────────────────────────────────────────────────────
//  Where conversations actually live
// ──────────────────────────────────────────────────────────────────────────

/// One registered account's claude config dir, as the importer sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountStore {
    pub name: String,
    /// The `CLAUDE_CONFIG_DIR` root (transcripts live under `projects/`).
    pub root: PathBuf,
}

/// Everything the importer needs beyond the AUI files: WHICH stores to search
/// for a session's conversation, and where a thread with no conversation goes.
///
/// Built from the account registry by [`ImportContext::from_registry`]; built
/// by hand in tests, which is why the fields are public.
#[derive(Debug, Clone, Default)]
pub struct ImportContext {
    /// Searched in order; the machine default first.
    pub stores: Vec<AccountStore>,
    /// The account a thread with no located conversation is placed on. `None`
    /// leaves the account NULL (no registry at all — tests, and a machine with
    /// no accounts, where NULL is the honest answer).
    ///
    /// **It is never the machine default.** See [`Self::from_registry`].
    pub default_account: Option<String>,
}

impl ImportContext {
    /// The stores of every registered account.
    ///
    /// # The machine default is neither searched first nor placed on
    ///
    /// This function used to do both, and both WROTE THE NAME INTO THE STORE —
    /// which made it the last path in the crate that could still create new
    /// `default` rows, long after placement stopped knowing the concept:
    ///
    /// * It hoisted the machine default to the FRONT of the search order, so
    ///   `~/.claude` — the operator's own terminal login store — was scanned
    ///   first for every session id. A hit there set the thread's account to
    ///   `"default"`, and that string was written to `threads.account` and
    ///   embedded in the thread's `settings_json`.
    /// * It then PREFERRED `"default"` as the fallback for any thread whose
    ///   conversation could not be located at all, so those rows were written
    ///   with the same name.
    ///
    /// Both are gone. The search order is plain alphabetical, and the fallback
    /// is the first REAL account — which is the only answer placement can act
    /// on, since nothing named `default` is in the pool and a thread pinned
    /// there could never be served.
    ///
    /// > *"We do not want the threads module to have any concept of the
    /// > default."*
    ///
    /// **Existing history is not rewritten** — 151 `threads` rows and 37
    /// `thread_accounts` rows still name it, and that is a record of where work
    /// actually ran. This stops more being created; it does not revise the past.
    pub fn from_registry(reg: &Registry) -> Self {
        let mut names = reg.names();
        // Alphabetical, and nothing hoisted. The hoist that used to be here put
        // the operator's own login at the front of the search.
        names.sort();
        names.retain(|n| n != DEFAULT_ACCOUNT);
        let mut stores = Vec::new();
        for name in &names {
            let Some(acct) = reg.get(name) else { continue };
            let root = match config_dir_of(acct) {
                Some(p) => p,
                None => match machine_claude_home() {
                    Some(p) => p,
                    None => continue,
                },
            };
            stores.push(AccountStore {
                name: name.clone(),
                root,
            });
        }
        // The first REAL account, or `None`. Never the machine default: a
        // thread placed there could never be served, because nothing named
        // `default` is a pool member — so writing it would record a location
        // that is not a location.
        let default_account = names.first().cloned();
        Self {
            stores,
            default_account,
        }
    }
}

/// The machine default `CLAUDE_CONFIG_DIR`: the env var when set, else
/// `~/.claude`. (An account registered with no `config_dir` IS this store.)
pub fn machine_claude_home() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !d.is_empty() {
            return Some(PathBuf::from(d));
        }
    }
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".claude"))
}

/// Claude Code's project-directory slug: every character that is not ASCII
/// alphanumeric becomes `-`.
///
/// `/Users/me/code/harness` → `-Users-me-code-harness`;
/// `/Users/me/.aui/mythcellaneous` → `-Users-me--aui-mythcellaneous`.
/// This is the function that decides whether `--resume` finds a conversation,
/// so the importer uses it to VERIFY a derived cwd rather than to invent one
/// (the mapping is lossy and cannot be inverted).
pub fn claude_project_slug(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// A located claude conversation for one session id.
#[derive(Debug, Clone, PartialEq)]
pub struct LocatedTranscript {
    /// The registered account whose store holds it.
    pub account: String,
    /// The `projects/<dir>` directory name it lives in.
    pub dir: String,
    pub path: PathBuf,
    pub bytes: u64,
    /// The working directory the conversation itself records.
    pub cwd: Option<String>,
}

/// Find `session_id`'s conversation across every store, largest copy wins
/// (ties broken by store order, i.e. the machine default).
pub fn locate_transcript(ctx: &ImportContext, session_id: &str) -> Option<LocatedTranscript> {
    let mut best: Option<LocatedTranscript> = None;
    for st in &ctx.stores {
        let Ok(rd) = std::fs::read_dir(st.root.join("projects")) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path().join(format!("{session_id}.jsonl"));
            let Ok(md) = std::fs::metadata(&path) else {
                continue;
            };
            if !md.is_file() {
                continue;
            }
            let cand = LocatedTranscript {
                account: st.name.clone(),
                dir: entry.file_name().to_string_lossy().to_string(),
                cwd: transcript_cwd(&path),
                path,
                bytes: md.len(),
            };
            if best.as_ref().is_none_or(|b| cand.bytes > b.bytes) {
                best = Some(cand);
            }
        }
    }
    best
}

/// The working directory a transcript records for itself — claude stamps `cwd`
/// on every message line. Reads only as far as the first line that carries one.
fn transcript_cwd(path: &Path) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    for line in BufReader::new(f)
        .lines()
        .take(20_000)
        .map_while(std::result::Result::ok)
    {
        if !line.contains("\"cwd\"") {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                if !c.is_empty() {
                    return Some(c.to_string());
                }
            }
        }
    }
    None
}

// ──────────────────────────────────────────────────────────────────────────
//  The report
// ──────────────────────────────────────────────────────────────────────────

/// What one import/reconcile pass did.
#[derive(Debug, Default)]
pub struct ImportReport {
    /// Index HEADERS seen (175 on this machine — more than there are threads).
    pub threads_seen: usize,
    pub threads_created: usize,
    pub events_imported: usize,
    pub events_skipped_watermark: u64,
    /// Distinct session ids after collision resolution.
    pub threads_distinct: usize,
    /// Session ids that appeared in more than one project index.
    pub collisions_resolved: usize,
    /// Threads whose claude conversation was found on disk.
    pub threads_with_conversation: usize,
    /// account name → threads placed on it.
    pub placed: BTreeMap<String, usize>,
    /// Threads deliberately NOT imported, with the reason. These stay on the
    /// direct path — never forced.
    pub skipped_unsafe: Vec<String>,
}

impl ImportReport {
    fn merge(&mut self, r: ImportReport) {
        self.threads_seen += r.threads_seen;
        self.threads_created += r.threads_created;
        self.events_imported += r.events_imported;
        self.events_skipped_watermark += r.events_skipped_watermark;
        self.threads_distinct += r.threads_distinct;
        self.collisions_resolved += r.collisions_resolved;
        self.threads_with_conversation += r.threads_with_conversation;
        for (k, v) in r.placed {
            *self.placed.entry(k).or_default() += v;
        }
        self.skipped_unsafe.extend(r.skipped_unsafe);
    }
}

impl std::fmt::Display for ImportReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "threads seen: {}, distinct: {}, created: {}, events imported: {}, \
             skipped (already imported): {}, collisions resolved: {}, \
             with a conversation: {}, placed: {:?}, not imported (unsafe): {}",
            self.threads_seen,
            self.threads_distinct,
            self.threads_created,
            self.events_imported,
            self.events_skipped_watermark,
            self.collisions_resolved,
            self.threads_with_conversation,
            self.placed,
            self.skipped_unsafe.len(),
        )
    }
}

// ──────────────────────────────────────────────────────────────────────────
//  Sources + collision resolution
// ──────────────────────────────────────────────────────────────────────────

/// One `.mind/threads.json` header, with its project and its event file.
#[derive(Debug, Clone)]
struct Source {
    project: String,
    project_root: PathBuf,
    aui_thread_id: String,
    header: Value,
    jsonl: PathBuf,
    /// Size of the AUI event file — the richness measure a collision is broken
    /// on (a longer log is a superset of a shorter one for a copied project).
    bytes: u64,
    last_turn_at: String,
}

impl Source {
    fn summary(&self) -> Value {
        json!({
            "project": self.project,
            "project_root": self.project_root.display().to_string(),
            "aui_thread_id": self.aui_thread_id,
            "event_file_bytes": self.bytes,
            "last_turn_at": self.last_turn_at,
        })
    }
}

/// Read one project's index into `(session_id, Source)` pairs.
fn collect_sources(project_root: &Path) -> Result<Vec<(String, Source)>> {
    let index_path = project_root.join(".mind").join("threads.json");
    let index: Value = match std::fs::read_to_string(&index_path) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| Error::Config(format!("parsing {}: {e}", index_path.display())))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(Error::Config(format!(
                "reading {}: {e}",
                index_path.display()
            )))
        }
    };
    let empty = Vec::new();
    let headers = index
        .get("threads")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let project = project_root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| project_root.display().to_string());
    let mut out = Vec::new();
    for header in headers {
        let Some(session_id) = header.get("session_id").and_then(Value::as_str) else {
            continue; // unusable header — nothing to key the thread by
        };
        let aui_thread_id = header
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let jsonl = project_root
            .join(".mind")
            .join("threads")
            .join(format!("{aui_thread_id}.jsonl"));
        let bytes = std::fs::metadata(&jsonl).map(|m| m.len()).unwrap_or(0);
        out.push((
            session_id.to_string(),
            Source {
                project: project.clone(),
                project_root: project_root.to_path_buf(),
                aui_thread_id,
                header: header.clone(),
                jsonl,
                bytes,
                last_turn_at: header
                    .get("last_turn_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
        ));
    }
    Ok(out)
}

/// Import (or delta-reconcile) every thread of one project root.
pub fn import_project(store: &Store, project_root: &Path, ctx: &ImportContext) -> Result<ImportReport> {
    import_sources(store, ctx, collect_sources(project_root)?)
}

/// Import every project under a minds root (each child dir — or symlink —
/// holding a `.mind/threads.json`).
///
/// The whole root is collected BEFORE anything is written, because a session id
/// shared by two projects is one conversation and has to be resolved with all
/// its sources in view (gap 4). Importing project-by-project is what let
/// "whichever project was read last" silently decide.
pub fn import_minds_root(
    store: &Store,
    minds_root: &Path,
    ctx: &ImportContext,
) -> Result<ImportReport> {
    let entries = std::fs::read_dir(minds_root)
        .map_err(|e| Error::Config(format!("reading {}: {e}", minds_root.display())))?;
    let mut roots: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(".mind").join("threads.json").exists())
        .collect();
    roots.sort();
    let mut sources = Vec::new();
    for root in &roots {
        sources.extend(collect_sources(root)?);
    }
    import_sources(store, ctx, sources)
}

fn import_sources(
    store: &Store,
    ctx: &ImportContext,
    sources: Vec<(String, Source)>,
) -> Result<ImportReport> {
    let mut total = ImportReport {
        threads_seen: sources.len(),
        ..Default::default()
    };
    let mut by_session: BTreeMap<String, Vec<Source>> = BTreeMap::new();
    for (sid, src) in sources {
        by_session.entry(sid).or_default().push(src);
    }
    total.threads_distinct = by_session.len();
    for (session_id, mut group) in by_session {
        if group.len() > 1 {
            total.collisions_resolved += 1;
        }
        // Deterministic: richest event file, then most recent turn, then the
        // project name. Never "whichever was read last".
        group.sort_by(|a, b| {
            b.bytes
                .cmp(&a.bytes)
                .then(b.last_turn_at.cmp(&a.last_turn_at))
                .then(a.project.cmp(&b.project))
        });
        let mut one = ImportReport::default();
        import_thread(store, ctx, &session_id, &group, &mut one)?;
        total.merge(one);
    }
    Ok(total)
}

fn import_thread(
    store: &Store,
    ctx: &ImportContext,
    session_id: &str,
    group: &[Source],
    report: &mut ImportReport,
) -> Result<()> {
    let winner = &group[0];

    // ── Where does this conversation actually live? (gaps 2 + 3) ─────────
    let transcript = locate_transcript(ctx, session_id);
    let (cwd, cwd_source) = match &transcript {
        Some(t) => match t.cwd.as_deref() {
            // The one thing that makes `--resume` work: the cwd we write must
            // slug to the directory the conversation is actually filed under.
            Some(c) if claude_project_slug(c) == t.dir => (c.to_string(), "transcript"),
            other => {
                // Found a conversation we cannot prove resumable. Importing it
                // would make the daemon spawn `--resume`, miss, and mint a NEW
                // conversation over the same id — silent, unrecoverable context
                // loss. Leave it on the direct path, where it works today.
                report.skipped_unsafe.push(format!(
                    "{session_id}: conversation at {} records cwd {:?}, which does not \
                     resolve to its own directory {} — left on the direct path",
                    t.path.display(),
                    other,
                    t.dir
                ));
                return Ok(());
            }
        },
        None => (winner.project_root.display().to_string(), "project_root"),
    };
    let account = match &transcript {
        Some(t) => Some(t.account.clone()),
        None => ctx.default_account.clone(),
    };
    if transcript.is_some() {
        report.threads_with_conversation += 1;
    }
    if let Some(a) = &account {
        *report.placed.entry(a.clone()).or_default() += 1;
    }

    // Thread row: create if absent; NEVER clobber a live thread's settings.
    let existing = store.get_thread(session_id)?;
    if existing.is_none() {
        let mut settings = ThreadSettings::new(cwd.clone(), "sonnet");
        settings.metadata = json!({
            "aui_import": winner.header,
            // Every index this session id was found in, richest first. A
            // collision is RECORDED, never silently merged (gap 4).
            "aui_import_sources": group.iter().map(Source::summary).collect::<Vec<_>>(),
            "aui_cwd_source": cwd_source,
            "aui_transcript": match &transcript {
                Some(t) => json!({
                    "account": t.account,
                    "path": t.path.display().to_string(),
                    "project_dir": t.dir,
                    "bytes": t.bytes,
                }),
                None => Value::Null,
            },
        });
        let created = winner
            .header
            .get("created_at")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(now_rfc3339);
        store.upsert_thread(&ThreadRow {
            id: session_id.to_string(),
            // The account whose store holds the conversation — not NULL, which
            // round-robins the thread away from its own history (gap 2).
            account: account.clone(),
            cwd: Some(cwd),
            model: Some(settings.model.clone()),
            settings_json: serde_json::to_string(&settings)?,
            last_seq: 0,
            next_message_id: 1,
            last_compaction_message_id: 0,
            needs_restate: false,
            created_at: created,
            updated_at: now_rfc3339(),
        })?;
        report.threads_created += 1;
    }

    // The admission gate asks "does this thread have a conversation on that
    // account?" — the same question placement just answered. Record it so the
    // two agree (they disagreed by construction while this was empty).
    if let (Some(t), Some(a)) = (&transcript, &account) {
        if &t.account == a {
            store.record_thread_account(session_id, a)?;
        }
    }

    // Event lines, from the watermark (delta reconciliation, Q8) — from the
    // WINNING source only; the other copies of a collided thread are recorded
    // in the metadata above and never appended over the top.
    let body = match std::fs::read_to_string(&winner.jsonl) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(Error::Config(format!(
                "reading {}: {e}",
                winner.jsonl.display()
            )))
        }
    };
    let lines: Vec<&str> = body.lines().collect();
    let source = format!("{}|{}", winner.project_root.display(), winner.aui_thread_id);
    let watermark = store.get_watermark(&source)?.unwrap_or(0);

    // **THE NEWEST `user_message` STAMP THIS IMPORT APPENDS**, carried out of the
    // loop because an import appends events WITHOUT calling `bump_last_seq` per
    // event — it bumps once at the end — so there is no per-append site to put
    // the recency stamp on. Without this, an imported thread would land with
    // `last_user_input_at` NULL and hold no place in `threads/recent-updates`
    // even though the operator's words are right there in the rows just written;
    // the one-time backfill cannot save it either, because that runs at open and
    // this import happens afterwards. `is_user_message` is the same classifier the
    // live append path and the fold use.
    let mut newest_user_input: Option<String> = None;
    for (i, line) in lines.iter().enumerate() {
        if (i as u64) < watermark {
            report.events_skipped_watermark += 1;
            continue;
        }
        let (ts, event) = map_line(line);
        if event.is_user_message() && newest_user_input.as_deref().is_none_or(|n| ts.as_str() > n) {
            newest_user_input = Some(ts.clone());
        }
        store.append_event(&StoredEvent {
            thread_id: session_id.to_string(),
            seq: i as u64 + 1,
            message_id: Some(i as u64),
            ts,
            event,
        })?;
        report.events_imported += 1;
    }

    // Counters: seq/id cursors continue past the imported tail.
    let line_count = lines.len() as u64;
    if line_count > 0 {
        // Only the WATERMARK, never a status: an import writes history, and an
        // imported thread is running exactly as much as it was a moment ago
        // (nothing). Saying "idle" here was harmless and was still a fourth
        // writer of an answer nobody should have been writing.
        store.bump_last_seq(
            session_id,
            store.max_seq(session_id)?,
            newest_user_input.as_deref(),
        )?;
        let next_id = store.max_message_id(session_id)? + 1;
        store.update_thread_counters(session_id, next_id)?;
    }
    store.set_watermark(&source, session_id, line_count)?;
    Ok(())
}

/// Map one AUI JSONL line to `(ts, ThreadEvent)`.
fn map_line(line: &str) -> (String, ThreadEvent) {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            return (
                now_rfc3339(),
                ThreadEvent::Custom {
                    event_type: "unparsed_line".into(),
                    data: json!({ "raw": line }),
                },
            )
        }
    };
    let ts = v
        .get("ts")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(now_rfc3339);
    let kind = v.get("kind").and_then(Value::as_str).unwrap_or("");
    let text = v
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let event = match kind {
        "user" | "human_turn" => ThreadEvent::UserMessage {
            text,
            intent: SendIntent::Immediate,
            metadata: json!({
                "source": "aui_import",
                "wav_path": v.get("wav_path").cloned().unwrap_or(Value::Null),
            }),
        },
        "assistant_speech" | "agent_turn" => ThreadEvent::AssistantText {
            text,
            parent_tool_use_id: None,
            api_message_id: None,
        },
        // Everything else keeps its original vocabulary + full payload —
        // nothing is lost, and consumers keep their own event types (§11).
        other => ThreadEvent::Custom {
            event_type: if other.is_empty() {
                "unknown".to_string()
            } else {
                other.to_string()
            },
            data: v.clone(),
        },
    };
    (ts, event)
}
