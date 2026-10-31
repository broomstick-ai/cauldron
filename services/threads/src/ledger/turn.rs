//! # The projected turn — `ledger/` wired into the three moments of a daemon turn
//!
//! R4.S4a. `docs/proto-ledger.md` §3 (projection at turn start), §4 (reconcile
//! at turn end, enforcement at placement). This module is the daemon-facing
//! surface of the proto-ledger: `runtime::run_once` calls [`ProjectedTurn::begin`]
//! before the spawn, [`ProjectedTurn::observe`] on every wire line it tails,
//! and [`ProjectedTurn::end`] after the child exits; `runtime::move_thread`
//! calls [`check_placement`] before it moves a thread's store. Nothing here
//! runs for a thread whose settings say `projection: native_owned`, which is
//! the default, so the daemon's path for every unflipped thread is today's.
//!
//! **The ledger lives at `<data_dir>/ledgers/<thread_id>.jsonl`** in the file
//! form [`Ledger`] defines, appended per event through the one write.
//! `docs/ASSUMPTIONS-LOG.md` A27 records why it is under the daemon's data dir
//! rather than the `~/.cauldron/data/thread_ledgers/` the operator named: this
//! step may not touch `~/.cauldron`, and the daemon has one data root.
//!
//! **The dirty mark is DERIVED, never stored.** [`dirty::DirtyMarks`] is
//! in-memory by design; here the mark is read off the ledger itself — the last
//! `system{projected}` with no `system{reconciled}` after it — so a daemon that
//! restarts between a turn's exit and its reconcile still knows the thread is
//! dirty, with no column and no second writer. [`dirty_mark`] is that read.
//!
//! **Three refusals, all typed, all reaching the caller:**
//!
//! * [`ProjectionRefused::NativeWithoutLedger`] — the thread holds a native
//!   conversation and no ledger. A render from an empty ledger would overwrite
//!   the only copy of that conversation, so the turn parks instead and the
//!   cutover (R4.S4b) is what builds the ledger first.
//! * [`ProjectionRefused::LossNotAllowed`] — the render met a named loss the
//!   operator has not ruled accepted ([`ACCEPTED_LOSSES`]) and the thread's
//!   settings do not allow by name (`projection_allow_loss`).
//! * [`PlacementRefused`] — the native file holds what the ledger does not:
//!   either a turn ran and was never reconciled (dirty), or the file's digest
//!   is not the one the ledger last recorded (someone wrote to it since).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::claude_code::{self, record_projected, render_lines, render_to, Outcome, RenderEnv, Rendered, RUNTIME};
use super::dirty::{self, DirtyMarks, Mark, Reconciled};
use super::event::{AppendRefused, Draft, Kind, Ledger, LedgerEvent, ReadError, ThreadMeta};

/// Where a thread's ledger lives under the daemon's data directory.
pub fn ledger_path(data_dir: &Path, thread_id: &str) -> PathBuf {
    data_dir.join("ledgers").join(format!("{thread_id}.jsonl"))
}

/// Where this thread's native file lives for this account: the runtime's own
/// rule, `<config_dir or ~/.claude>/projects/<slug(cwd)>/<thread_id>.jsonl`.
/// A function of its inputs, never a scan — `docs/thread-adapters.md` §5.7's
/// `locate`. The 200-character cut-plus-hash the runtime applies to very long
/// cwds is not reproduced (A29).
///
/// The cwd is CANONICALIZED before it is slugged, because the runtime slugs
/// what `getcwd()` answers, not the string it was handed: on macOS a thread
/// whose settings say `/var/folders/…` runs in `/private/var/folders/…`, and
/// the runtime files the session under the latter. Measured by
/// `tests/projected_turn.rs`, where the two slugs disagreed until this line.
/// A cwd that does not exist slugs as written.
pub fn native_path(config_dir: Option<&str>, home: &Path, cwd: &str, thread_id: &str) -> PathBuf {
    let physical = std::fs::canonicalize(cwd)
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| cwd.to_string());
    crate::accounts::projects_dir_for(config_dir, home)
        .join(crate::import::claude_project_slug(&physical))
        .join(format!("{thread_id}.jsonl"))
}

/// **THE LOSSES THE OPERATOR ALREADY RULED ACCEPTED** — `docs/proto-ledger.md`
/// §3's table, *"accept certain loss"*. A render that meets only these needs
/// no allowance. Any other name in [`claude_code::LOSSES`] must be allowed by
/// the thread or the render is refused.
pub const ACCEPTED_LOSSES: &[&str] = &["subagent_interior", "not_a_conversation_line", "folded_block"];

/// **A LOSS NAME THE RENDER CANNOT MEET IS REFUSED AT THE DOOR** (R4.S4b,
/// HOLDS R17). `projection_allow_loss` is matched by exact name against what
/// a render reports (`claude_code::LOSSES`), so a misspelled name allows
/// nothing and the turn parks with a `projection` refusal that names the loss
/// the operator believed he had allowed. Checked where a setting is written
/// (`update_settings`, `create_thread`), so the typo is answered to whoever
/// typed it rather than discovered as a park later. `Err` names every unknown
/// name and every known one.
pub fn validate_allow_loss(names: &[String]) -> Result<(), String> {
    let unknown: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| !claude_code::LOSSES.contains(n))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(format!(
        "projection_allow_loss names no loss a render can meet: {unknown:?} (known: {:?})",
        claude_code::LOSSES
    ))
}

/// Why a projected turn could not begin. Every variant names what the caller
/// needs to say in the park reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionRefused {
    /// The thread's ledger file exists and cannot be read.
    LedgerUnreadable { path: PathBuf, why: String },
    /// The thread has a native conversation and no ledger: rendering would
    /// overwrite it. The cutover builds the ledger first.
    NativeWithoutLedger { native: PathBuf, bytes: u64 },
    /// The render met a named loss the thread does not allow.
    LossNotAllowed {
        loss: String,
        count: usize,
        allowed: Vec<String>,
    },
    /// Writing the render, or the ledger, failed.
    Io { path: PathBuf, why: String },
}

