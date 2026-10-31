//! # The shadow (PL-3): every turn, written into protoledger beside `events`
//!
//! `docs/PROTOLEDGER-INTEGRATION.md` rev 3, the PL-3 row of §4.1 and §0R3.3.
//! **Behind [`FLAG`] (`THREADS_PL_SHADOW=1`), default OFF.** With the flag
//! unset the runtime holds no [`Shadow`] at all, so nothing here runs and
//! nothing is written: not a directory, not a ledger, not an index.
//!
//! With the flag set, each turn of a thread does, in this order:
//!
//! 1. **[`Shadow::before_turn`]** (before `claude` is spawned): the thread's
//!    ledger is made to exist (imported from its native transcript the first
//!    time, when there is one, else a bare header); cauldron's own facts go
//!    in through PL-1's writers — a FULL settings snapshot when it changed,
//!    and the composed prompt — and the thread is **projected**
//!    (`protoledger::sync::project`, export + bind) or, when its binding is
//!    already active, **re-projected** (`reproject`, between harness runs as
//!    SPEC §11.2 requires).
//! 2. `claude --resume` runs exactly as it does without the flag. Its stdout
//!    stream-json stays what it was: the live display and `events`.
//! 3. **[`Shadow::after_turn`]**: what Claude appended to its transcript this
//!    turn is carried onto the bound location ([`carry`]), and the bindings
//!    are followed by a [`Follower`] (`step(deadline)`), which appends what
//!    the location gained, once. The turn's conversation rows in `events`
//!    are then compared with the ledger's ACTIVE PATH for the same turn
//!    ([`conversation_drift`]).
//!
//! **`events` stays the truth.** Every error here is LOGGED and COUNTED
//! ([`ShadowStats`]) and never reaches the turn: the runtime calls only the
//! non-failing wrappers [`Shadow::run_before`] / [`Shadow::run_after`]. A
//! binding sync finds **diverged** is a counted shadow failure too (rev 3),
//! never fatal; the next turn projects the thread again, into a new
//! generation directory.
//!
//! **Where the projection lives, and why not in Claude's own project dir.**
//! The bound location is `<store>/projections/<thread>/g<n>/<thread>.jsonl`,
//! inside the store (so the rollback `rm -r store/protoledger/` removes it).
//! Projecting into the account's real project dir would make protoledger's
//! render the file `claude --resume` reads; until PL-5 backfills every
//! thread, a ledger can be shorter than the native transcript, and
//! "`events` stays the truth" forbids the shadow from changing what the
//! model is shown. So Claude keeps writing its own file, and [`carry`] copies
//! the lines it appended onto the projection, where sync reads them as the
//! harness's appends (`PL-MEANWHILE(PL-5 backfill)`: the carry goes when the
//! projection becomes the file Claude resumes, PL-9a).

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use protoledger::sync::{self, FollowOptions, Follower};
use protoledger::{Event, Kind};
use serde_json::{Map, Value};

use super::store::PlStore;
use super::write::{Prompt, SettingsSnapshot};
use crate::error::{Error, Result};

/// The switch. `1` turns the shadow on; anything else (or unset) is off.
pub const FLAG: &str = "THREADS_PL_SHADOW";
/// The harness format every projection is written in.
pub const FORMAT: &str = "claude-code";
/// Under the store root: one directory per thread, one generation per binding.
pub const PROJECTIONS_DIR: &str = "projections";

fn pl<E: std::fmt::Display>(e: E) -> Error {
    Error::Store(format!("protoledger shadow: {e:#}"))
}

/// Is the flag set? Only the exact value `1` counts.
pub fn enabled(value: Option<&str>) -> bool {
    value == Some("1")
}

/// What the shadow has done since the daemon started.
#[derive(Debug, Default)]
pub struct ShadowStats {
    turns: AtomicU64,
    failures: AtomicU64,
    diverged: AtomicU64,
    drift: AtomicU64,
}

/// A read of [`ShadowStats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    /// Turns whose after-step completed without error.
    pub turns: u64,
    /// Every shadow failure: an error in either step, a diverged binding, a drift.
    pub failures: u64,
    /// Bindings a sync found diverged (each also counted in `failures`).
    pub diverged: u64,
    /// Turns whose `events` conversation rows differed from the ledger's
    /// active path (each also counted in `failures`).
    pub drift: u64,
}

impl ShadowStats {
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            turns: self.turns.load(Ordering::SeqCst),
            failures: self.failures.load(Ordering::SeqCst),
            diverged: self.diverged.load(Ordering::SeqCst),
            drift: self.drift.load(Ordering::SeqCst),
        }
    }
}