impl std::fmt::Display for ProjectionRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectionRefused::LedgerUnreadable { path, why } => {
                write!(f, "the thread's ledger at {} cannot be read: {why}", path.display())
            }
            ProjectionRefused::NativeWithoutLedger { native, bytes } => write!(
                f,
                "this thread holds a native conversation ({bytes} bytes at {}) and no ledger; \
                 rendering would overwrite it — cut the thread over first (R4.S4b)",
                native.display()
            ),
            ProjectionRefused::LossNotAllowed { loss, count, allowed } => write!(
                f,
                "the render would lose `{loss}` ({count} event(s)) and this thread does not allow \
                 that loss by name (projection_allow_loss = {allowed:?})"
            ),
            ProjectionRefused::Io { path, why } => write!(f, "{}: {why}", path.display()),
        }
    }
}

/// Why a thread may not be placed on another account right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementRefused {
    /// A turn ran and the file was never reconciled — `dirty`'s own refusal.
    Dirty(dirty::PlacementRefused),
    /// The native file's digest is not the one the ledger last recorded for it.
    NativeMoved {
        thread_id: String,
        to_account: String,
        path: PathBuf,
        expected_digest: String,
        found_digest: String,
    },
    LedgerUnreadable { path: PathBuf, why: String },
}

impl std::fmt::Display for PlacementRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlacementRefused::Dirty(d) => write!(f, "{d}"),
            PlacementRefused::NativeMoved { thread_id, to_account, path, expected_digest, found_digest } => write!(
                f,
                "thread {thread_id}: its native file {} moved since the ledger last looked \
                 (digest {found_digest}, the ledger recorded {expected_digest}); reconcile it \
                 before placing it on {to_account}",
                path.display()
            ),
            PlacementRefused::LedgerUnreadable { path, why } => {
                write!(f, "the thread's ledger at {} cannot be read: {why}", path.display())
            }
        }
    }
}

fn sha256_of(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The last `system{projected}` / `system{reconciled}` record, newest first.
fn last_mark_record(ledger: &Ledger) -> Option<&super::event::LedgerEvent> {
    ledger.events().iter().rev().find(|e| {
        e.kind == Kind::System
            && matches!(e.body.get("what").and_then(Value::as_str), Some("projected") | Some("reconciled"))
    })
}

/// **THE DIRTY MARK, READ OFF THE LEDGER.** `Some` when the last render was
/// never followed by a reconcile: the turn it was rendered for, and how many
/// bytes the render wrote — the byte the runtime's tail begins at.
pub fn dirty_mark(ledger: &Ledger) -> Option<Mark> {
    let e = last_mark_record(ledger)?;
    if e.body.get("what").and_then(Value::as_str) != Some("projected") {
        return None;
    }
    Some(Mark {
        since_turn: e.turn.clone().unwrap_or_default(),
        rendered_bytes: e.body.get("detail").and_then(|d| d.get("bytes")).and_then(Value::as_u64).unwrap_or(0),
    })
}

/// The digest the ledger last recorded for the native file: the render's, or
/// the whole file's as reconcile read it. `None` when the ledger has never
/// looked, or the file was lost at the last reconcile.
pub fn expected_native_digest(ledger: &Ledger) -> Option<String> {
    let e = last_mark_record(ledger)?;
    let detail = e.body.get("detail")?;
    match e.body.get("what").and_then(Value::as_str) {
        Some("projected") => detail.get("digest").and_then(Value::as_str).map(str::to_string),
        Some("reconciled") => detail.get("file_digest").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

/// **THE ONE ENFORCEMENT POINT.** Called by `runtime::move_thread` before a
/// projected thread moves to another account's store. A thread with no ledger
/// has never been projected and is placed as today. Otherwise: dirty refuses;
/// a native file whose digest is not the one the ledger last recorded refuses;
/// a native file that is simply absent does not (the next turn renders it).
pub fn check_placement(
    thread_id: &str,
    ledger_path: &Path,
    native_path: &Path,
    to_account: &str,
) -> Result<(), PlacementRefused> {
    if !ledger_path.exists() {
        return Ok(());
    }
    let ledger = Ledger::read_jsonl(ledger_path).map_err(|e| PlacementRefused::LedgerUnreadable {
        path: ledger_path.to_path_buf(),
        why: e.to_string(),
    })?;
    let mut marks = DirtyMarks::default();
    if let Some(m) = dirty_mark(&ledger) {
        marks.mark(thread_id, &m.since_turn, m.rendered_bytes);
    }
    marks.check_placement(thread_id, to_account).map_err(PlacementRefused::Dirty)?;
    if let (Some(expected), Ok(bytes)) = (expected_native_digest(&ledger), std::fs::read(native_path)) {
        let found = sha256_of(&bytes);
        if found != expected {
            return Err(PlacementRefused::NativeMoved {
                thread_id: thread_id.to_string(),
                to_account: to_account.to_string(),
                path: native_path.to_path_buf(),
                expected_digest: expected,
                found_digest: found,
            });
        }
    }
    Ok(())
}

/// The Claude Code version the thread last ran under, read off its last
/// `system{init}` event; `"unknown"` before any turn. The adapter's dialect
/// probe (`docs/thread-adapters.md` §8 Version) is not built here (A30).
fn version_of(ledger: &Ledger) -> String {
    ledger
        .events()
        .iter()
        .rev()
        .find(|e| e.kind == Kind::System && e.body.get("what").and_then(Value::as_str) == Some("init"))
        .and_then(|e| e.body.get("version").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".to_string())
}

/// What `begin` needs that the runtime holds.
#[derive(Debug, Clone)]
pub struct TurnInputs<'a> {
    pub data_dir: &'a Path,
    pub thread_id: &'a str,
    pub config_dir: Option<&'a str>,
    pub home: &'a Path,
    pub cwd: &'a str,
    pub prompt: &'a str,
    pub message_ids: &'a [u64],
    pub attempt: u32,
    pub cookie: &'a str,
    pub allow_loss: &'a [String],
    pub now: &'a str,
    /// **THE ONE WRITE LOCK FOR `ledgers/`** — the managed runtime's
    /// (`crate::managed::Managed::write_lock`). `ledgers/<thread>.jsonl` has a
    /// second writer: a parent's spawn edges, reports and wakes, appended
    /// while this turn is open (an `agents_spawn` is a tool call made DURING
    /// the turn). Every write of this turn is taken under it.
    pub write_lock: Arc<Mutex<()>>,
}

/// **ONE PROJECTED TURN**, from render to reconcile. Holds the thread's ledger
/// for the duration of the turn and writes every event through to its file as
/// it lands.
#[derive(Debug)]
pub struct ProjectedTurn {
    ledger: Ledger,
    ledger_path: PathBuf,
    native_path: PathBuf,
    cookie: String,
    /// What the render did, and the byte the runtime's tail begins at.
    pub rendered: Rendered,
    /// What `begin`'s crash-recovery reconcile found, if the thread was dirty.
    pub recovered: Option<Reconciled>,
    /// **R13.1 (B): the ledger ALREADY held this delivery's prompt** — a
    /// `user` line whose `metadata.daemon_message_ids` is exactly this
    /// batch, appended by an earlier attempt of the same delivery. When set,
    /// `begin` appended nothing, and the render this turn resumes from
    /// carries the message: the model has it, so the runtime must not write
    /// it again (it sends a continue note instead).
    pub prompt_on_ledger: bool,
    /// Wire lines whose id the ledger already held.
    pub replayed: usize,
    persisted: usize,
    write_lock: Arc<Mutex<()>>,
    /// The file's byte length after this turn's last write (or when `begin`
    /// read it). A different length at the next write means another writer
    /// appended in between.
    on_disk: u64,
}

/// An event already in one ledger, as a draft for another: its id, time,
/// turn, body and native line, with `seq`/`message_id` left to the one write.
fn redraft(e: &LedgerEvent) -> Draft {
    Draft {
        id: Some(e.id.clone()),
        ts: e.ts.clone(),
        turn: e.turn.clone(),
        within: e.within.clone(),
        kind: Some(e.kind),
        body: e.body.clone(),
        native: e.native.clone(),
    }
}

/// Whether `ledger` already holds the daemon's `user` line for exactly this
/// batch of message ids (R13.1 B) — the line [`ProjectedTurn::begin`] appends
/// once per delivery.
pub fn holds_prompt_for(ledger: &Ledger, message_ids: &[u64]) -> bool {
    ledger.events().iter().any(|e| {
        e.kind == Kind::User
            && e.body
                .pointer("/metadata/daemon_message_ids")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_u64()).collect::<Vec<_>>())
                .as_deref()
                == Some(message_ids)
    })
}

/// A native conversation anywhere in this account's store, for a thread whose
/// ledger cannot vouch for it, is one a render would overwrite.
fn refuse_if_native(i: &TurnInputs<'_>) -> Result<(), ProjectionRefused> {
    if let Some(existing) = crate::accounts::locate_transcript(i.config_dir, i.home, i.thread_id) {
        let bytes = std::fs::metadata(&existing).map(|m| m.len()).unwrap_or(0);
        if bytes > 0 {
            return Err(ProjectionRefused::NativeWithoutLedger { native: existing, bytes });
        }
    }
    Ok(())
}

impl ProjectedTurn {
    /// **BEFORE THE SPAWN.** Load (or start) the ledger; reconcile a dirty
    /// thread first (the crash-recovery path); append `turn_started`; render
    /// the native file in full and record `system{projected}`; then append the
    /// prompt as delivered — AFTER the render, so the runtime's own copy of it
    /// is the one in the file this turn and the ledger's copy renders from the
    /// next turn on (A15: the prompt keeps the daemon's id).
    pub fn begin(i: TurnInputs<'_>) -> Result<Self, ProjectionRefused> {
        Self::begin_inner(i, true)
    }

    /// **BEFORE AN INTERACTIVE ATTACH** (SH-A2, `docs/SHIMS.md` §A.1): every
    /// step of [`Self::begin`] — reconcile a dirty thread, `turn_started`,
    /// render, `system{projected}` — EXCEPT appending a prompt. There is none:
    /// the operator types it into the TUI, and it reaches the ledger through
    /// reconcile from the native file. Appending `i.prompt` (empty) here would
    /// put an empty user line into every later render.
    pub fn begin_interactive(i: TurnInputs<'_>) -> Result<Self, ProjectionRefused> {
        Self::begin_inner(i, false)
    }

    fn begin_inner(i: TurnInputs<'_>, append_prompt: bool) -> Result<Self, ProjectionRefused> {
        let ledger_path = ledger_path(i.data_dir, i.thread_id);
        let native_path = native_path(i.config_dir, i.home, i.cwd, i.thread_id);
        // The length BEFORE the read: an append racing the read only makes the
        // first write re-read the tail, never skip it.
        // R4.S4b (HOLDS R18): a torn tail is cut before the read, under the
        // one write lock, so a killed managed append cannot park the turn as
        // `LedgerUnreadable`.
        if ledger_path.exists() {
            let _w = i.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            super::repair_torn_tail(&ledger_path).map_err(|e| ProjectionRefused::LedgerUnreadable {
                path: ledger_path.clone(),
                why: e.to_string(),
            })?;
        }
        let on_disk = std::fs::metadata(&ledger_path).map(|m| m.len()).unwrap_or(0);
        let (ledger, persisted) = if ledger_path.exists() {
            let l = Ledger::read_jsonl(&ledger_path).map_err(|e| ProjectionRefused::LedgerUnreadable {
                path: ledger_path.clone(),
                why: match e {
                    ReadError::Io(e) => e.to_string(),
                    other => other.to_string(),
                },
            })?;
            // A LEDGER THAT HAS NEVER RENDERED CANNOT VOUCH FOR THE NATIVE FILE.
            // `ledgers/<thread>.jsonl` is also where R4.S2's managed runtime
            // writes a parent's STRUCTURE (task, child edges, reports) — so a
            // ledger existing is not a ledger holding the conversation. Until
            // its first `system{projected}`, a non-empty native file is the
            // only copy, and the refusal is the same as for no ledger at all.
            let ever_projected = l
                .events()
                .iter()
                .any(|e| e.kind == Kind::System && e.body.get("what").and_then(|w| w.as_str()) == Some("projected"));
            if !ever_projected {
                refuse_if_native(&i)?;
            }
            let n = l.len();
            (l, n)
        } else {
            // No ledger yet. A native conversation anywhere in this account's
            // store is one this render would overwrite — refuse by name.
            refuse_if_native(&i)?;
            (
                Ledger::new(ThreadMeta::root(i.thread_id, RUNTIME, i.now)),
                0,
            )
        };
        let mut this = ProjectedTurn {
            ledger,
            ledger_path,
            native_path,
            cookie: i.cookie.to_string(),
            rendered: Rendered {
                lines: 0,
                bytes: 0,
                digest: String::new(),
                losses: BTreeMap::new(),
            },
            recovered: None,
            prompt_on_ledger: false,
            replayed: 0,
            persisted,
            write_lock: i.write_lock.clone(),
            on_disk,
        };
        // Crash recovery: a turn whose reconcile never ran.
        if let Some(m) = dirty_mark(&this.ledger) {
            let mut marks = DirtyMarks::default();
            marks.mark(i.thread_id, &m.since_turn, m.rendered_bytes);
            let r = dirty::reconcile(&mut this.ledger, &mut marks, &this.native_path, i.now).map_err(|e| {
                ProjectionRefused::Io {
                    path: this.native_path.clone(),
                    why: e.to_string(),
                }
            })?;
            this.recovered = r;
        }
        this.ledger
            .append(
                Draft::new(
                    Kind::TurnStarted,
                    i.now,
                    json!({"attempt": i.attempt, "message_ids": i.message_ids}),
                )
                .in_turn(Some(i.cookie)),
            )
            .expect("a minted id is never a replay");
        // The render, checked against the thread's allowance before a byte is
        // written: a loss that is neither ruled accepted nor allowed by name
        // refuses the turn.
        let env = RenderEnv {
            cwd: i.cwd.to_string(),
            version: version_of(&this.ledger),
            git_branch: None,
        };
        let (_, losses) = render_lines(&this.ledger, &env);
        for (loss, count) in &losses {
            if !ACCEPTED_LOSSES.contains(loss) && !i.allow_loss.iter().any(|a| a == loss) {
                return Err(ProjectionRefused::LossNotAllowed {
                    loss: loss.to_string(),
                    count: *count,
                    allowed: i.allow_loss.to_vec(),
                });
            }
        }
        if let Some(parent) = this.native_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ProjectionRefused::Io {
                path: parent.to_path_buf(),
                why: e.to_string(),
            })?;
        }
        let rendered = render_to(&this.ledger, &env, &this.native_path).map_err(|e| ProjectionRefused::Io {
            path: this.native_path.clone(),
            why: e.to_string(),
        })?;
        if rendered.lines == 0 {
            // Nothing to resume from: the runtime starts the session itself
            // (`--session-id`), and an empty file in its way is not a session.
            let _ = std::fs::remove_file(&this.native_path);
        }
        record_projected(&mut this.ledger, &rendered, i.now, Some(i.cookie));
        this.rendered = rendered;
        // ── ONE COPY OF THE PROMPT PER DELIVERY, NOT PER ATTEMPT (R13.1 B) ──
        //
        // This used to append a fresh `user` line on EVERY attempt, so a
        // delivery retried N times rendered N-1 copies of the same message
        // into attempt N's conversation (an internal thread "ripples", message
        // 62: four deliveries). The batch's ids are the key: a `user` line
        // already carrying exactly these `daemon_message_ids` IS this
        // delivery's prompt, it renders into this turn's file, and a second
        // one would be a duplicate. A batch with no ids has nothing to key
        // on and keeps the old behaviour.
        if append_prompt {
            this.prompt_on_ledger = !i.message_ids.is_empty() && holds_prompt_for(&this.ledger, i.message_ids);
        }
        if append_prompt && !this.prompt_on_ledger {
        this.ledger
            .append(
                Draft::new(
                    Kind::User,
                    i.now,
                    json!({
                        "text": i.prompt,
                        "intent": "immediate",
                        "metadata": {"daemon_message_ids": i.message_ids},
                    }),
                )
                .in_turn(Some(i.cookie)),
            )
            .expect("a minted id is never a replay");
        }
        this.persist().map_err(|e| ProjectionRefused::Io {
            path: this.ledger_path.clone(),
            why: e.to_string(),
        })?;
        Ok(this)
    }

    /// Whether the runtime should `--resume` (the render holds a conversation)
    /// or `--session-id` (it does not).
    pub fn resumable(&self) -> bool {
        self.rendered.lines > 0
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }
    pub fn native_path(&self) -> &Path {
        &self.native_path
    }
    pub fn ledger_path(&self) -> &Path {
        &self.ledger_path
    }

    /// **DURING THE TURN.** One wire line, as the daemon tails it, through the
    /// same `translate` the archive parser uses. A line with no timestamp
    /// takes the daemon's accept time. Persistence failures are logged, never
    /// fatal to the turn: the `events` table is still written beside this.
    pub fn observe(&mut self, line: &Value, now: &str) {
        match claude_code::translate(line, Some(&self.cookie)) {
            Outcome::Events(drafts) => {
                for mut d in drafts {
                    if d.ts.is_empty() {
                        d.ts = now.to_string();
                    }
                    match self.ledger.append(d) {
                        Ok(_) => {}
                        Err(AppendRefused::Replay { .. }) => self.replayed += 1,
                        Err(AppendRefused::NoKind) => unreachable!("translate always sets a kind"),
                    }
                }
                if let Err(e) = self.persist() {
                    tracing::warn!(
                        thread = %self.ledger.thread_id(), error = %e,
                        "could not persist a projected turn's wire events to its ledger"
                    );
                }
            }
            Outcome::Ephemeral(_) | Outcome::Malformed(_) => {}
        }
    }

    /// **AFTER THE EXIT.** A turn the wire never closed gets a daemon-authored
    /// `turn_ended{interrupted}`; then the runtime's tail is reconciled from
    /// the render's byte length and `system{reconciled}` lands.
    pub fn end(&mut self, exit_code: Option<i32>, now: &str) -> std::io::Result<Reconciled> {
        let closed = self
            .ledger
            .events()
            .iter()
            .any(|e| e.kind == Kind::TurnEnded && e.turn.as_deref() == Some(self.cookie.as_str()));
        if !closed {
            let _ = self.ledger.append(
                Draft::new(
                    Kind::TurnEnded,
                    now,
                    json!({"outcome": "interrupted", "detail": {"exit_code": exit_code}}),
                )
                .in_turn(Some(&self.cookie)),
            );
        }
        let mut marks = DirtyMarks::default();
        marks.mark(self.ledger.thread_id(), &self.cookie, self.rendered.bytes);
        let r = dirty::reconcile(&mut self.ledger, &mut marks, &self.native_path, now)?
            .expect("the mark was just set, so reconcile had a thread to reconcile");
        self.persist()?;
        Ok(r)
    }

    /// Write every event not yet on disk, under the `ledgers/` write lock.
    /// The first write of a new ledger is the whole file (header line
    /// included); every later one appends.
    ///
    /// **TWO WRITERS, ONE FILE.** If the file moved since this turn's last
    /// write, another writer (the managed runtime's spawn edge, report or
    /// wake) appended to it, and this turn's in-memory copy is stale: its
    /// next `seq` is one the file already used. So the file is re-read, this
    /// turn's unwritten events are re-appended after ITS tail through the one
    /// write (re-deriving `seq` and `message_id`), and the re-read ledger —
    /// both writers' events, in file order — becomes this turn's copy.
    fn persist(&mut self) -> std::io::Result<()> {
        let lock = Arc::clone(&self.write_lock);
        let _w = lock.lock().unwrap_or_else(|e| e.into_inner());
        if !self.ledger_path.exists() {
            if let Some(parent) = self.ledger_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.ledger.write_jsonl(&self.ledger_path)?;
            self.persisted = self.ledger.len();
            self.on_disk = std::fs::metadata(&self.ledger_path)?.len();
            return Ok(());
        }
        if std::fs::metadata(&self.ledger_path)?.len() != self.on_disk {
            // R4.S4b (HOLDS R18): a killed foreign append leaves a line with no
            // newline; it is an append that did not happen. Cut it under the
            // lock, exactly as `Managed::load` does, before the re-read.
            super::repair_torn_tail(&self.ledger_path)?;
            let mut disk = Ledger::read_jsonl(&self.ledger_path).map_err(|e| {
                std::io::Error::other(format!(
                    "another writer appended to {} and it cannot be re-read: {e}",
                    self.ledger_path.display()
                ))
            })?;
            let base = disk.len();
            for e in &self.ledger.events()[self.persisted..] {
                match disk.append(redraft(e)) {
                    Ok(_) | Err(AppendRefused::Replay { .. }) => {}
                    Err(AppendRefused::NoKind) => unreachable!("a ledger event always has a kind"),
                }
            }
            self.ledger = disk;
            self.persisted = base;
        }
        for e in &self.ledger.events()[self.persisted..] {
            Ledger::append_to_file(&self.ledger_path, e)?;
        }
        self.persisted = self.ledger.len();
        self.on_disk = std::fs::metadata(&self.ledger_path)?.len();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>(
        dir: &'a Path,
        home: &'a Path,
        prompt: &'a str,
        cookie: &'a str,
        allow: &'a [String],
    ) -> TurnInputs<'a> {
        TurnInputs {
            data_dir: dir,
            thread_id: "t-proj",
            config_dir: None,
            home,
            cwd: "/w/x",
            prompt,
            message_ids: &[1],
            attempt: 1,
            cookie,
            allow_loss: allow,
            now: "2026-10-10T00:00:00Z",
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    /// **EVERY LOSS A RENDER CAN MEET MAY BE ALLOWED; ANY OTHER NAME IS
    /// REFUSED, BY NAME** (R4.S4b, HOLDS R17).
    ///
    /// **THE MUTATION.** In `validate_allow_loss`, return `Ok(())`
    /// unconditionally (or, opposed, refuse every non-empty list).
    ///
    /// Forced red:
    /// ```text
    /// an allow_loss name no render can meet is refused
    /// ```
    #[test]
    fn every_named_loss_is_allowed_and_an_unknown_name_is_refused() {
        // Every name a render reports may be allowed — the positive half, so a
        // validator that refuses everything fails here.
        let all: Vec<String> = claude_code::LOSSES.iter().map(|s| s.to_string()).collect();
        assert_eq!(validate_allow_loss(&all), Ok(()), "every name in LOSSES is allowable");
        assert_eq!(validate_allow_loss(&[]), Ok(()), "allowing nothing is allowed");
        // A typo of the one loss that needs allowing allows nothing, so it is refused, by name.
        let err = validate_allow_loss(&["prompt_text_unrecoverable".into(), "prompt_text_unrecoverabel".into()])
            .expect_err("an allow_loss name no render can meet is refused");
        assert!(err.contains("prompt_text_unrecoverabel"), "the refusal names the unknown name: {err}");
        assert!(!err.contains("[\"prompt_text_unrecoverable\""), "and only the unknown one as unknown: {err}");
    }

    /// **THE DIRTY MARK IS READ OFF THE LEDGER, AND IT SURVIVES A RELOAD.**
    /// After `begin` the thread is dirty since this cookie at the render's
    /// length; after `end` it is clean; a fresh read of the file says the same.
    ///
    /// **THE MUTATION.** In `dirty_mark`, return `None` unconditionally.
    ///
    /// Forced red:
    /// ```text
    /// a rendered-and-unreconciled thread is dirty since its cookie
    /// ```
    #[test]
    fn the_dirty_mark_is_derived_from_the_ledger() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let mut t = ProjectedTurn::begin(inputs(tmp.path(), &home, "hello", "c-1", &[])).unwrap();
        assert!(!t.resumable(), "a brand-new thread renders nothing and starts its own session");
        assert!(!t.native_path().exists(), "no empty file is left in the runtime's way");
        let on_disk = Ledger::read_jsonl(t.ledger_path()).unwrap();
        assert_eq!(
            dirty_mark(&on_disk),
            Some(Mark { since_turn: "c-1".into(), rendered_bytes: 0 }),
            "a rendered-and-unreconciled thread is dirty since its cookie"
        );
        // The runtime writes its file; the daemon tails one assistant line.
        std::fs::create_dir_all(t.native_path().parent().unwrap()).unwrap();
        std::fs::write(
            t.native_path(),
            "{\"type\":\"user\",\"uuid\":\"p1\",\"timestamp\":\"2026-10-10T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n\
             {\"type\":\"assistant\",\"uuid\":\"a1\",\"parentUuid\":\"p1\",\"timestamp\":\"2026-10-10T00:00:02Z\",\"message\":{\"id\":\"m\",\"model\":\"x\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n",
        )
        .unwrap();
        t.observe(
            &json!({"type": "assistant", "uuid": "a1", "timestamp": "2026-10-10T00:00:02Z", "message": {"id": "m", "model": "x", "content": [{"type": "text", "text": "hi"}]}}),
            "2026-10-10T00:00:02Z",
        );
        let r = t.end(Some(0), "2026-10-10T00:00:03Z").unwrap();
        assert_eq!((r.prompts_confirmed, r.already, r.ingested), (1, 1, 0));
        let on_disk = Ledger::read_jsonl(t.ledger_path()).unwrap();
        assert_eq!(dirty_mark(&on_disk), None, "reconciled → clean, on disk");
        assert!(expected_native_digest(&on_disk).is_some(), "reconcile recorded the file's digest");
        assert!(
            on_disk.events().iter().any(|e| e.kind == Kind::TurnEnded && e.turn.as_deref() == Some("c-1")),
            "a turn the wire never closed is closed by the daemon"
        );
        // The next turn resumes: the prompt renders under the daemon's id now.
        let t2 = ProjectedTurn::begin(inputs(tmp.path(), &home, "again", "c-2", &[])).unwrap();
        assert!(t2.resumable());
        assert_eq!(t2.rendered.lines, 2, "the prompt and the reply");
    }

    /// **PLACEMENT REFUSES A DIRTY THREAD AND A MOVED FILE, AND NOTHING ELSE.**
    ///
    /// **THE MUTATION.** In `check_placement`, replace `found != expected`
    /// with `false`.
    ///
    /// Forced red:
    /// ```text
    /// a native file whose digest moved since the ledger last looked is refused at placement
    /// ```
    #[test]
    fn placement_refuses_dirty_and_moved_and_passes_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let lp = ledger_path(tmp.path(), "t-proj");
        let np = native_path(None, &home, "/w/x", "t-proj");
        assert_eq!(check_placement("t-proj", &lp, &np, "cc2"), Ok(()), "never projected → placed as today");
        let mut t = ProjectedTurn::begin(inputs(tmp.path(), &home, "hello", "c-1", &[])).unwrap();
        match check_placement("t-proj", &lp, &np, "cc2") {
            Err(PlacementRefused::Dirty(d)) => assert_eq!(d.since_turn, "c-1"),
            other => panic!("a thread mid-turn is dirty: {other:?}"),
        }
        std::fs::create_dir_all(np.parent().unwrap()).unwrap();
        std::fs::write(&np, "{\"type\":\"user\",\"uuid\":\"p1\",\"timestamp\":\"2026-10-10T00:00:01Z\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n").unwrap();
        t.end(Some(0), "2026-10-10T00:00:03Z").unwrap();
        assert_eq!(check_placement("t-proj", &lp, &np, "cc2"), Ok(()), "reconciled → placed");
        // Someone typed into the file after the reconcile.
        let mut f = std::fs::OpenOptions::new().append(true).open(&np).unwrap();
        use std::io::Write;
        f.write_all(b"{\"type\":\"user\",\"uuid\":\"ext\",\"message\":{\"role\":\"user\",\"content\":\"typed by hand\"}}\n").unwrap();
        drop(f);
        match check_placement("t-proj", &lp, &np, "cc2") {
            Err(PlacementRefused::NativeMoved { path, .. }) => assert_eq!(path, np),
            other => panic!("a native file whose digest moved since the ledger last looked is refused at placement: {other:?}"),
        }
        // Gone entirely: nothing the ledger lacks is in it.
        std::fs::remove_file(&np).unwrap();
        assert_eq!(check_placement("t-proj", &lp, &np, "cc2"), Ok(()));
    }

    /// **A LOSS THE THREAD DOES NOT ALLOW REFUSES THE TURN, TYPED; ALLOWING IT
    /// BY NAME LETS THE TURN RUN; A NATIVE CONVERSATION WITH NO LEDGER REFUSES.**
    ///
    /// **THE MUTATION.** In `begin`, drop the `!ACCEPTED_LOSSES.contains(loss)`
    /// loop (never refuse a loss).
    ///
    /// Forced red:
    /// ```text
    /// a render that would lose a prompt's text is refused unless the thread allows it by name
    /// ```
    #[test]
    fn a_loss_not_allowed_refuses_typed_and_a_native_without_ledger_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        // A ledger holding a user event with no text: the one loss not ruled accepted.
        let lp = ledger_path(tmp.path(), "t-proj");
        std::fs::create_dir_all(lp.parent().unwrap()).unwrap();
        let mut l = Ledger::new(ThreadMeta::root("t-proj", RUNTIME, "2026-10-10T00:00:00Z"));
        l.append(Draft::new(Kind::User, "2026-10-10T00:00:01Z", json!({"text": null})).with_id("u0")).unwrap();
        l.write_jsonl(&lp).unwrap();
        // A match, not `unwrap_err`: the forced red must show THIS message, not
        // `unwrap_err` on an `Ok`.
        let err = match ProjectedTurn::begin(inputs(tmp.path(), &home, "hello", "c-1", &[])) {
            Err(e) => e,
            Ok(t) => panic!(
                "a render that would lose a prompt's text is refused unless the thread allows it \
                 by name; it rendered {} line(s) with losses {:?}",
                t.rendered.lines, t.rendered.losses
            ),
        };
        assert_eq!(
            err,
            ProjectionRefused::LossNotAllowed {
                loss: "prompt_text_unrecoverable".into(),
                count: 1,
                allowed: vec![]
            },
            "…and the refusal names the loss and the count"
        );
        assert!(err.to_string().contains("prompt_text_unrecoverable"));
        let allow = vec!["prompt_text_unrecoverable".to_string()];
        let t = ProjectedTurn::begin(inputs(tmp.path(), &home, "hello", "c-1", &allow)).unwrap();
        assert_eq!(t.rendered.losses.get("prompt_text_unrecoverable"), Some(&1), "…and allowed, it renders and counts the loss");

        // A second thread with a native conversation and no ledger.
        let np = native_path(None, &home, "/w/x", "t-native");
        std::fs::create_dir_all(np.parent().unwrap()).unwrap();
        std::fs::write(&np, "{\"type\":\"user\",\"uuid\":\"p\",\"message\":{\"content\":\"old\"}}\n").unwrap();
        let mut i = inputs(tmp.path(), &home, "hello", "c-9", &[]);
        i.thread_id = "t-native";
        match ProjectedTurn::begin(i) {
            Err(ProjectionRefused::NativeWithoutLedger { native, bytes }) => {
                assert_eq!(native, np);
                assert!(bytes > 0);
            }
            other => panic!("a native conversation with no ledger must not be overwritten: {other:?}"),
        }
        assert!(std::fs::metadata(&np).unwrap().len() > 0, "and the file is untouched");
    }

    /// **A LEDGER THAT HAS NEVER RENDERED IS NO LEDGER, FOR THE OVERWRITE
    /// REFUSAL.** R4.S2's managed runtime writes a parent's STRUCTURE (child
    /// edges, reports) to the same `ledgers/<thread>.jsonl`. A thread with
    /// such a ledger and a native conversation, flipped, would have rendered
    /// zero conversation lines over the only copy of it.
    ///
    /// **THE MUTATION.** In `begin`, delete `refuse_if_native(&i)?` from the
    /// ledger-exists arm.
    ///
    /// Forced red:
    /// ```text
    /// a structure-only ledger does not license overwriting a native conversation
    /// ```
    #[test]
    fn a_ledger_that_never_rendered_does_not_license_an_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let np = native_path(None, &home, "/w/x", "t-parent");
        std::fs::create_dir_all(np.parent().unwrap()).unwrap();
        let native = "{\"type\":\"user\",\"uuid\":\"p\",\"message\":{\"content\":\"his only copy\"}}\n";
        std::fs::write(&np, native).unwrap();
        let write = |projected: bool| {
            let lp = ledger_path(tmp.path(), "t-parent");
            std::fs::create_dir_all(lp.parent().unwrap()).unwrap();
            let mut l = Ledger::new(ThreadMeta::root("t-parent", RUNTIME, "2026-10-10T00:00:00Z"));
            l.append(Draft::new(Kind::Child, "2026-10-10T00:00:01Z", json!({"child": "t-kid"})).with_id("edge"))
                .unwrap();
            if projected {
                l.append(
                    Draft::new(Kind::System, "2026-10-10T00:00:02Z", json!({"what": "projected", "detail": {"lines": 0}}))
                        .with_id("proj"),
                )
                .unwrap();
                l.append(
                    Draft::new(Kind::System, "2026-10-10T00:00:03Z", json!({"what": "reconciled", "detail": {}}))
                        .with_id("rec"),
                )
                .unwrap();
            }
            std::fs::remove_file(&lp).ok();
            l.write_jsonl(&lp).unwrap();
        };
        let mut i = inputs(tmp.path(), &home, "hello", "c-1", &[]);
        i.thread_id = "t-parent";
        write(false);
        match ProjectedTurn::begin(i) {
            Err(ProjectionRefused::NativeWithoutLedger { native: n, .. }) => assert_eq!(n, np),
            other => panic!("a structure-only ledger does not license overwriting a native conversation: {other:?}"),
        }
        assert_eq!(std::fs::read_to_string(&np).unwrap(), native, "and the file is untouched");
        // Opposed: once the ledger HAS rendered (it owns the file), the
        // refusal does not fire — a constant refusal fails here.
        write(true);
        let mut i = inputs(tmp.path(), &home, "hello", "c-2", &[]);
        i.thread_id = "t-parent";
        let r = ProjectedTurn::begin(i);
        assert!(
            !matches!(r, Err(ProjectionRefused::NativeWithoutLedger { .. })),
            "a ledger that has rendered owns the native file: {:?}",
            r.err()
        );
    }

    /// **A TORN FOREIGN TAIL IS CUT ON THE PROJECTED PATH, AND A WHOLE
    /// FOREIGN LINE IS KEPT** (R4.S4b, HOLDS R18). A managed append killed
    /// mid-line leaves bytes with no newline. Before the repair was reachable
    /// from here, `persist`'s re-read refused the file (the turn's writes were
    /// logged and lost) and the NEXT turn's `begin` parked `LedgerUnreadable`
    /// until `Managed::load` happened to run.
    ///
    /// Opposition: the complete foreign line written before the torn one must
    /// survive (a repair that truncates to the turn's own last write fails),
    /// and the turn's own event must land after it.
    ///
    /// **THE MUTATION.** Delete the `repair_torn_tail` call in `persist` (the
    /// first half) or in `begin` (the second half).
    ///
    /// Forced red:
    /// ```text
    /// the projected re-read cuts a torn foreign tail and keeps writing
    /// a torn tail does not park the next turn
    /// ```
    #[test]
    fn a_torn_foreign_tail_is_cut_on_the_projected_path_and_a_whole_line_is_kept() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let mut t = ProjectedTurn::begin(inputs(tmp.path(), &home, "hello", "c-torn", &[])).unwrap();
        let path = t.ledger_path().to_path_buf();
        // The other writer: one whole child edge, then a kill mid-append.
        let mut l = Ledger::read_jsonl(&path).unwrap();
        l.append(Draft::new(Kind::Child, "2026-10-10T00:00:01Z", json!({"phase": "started", "child_id": "c-whole"})))
            .unwrap();
        Ledger::append_to_file(&path, l.events().last().unwrap()).unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"seq\":99,\"kind\":\"chi").unwrap();
        drop(f);
        assert!(Ledger::read_jsonl(&path).is_err(), "sentinel: the torn file is unreadable as it stands");
        t.observe(
            &json!({"type": "assistant", "uuid": "a-after", "timestamp": "2026-10-10T00:00:02Z",
                    "message": {"id": "m-after", "model": "x", "content": [{"type": "text", "text": "hi"}]}}),
            "2026-10-10T00:00:02Z",
        );
        let on_disk = Ledger::read_jsonl(&path)
            .unwrap_or_else(|e| panic!("the projected re-read cuts a torn foreign tail and keeps writing: {e:?}"));
        let kinds: Vec<Kind> = on_disk.events().iter().map(|e| e.kind).collect();
        let child = kinds.iter().position(|k| *k == Kind::Child).expect("the whole foreign line is kept");
        let mine = kinds.iter().position(|k| *k == Kind::Assistant).expect("the turn's own event landed");
        assert!(child < mine, "in file order: {kinds:?}");

        drop(t);

        // A turn's `begin` meets a torn tail (the kill came between turns) and
        // must not park on it. A fresh thread whose ledger holds only its
        // header and a killed append.
        let tmp2 = tempfile::tempdir().unwrap();
        let path2 = ledger_path(tmp2.path(), "t-proj");
        std::fs::create_dir_all(path2.parent().unwrap()).unwrap();
        Ledger::new(ThreadMeta::root("t-proj", RUNTIME, "2026-10-10T00:00:00Z")).write_jsonl(&path2).unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(&path2).unwrap();
        f.write_all(b"{\"seq\":").unwrap();
        drop(f);
        assert!(Ledger::read_jsonl(&path2).is_err(), "sentinel: unreadable as it stands");
        let t2 = ProjectedTurn::begin(inputs(tmp2.path(), &tmp2.path().join("home"), "again", "c-torn-2", &[]))
            .unwrap_or_else(|e| panic!("a torn tail does not park the next turn: {e}"));
        assert!(Ledger::read_jsonl(t2.ledger_path()).is_ok(), "and the file it leaves is whole");
    }

    /// **TWO WRITERS RACING ON ONE FILE, UNDER THE ONE LOCK.** The managed
    /// runtime's writer (read the file, append one event, append its line —
    /// exactly `Managed::load` + `append_last`, under the shared lock) runs on
    /// another thread while this turn observes wire lines and writes each one
    /// through. The integration test (`tests/projected_turn.rs`,
    /// `a_flipped_parent_that_spawns_mid_turn_keeps_a_readable_ledger`) forces
    /// the interleaving ONE way, deterministically; this one races it, so
    /// the re-read alone (no lock) is not enough to pass: a foreign append
    /// landing between `persist`'s length check and its own append re-uses a
    /// `seq`.
    ///
    /// Opposition: the file must be readable AND hold every event of both
    /// writers (`N` child edges, `N` assistant lines), so dropping either
    /// writer's events fails as surely as corrupting the numbering.
    ///
    /// **THE MUTATION.** Delete the `let _w = lock.lock()…` line in
    /// `ProjectedTurn::persist` (the lock), or replace the re-read block's
    /// condition with `false` (the tail re-read).
    ///
    /// Forced red: see an internal report.
    #[test]
    fn a_racing_second_writer_under_the_lock_never_renumbers_the_file() {
        const N: usize = 300;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let lock = Arc::new(Mutex::new(()));
        let mut i = inputs(tmp.path(), &home, "hello", "c-race", &[]);
        i.write_lock = lock.clone();
        let mut t = ProjectedTurn::begin(i).unwrap();
        let path = t.ledger_path().to_path_buf();
        let other = {
            let (lock, path) = (lock.clone(), path.clone());
            std::thread::spawn(move || {
                for k in 0..N {
                    let _w = lock.lock().unwrap_or_else(|e| e.into_inner());
                    let mut l = Ledger::read_jsonl(&path)
                        .unwrap_or_else(|e| panic!("the other writer reads a whole file before its append {k}: {e}"));
                    l.append(Draft::new(
                        Kind::Child,
                        "2026-10-10T00:00:01Z",
                        json!({"phase": "started", "child_id": format!("c{k}")}),
                    ))
                    .unwrap();
                    Ledger::append_to_file(&path, l.events().last().unwrap()).unwrap();
                }
            })
        };
        for k in 0..N {
            t.observe(
                &json!({"type": "assistant", "uuid": format!("a{k}"), "timestamp": "2026-10-10T00:00:02Z",
                        "message": {"id": format!("m{k}"), "model": "x", "content": [{"type": "text", "text": "hi"}]}}),
                "2026-10-10T00:00:02Z",
            );
        }
        let joined = other.join();
        // One more line after the other writer is done, so the turn's last
        // write is the file's last write and its copy must be the whole file.
        t.observe(
            &json!({"type": "assistant", "uuid": "a-last", "timestamp": "2026-10-10T00:00:03Z",
                    "message": {"id": "m-last", "model": "x", "content": [{"type": "text", "text": "bye"}]}}),
            "2026-10-10T00:00:03Z",
        );
        let l = Ledger::read_jsonl(&path)
            .unwrap_or_else(|e| panic!("the ledger two writers raced on is readable: {e:?} (other writer: {joined:?})"));
        assert!(joined.is_ok(), "the other writer never read a renumbered file");
        let count = |k: Kind| l.events().iter().filter(|e| e.kind == k).count();
        assert_eq!(
            (count(Kind::Child), count(Kind::Assistant)),
            (N, N + 1),
            "every event of BOTH writers is in the file"
        );
        assert_eq!(t.ledger().len(), l.len(), "the turn's copy is the file's, both writers included");
    }
}