/// One conversation row, in the terms both sides share. Built from `events`
/// by the runtime and from the ledger by [`ledger_conversation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conv {
    AssistantText(String),
    Reasoning(String),
    ToolCall { id: String, name: String },
    ToolResult { id: String },
}

/// What the runtime hands [`Shadow::before_turn`].
#[derive(Debug, Clone)]
pub struct TurnStart {
    pub thread_id: String,
    pub ts: String,
    pub prompt: Prompt,
    pub settings: SettingsSnapshot,
    /// The thread's native transcript, when one exists before the turn.
    pub native: Option<PathBuf>,
}

/// What [`Shadow::after_turn`] found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnReport {
    /// Bytes carried from the native transcript onto the projection.
    pub carried: usize,
    /// Events the follower appended to this thread's ledger.
    pub appended: usize,
    /// A binding the follower found diverged, with the reason.
    pub diverged: Vec<String>,
    /// `Some(why)` when the conversation rows and the active path differ.
    pub drift: Option<String>,
}

struct Inner {
    store: PlStore,
    follower: Follower,
    /// The last snapshot written per thread (in this process): a snapshot is
    /// written when the settings CHANGE, and always on a thread's first turn
    /// after a start.
    last_settings: HashMap<String, SettingsSnapshot>,
}

/// The shadow writer of one daemon (one data dir).
pub struct Shadow {
    data_dir: PathBuf,
    /// Opened on first use, so a store that cannot be opened is a counted
    /// failure of a turn, never of the daemon's start.
    inner: Mutex<Option<Inner>>,
    stats: ShadowStats,
}

impl Shadow {
    /// The shadow for `data_dir` when `flag` (the value of [`FLAG`]) turns it
    /// on; `None` otherwise, and then nothing is ever written.
    pub fn from_flag(flag: Option<&str>, data_dir: &Path) -> Option<Shadow> {
        enabled(flag).then(|| Shadow::new(data_dir))
    }

    /// [`Self::from_flag`] over this process's environment.
    pub fn from_env(data_dir: &Path) -> Option<Shadow> {
        Shadow::from_flag(std::env::var(FLAG).ok().as_deref(), data_dir)
    }

    pub fn new(data_dir: &Path) -> Shadow {
        Shadow { data_dir: data_dir.to_path_buf(), inner: Mutex::new(None), stats: ShadowStats::default() }
    }

    pub fn stats(&self) -> StatsSnapshot {
        self.stats.snapshot()
    }

    fn fail(&self, thread_id: &str, step: &str, e: &Error) {
        self.stats.failures.fetch_add(1, Ordering::SeqCst);
        tracing::warn!(thread = %thread_id, step, error = %e, "protoledger shadow failed (counted; the turn is unaffected)");
    }

    /// [`Self::before_turn`], never failing: an error and a diverged binding
    /// are logged and counted. `true` when the step completed.
    pub fn run_before(&self, t: &TurnStart) -> bool {
        match self.before_turn(t) {
            Ok(diverged) => {
                for why in &diverged {
                    self.diverged(&t.thread_id, why);
                }
                true
            }
            Err(e) => {
                self.fail(&t.thread_id, "before_turn", &e);
                false
            }
        }
    }

    fn diverged(&self, thread_id: &str, why: &str) {
        self.stats.diverged.fetch_add(1, Ordering::SeqCst);
        self.stats.failures.fetch_add(1, Ordering::SeqCst);
        tracing::warn!(thread = %thread_id, reason = %why, "protoledger shadow: a binding DIVERGED (counted; the thread is projected again)");
    }

    /// [`Self::after_turn`], never failing: errors, a diverged binding and a
    /// drift are logged and counted.
    pub fn run_after(&self, thread_id: &str, native: Option<&Path>, conversation: &[Conv]) -> Option<TurnReport> {
        match self.after_turn(thread_id, native, conversation) {
            Ok(r) => {
                self.stats.turns.fetch_add(1, Ordering::SeqCst);
                for why in &r.diverged {
                    self.diverged(thread_id, why);
                }
                if let Some(why) = &r.drift {
                    self.stats.drift.fetch_add(1, Ordering::SeqCst);
                    self.stats.failures.fetch_add(1, Ordering::SeqCst);
                    tracing::warn!(thread = %thread_id, drift = %why, "protoledger shadow: the ledger's active path differs from events (counted)");
                }
                Some(r)
            }
            Err(e) => {
                self.fail(thread_id, "after_turn", &e);
                None
            }
        }
    }

    fn with_inner<T>(&self, f: impl FnOnce(&mut Inner) -> Result<T>) -> Result<T> {
        let mut g = self.inner.lock().map_err(|_| Error::Store("protoledger shadow: lock poisoned".into()))?;
        if g.is_none() {
            let store = PlStore::open_in(&self.data_dir)?;
            let opts = FollowOptions { debounce: std::time::Duration::ZERO, poll: std::time::Duration::ZERO, watcher: false };
            let follower = Follower::new(store.store(), opts).map_err(pl)?;
            *g = Some(Inner { store, follower, last_settings: HashMap::new() });
        }
        f(g.as_mut().expect("opened above"))
    }

    /// Step 1: ledger, cauldron's facts, projection. Returns the bindings
    /// found diverged on the way (the thread is then projected afresh).
    pub fn before_turn(&self, t: &TurnStart) -> Result<Vec<String>> {
        self.with_inner(|inner| {
            let id = t.thread_id.as_str();
            if !inner.store.has_thread(id)? {
                if let Some(native) = t.native.as_deref().filter(|p| p.is_file()) {
                    inner.store.store_mut().import(FORMAT, native).map_err(pl)?;
                }
                if !inner.store.has_thread(id)? {
                    inner.store.create_thread(id, &t.ts, Map::new())?;
                }
            }
            if inner.last_settings.get(id) != Some(&t.settings) {
                inner.store.write_settings(id, &t.ts, &t.settings)?;
                inner.last_settings.insert(id.to_string(), t.settings.clone());
            }
            inner.store.write_prompt(id, &t.ts, &t.prompt)?;
            // What the location gained since the last turn, first: a location
            // changed in a way that is not an append is DIVERGED here, before
            // a re-projection would refuse over it.
            let mut diverged = Vec::new();
            if active_binding(&inner.store, id)?.is_some() {
                let Inner { store, follower, .. } = &mut *inner;
                for (_, s) in follower.step(store.store_mut(), Instant::now()).map_err(pl)? {
                    if let Some(why) = &s.diverged {
                        diverged.push(format!("binding {}: {why}", s.binding));
                    }
                }
            }
            match active_binding(&inner.store, id)? {
                Some(b) => {
                    sync::reproject(inner.store.store_mut(), &b).map_err(pl)?;
                }
                None => {
                    let generation = inner.store.store().bindings().map_err(pl)?.into_iter().filter(|b| b.agent == id).count();
                    let dir = inner.store.root().join(PROJECTIONS_DIR).join(id).join(format!("g{generation}"));
                    std::fs::create_dir_all(&dir).map_err(|e| Error::Store(format!("creating {}: {e}", dir.display())))?;
                    sync::project(inner.store.store_mut(), id, FORMAT, &dir).map_err(pl)?;
                }
            }
            Ok(diverged)
        })
    }

    /// Step 3: carry, follow, compare.
    pub fn after_turn(&self, thread_id: &str, native: Option<&Path>, conversation: &[Conv]) -> Result<TurnReport> {
        self.with_inner(|inner| {
            let mut report = TurnReport::default();
            let loc = active_location(&inner.store, thread_id)?
                .ok_or_else(|| Error::Store(format!("protoledger shadow: {thread_id} has no active projection")))?;
            if let Some(native) = native.filter(|p| p.is_file()) {
                let real = std::fs::read_to_string(native).map_err(|e| Error::Store(format!("reading {}: {e}", native.display())))?;
                let shadow = std::fs::read_to_string(&loc).map_err(|e| Error::Store(format!("reading {}: {e}", loc.display())))?;
                let add = carry(&shadow, &real);
                if !add.is_empty() {
                    let mut f = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&loc)
                        .map_err(|e| Error::Store(format!("opening {}: {e}", loc.display())))?;
                    let mut bytes = String::new();
                    if !shadow.is_empty() && !shadow.ends_with('\n') {
                        bytes.push('\n');
                    }
                    bytes.push_str(&add);
                    f.write_all(bytes.as_bytes()).and_then(|_| f.sync_data()).map_err(|e| Error::Store(format!("carrying onto {}: {e}", loc.display())))?;
                    report.carried = bytes.len();
                }
            }
            let Inner { store, follower, .. } = inner;
            let steps = follower.step(store.store_mut(), Instant::now()).map_err(pl)?;
            let mut appended_ids: HashSet<String> = HashSet::new();
            for (_, s) in &steps {
                if let Some(why) = &s.diverged {
                    report.diverged.push(format!("binding {}: {why}", s.binding));
                }
                for p in s.appended.iter().filter(|p| p.agent == thread_id) {
                    appended_ids.insert(p.event.id.clone());
                }
            }
            report.appended = appended_ids.len();
            let active: Vec<Event> =
                store.store_mut().held(thread_id).map_err(pl)?.active_path().into_iter().cloned().collect();
            let synced: Vec<Event> = active.into_iter().filter(|e| appended_ids.contains(&e.id)).collect();
            report.drift = conversation_drift(conversation, &ledger_conversation(&synced));
            Ok(report)
        })
    }
}

/// The thread's active projection binding, if any.
fn active_binding(store: &PlStore, thread_id: &str) -> Result<Option<String>> {
    Ok(store
        .store()
        .bindings()
        .map_err(pl)?
        .into_iter()
        .find(|b| b.agent == thread_id && b.active && b.mode == "projection")
        .map(|b| b.binding))
}

/// Where the thread's active projection binding is.
pub fn active_location(store: &PlStore, thread_id: &str) -> Result<Option<PathBuf>> {
    Ok(store
        .store()
        .bindings()
        .map_err(pl)?
        .into_iter()
        .find(|b| b.agent == thread_id && b.active && b.mode == "projection")
        .map(|b| PathBuf::from(b.location)))
}

fn uuid_of(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    v.get("uuid").and_then(Value::as_str).map(str::to_string)
}

/// **THE CARRY.** The lines of the native transcript `real` that the
/// projection `shadow` lacks, as text to append (each line `\n`-terminated).
///
/// Measured by RECORD, not by byte offset, because cauldron itself rewrites
/// the native file between turns (the API-error scrub rewrites records in
/// place; a failed attempt's tail is rolled back): an offset taken before the
/// turn can point into the middle of a line after it. So: the last line of
/// `real` whose `uuid` the projection holds is where the projection and the
/// transcript last agreed; every complete line after it that the projection
/// does not already hold byte for byte is carried. With no uuid in common
/// (a thread whose ledger began empty), the whole transcript is carried. A
/// torn final line (no `\n` yet) is never carried.
pub fn carry(shadow: &str, real: &str) -> String {
    let known_uuids: HashSet<String> = shadow.lines().filter_map(uuid_of).collect();
    let known_lines: HashSet<&str> = shadow.lines().collect();
    let complete: Vec<&str> = match real.rfind('\n') {
        Some(end) => real[..end].split('\n').collect(),
        None => Vec::new(),
    };
    let start = complete
        .iter()
        .rposition(|l| uuid_of(l).is_some_and(|u| known_uuids.contains(&u)))
        .map(|i| i + 1)
        .unwrap_or(0);
    let mut out = String::new();
    for l in &complete[start..] {
        if l.trim().is_empty() || known_lines.contains(l) {
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// The conversation rows of ledger events (the harness's own, in order).
pub fn ledger_conversation(events: &[Event]) -> Vec<Conv> {
    let mut out = Vec::new();
    for e in events {
        match e.kind {
            Kind::Text if e.str("role") == Some("assistant") => {
                if let Some(t) = e.str("text").filter(|t| !t.is_empty()) {
                    out.push(Conv::AssistantText(t.to_string()));
                }
            }
            Kind::Reasoning => {
                if let Some(t) = e.str("text").filter(|t| !t.is_empty()) {
                    out.push(Conv::Reasoning(t.to_string()));
                }
            }
            Kind::ToolCall => out.push(Conv::ToolCall {
                id: e.str("call_id").unwrap_or_default().to_string(),
                name: e.str("name").unwrap_or_default().to_string(),
            }),
            Kind::ToolResult => out.push(Conv::ToolResult { id: e.str("call_id").unwrap_or_default().to_string() }),
            _ => {}
        }
    }
    out
}

/// `None` when the two agree; otherwise where they first differ.
pub fn conversation_drift(events: &[Conv], ledger: &[Conv]) -> Option<String> {
    if events == ledger {
        return None;
    }
    let at = events.iter().zip(ledger).position(|(a, b)| a != b).unwrap_or(events.len().min(ledger.len()));
    Some(format!(
        "events has {} conversation rows, the ledger's active path {}; first difference at {at}: events {:?} vs ledger {:?}",
        events.len(),
        ledger.len(),
        events.get(at),
        ledger.get(at)
    ))
}

/// A ledger FILE's active path, read with protoledger's plain file reader (no
/// store, no index, no lock): for checks that must not go through the
/// shadow's own store handle.
pub fn read_active_path(ledger_file: &Path) -> Result<Vec<Event>> {
    let l = protoledger::Ledger::read(ledger_file).map_err(pl)?;
    Ok(l.active_path().into_iter().cloned().collect())
}

/// A ledger FILE's events, all of them, in file order (same reader).
pub fn read_all(ledger_file: &Path) -> Result<Vec<Event>> {
    Ok(protoledger::Ledger::read(ledger_file).map_err(pl)?.events)
}
