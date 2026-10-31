//! # The periodic subagent health invariant (R10.H2, TC-13; HEALTH F3)
//!
//! an internal report (substrate `interface-layer`) §"F3 — Periodic
//! health invariant": every [`PERIOD`] (5 min) the daemon checks five
//! invariants over its own store, ledgers and the process table, and warns on
//! the `warnings` shape for each one that is violated, naming the ids so a
//! sweep (F1) and a person can both act on it.
//!
//! | code | violated when | the ids it names |
//! |---|---|---|
//! | `I1` | a thread reads `executing` but no live pid runs its turn; or a live claude-like process whose session is one of this store's threads, while that thread is not executing | `thread=…` / `thread=… pid=…` |
//! | `I2` | a parent's `outstanding` holds a child that is idle, has `queued == 0`, and whose run ended more than [`STUCK_WAIT`] ago — a stuck wait | `parent=… child=…` |
//! | `I3` | a `child{started}` with no `child{finished}` while the child has been idle for more than [`IDLE_CHILD`], any wake mode (D1) | `parent=… child=…` |
//! | `I4` | a thread's newest roster (`background_tasks_changed`) still lists a direct agent (`local_agent`) with no `task_notification` after it, and the thread's last turn ended more than [`ROSTER_AFTER_END`] ago (D2) | `thread=… task=…` |
//! | `I5` | a claude-like process with ppid 1 whose `--session-id` / `--resume` names no thread of this store (the leaked fixtures) | `pid=… session=…` |
//!
//! ## How a violation reaches the `warnings` shape, and what "cleared" can mean there
//!
//! The daemon's `warnings` shape ([`crate::shapes::warnings`]) is a COUNT of
//! `WARN`/`ERROR` events by call site, fed by the [`crate::warnings`] tracing
//! layer. So each invariant has its OWN `tracing::warn!` call site below
//! ([`Health::apply`]), and each is one `kind` on the shape. A warning is
//! emitted when an invariant goes from holding to violated, and again when a
//! violated invariant gains an id it had not named — never once per period
//! for the same standing violation, which would make the count a clock. The
//! warning names ONLY the new ids, and an id is forgotten (so its return warns
//! again) only after [`FORGET_AFTER_CLEAN_PASSES`] clean passes without it
//! (WARN-1 D-D; see [`Health::apply`]).
//!
//! The shape has no way to UN-count a warning. "Cleared" is therefore carried
//! where it can be: the standing set ([`Health::standing`]) drops the
//! invariant, and an `INFO` line (`health: … holds again`) records it. A
//! consumer-visible "currently violated" cell would need a change to the shape
//! itself (`shapes/warnings.rs`), which this step does not make; see the R10.H2
//! report.
//!
//! ## Read-only, and never the close lock
//!
//! The check reads ledgers with [`Ledger::read_jsonl`] directly — NOT
//! [`crate::managed::Managed::load`], which cuts a torn tail (a write). A
//! ledger caught mid-append fails to parse and is skipped for that pass. It
//! takes neither the managed `close` lock nor the `ledgers/` write lock; every
//! store access is a read. It runs off the reactor (`spawn_blocking`).
//!
//! ## Injectable, for doctrine 4
//!
//! The period and every age threshold live in [`Limits`], settable on a live
//! [`Health`] ([`Health::set_limits`] restarts the wait). The process table is
//! a function ([`ProcSource`]); production reads `/bin/ps`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::event::ThreadEvent;
use crate::ledger::{Kind, Ledger};
use crate::runtime::ThreadManager;

/// How often the check runs in production.
pub const PERIOD: Duration = Duration::from_secs(5 * 60);
/// I2: how long ago an outstanding child's run must have ended.
pub const STUCK_WAIT: Duration = Duration::from_secs(2 * 60);
/// I3: how long an unfinished child must have been idle.
pub const IDLE_CHILD: Duration = Duration::from_secs(10 * 60);
/// I4: how long ago the thread's last turn must have ended.
pub const ROSTER_AFTER_END: Duration = Duration::from_secs(10 * 60);
/// I1: the dispatch window. A lock is taken before `claude` is spawned and a
/// process outlives its turn end by a moment; neither is a violation inside
/// this grace.
pub const GRACE: Duration = Duration::from_secs(60);

/// The `child` event fact a managed child's completion record carries
/// (`managed::RUN_ENDED`, private there).
const RUN_ENDED: &str = "run_ended";

/// The five invariants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Invariant {
    I1,
    I2,
    I3,
    I4,
    I5,
}

impl Invariant {
    pub const ALL: [Invariant; 5] = [Invariant::I1, Invariant::I2, Invariant::I3, Invariant::I4, Invariant::I5];

    pub fn code(self) -> &'static str {
        match self {
            Invariant::I1 => "I1",
            Invariant::I2 => "I2",
            Invariant::I3 => "I3",
            Invariant::I4 => "I4",
            Invariant::I5 => "I5",
        }
    }

    pub fn what(self) -> &'static str {
        match self {
            Invariant::I1 => "executing without a live pid, or a live claude no executing thread owns",
            Invariant::I2 => "a parent waits on an idle child whose run ended (stuck wait)",
            Invariant::I3 => "a started child never recorded finished, idle past the bound (D1)",
            Invariant::I4 => "a roster still lists a direct agent with no task_notification after the turn ended (D2)",
            Invariant::I5 => "an orphaned (ppid 1) claude holds a session that is no thread",
        }
    }
}

/// The period and the age bounds. [`Limits::default`] is production.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub period: Duration,
    pub stuck_wait: Duration,
    pub idle_child: Duration,
    pub roster_after_end: Duration,
    pub grace: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { period: PERIOD, stuck_wait: STUCK_WAIT, idle_child: IDLE_CHILD, roster_after_end: ROSTER_AFTER_END, grace: GRACE }
    }
}

/// One row of the process table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: i64,
    pub ppid: i64,
    /// Seconds since the process started.
    pub age_s: u64,
    /// The command line, whitespace-joined as `ps` prints it.
    pub args: String,
}

impl Proc {
    /// **CLAUDE-LIKE:** the basename of the program, or of the script a shell
    /// or interpreter runs (`argv[1]`, the `stall-claude` fixture shape),
    /// contains `claude`. A `-c` script body never counts: an operator's
    /// shell whose command TEXT mentions claude is not a claude.
    pub fn claude_like(&self) -> bool {
        let mut it = self.args.split_whitespace();
        let base = |s: &str| s.rsplit('/').next().unwrap_or(s).to_string();
        let a0 = it.next().map(base).unwrap_or_default();
        if a0.contains("claude") {
            return true;
        }
        match it.next() {
            Some(a1) if !a1.starts_with('-') => base(a1).contains("claude"),
            _ => false,
        }
    }

    /// The session a claude-like process holds: the token after
    /// `--session-id` (first turn) or `--resume` (every later turn — the
    /// daemon passes the thread id to either, `spawn.rs`).
    pub fn session(&self) -> Option<String> {
        let toks: Vec<&str> = self.args.split_whitespace().collect();
        toks.windows(2)
            .find(|w| w[0] == "--session-id" || w[0] == "--resume")
            .map(|w| w[1].to_string())
            .or_else(|| {
                toks.iter()
                    .find_map(|t| t.strip_prefix("--session-id=").or_else(|| t.strip_prefix("--resume=")))
                    .map(str::to_string)
            })
    }
}

/// Where the process table comes from.
pub type ProcSource = Arc<dyn Fn() -> std::io::Result<Vec<Proc>> + Send + Sync>;

/// The production source: `/bin/ps -axo pid=,ppid=,etime=,args=`.
pub fn ps_table() -> std::io::Result<Vec<Proc>> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-axww", "-o", "pid=,ppid=,etime=,args="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!("ps exited {}", out.status)));
    }
    Ok(parse_ps(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse `pid ppid etime args…` lines; a line that does not parse is skipped.
pub fn parse_ps(text: &str) -> Vec<Proc> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let age_s = parse_etime(it.next()?)?;
            let args = it.collect::<Vec<_>>().join(" ");
            Some(Proc { pid, ppid, age_s, args })
        })
        .collect()
}

/// `ps` `etime`: `[[dd-]hh:]mm:ss`.
pub fn parse_etime(s: &str) -> Option<u64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let parts: Vec<u64> = rest.split(':').map(|p| p.parse().ok()).collect::<Option<_>>()?;
    let (h, m, sec) = match parts.as_slice() {
        [m, s] => (0, *m, *s),
        [h, m, s] => (*h, *m, *s),
        _ => return None,
    };
    Some(days * 86_400 + h * 3_600 + m * 60 + sec)
}

/// One pass's findings: per invariant, the ids in violation.
///
/// **An id is STABLE across passes** — it names the threads, never an age.
/// [`Health::apply`] warns when an id is new, so an id carrying "ended 2s ago"
/// would be a new id every second and re-warn a standing violation forever
/// (found by the full val, `R10.H2-val.log`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub violations: BTreeMap<Invariant, BTreeSet<String>>,
    /// Ledgers that failed to parse this pass (e.g. caught mid-append).
    pub skipped_ledgers: usize,
    /// The process table could not be read this pass, so I1's pid half and I5
    /// saw nothing (WARN-1 D-D: such a pass is DEGRADED and cannot clear an id).
    pub procs_failed: bool,
}

impl Report {
    fn add(&mut self, inv: Invariant, id: String) {
        self.violations.entry(inv).or_default().insert(id);
    }

    pub fn ids(&self, inv: Invariant) -> Vec<String> {
        self.violations.get(&inv).map(|s| s.iter().cloned().collect()).unwrap_or_default()
    }

    /// **DEGRADED:** part of the evidence was unreadable this pass (a ledger
    /// caught mid-append, or `ps` failed), so an id's ABSENCE proves nothing and
    /// may not count toward forgetting it (WARN-1 D-D).
    pub fn degraded(&self) -> bool {
        self.skipped_ledgers > 0 || self.procs_failed
    }
}

fn age(ts: &str) -> Option<Duration> {
    let at = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    crate::clock::now().signed_duration_since(at.with_timezone(&chrono::Utc)).to_std().ok().or(Some(Duration::ZERO))
}

/// The newest turn end (`turn_ended` / `turn_failed` / `turn_interrupted`)
/// of a thread: `(seq, ts)`.
fn newest_turn_end(m: &ThreadManager, id: &str) -> crate::Result<Option<(u64, String)>> {
    let mut newest: Option<(u64, String)> = None;
    for kind in ["turn_ended", "turn_failed", "turn_interrupted"] {
        if let Some(e) = m.store.newest_of_kind(id, kind, false)? {
            if newest.as_ref().is_none_or(|n| e.seq > n.0) {
                newest = Some((e.seq, e.ts));
            }
        }
    }
    Ok(newest)
}

/// The same "idle" `managed` decides a run's end by: nothing executing or
/// waiting, no armed retry, no parked turn.
fn idle(m: &ThreadManager, id: &str) -> crate::Result<bool> {
    Ok(m.is_idle(id)? && m.store.get_retry(id)?.is_none() && m.store.get_parked_turn(id)?.is_none())
}

/// Every ledger in `dir`, read WITHOUT repair. Unparseable ones are counted.
fn ledgers(dir: &Path, skipped: &mut usize) -> Vec<Ledger> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut paths: Vec<_> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| match Ledger::read_jsonl(&p) {
            Ok(l) => Some(l),
            Err(_) => {
                *skipped += 1;
                None
            }
        })
        .collect()
}

/// The child's newest completion record on its own ledger: its `ts`.
fn record_ts(child: &Ledger) -> Option<String> {
    child
        .events()
        .iter()
        .rev()
        .find(|e| e.kind == Kind::System && e.body.get("fact").and_then(Value::as_str) == Some(RUN_ENDED))
        .map(|e| e.ts.clone())
}

fn newest(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(x), Some(y)) => {
            let px = chrono::DateTime::parse_from_rfc3339(&x).ok();
            let py = chrono::DateTime::parse_from_rfc3339(&y).ok();
            if py > px { Some(y) } else { Some(x) }
        }
        (x, y) => x.or(y),
    }
}

/// **ONE PASS.** Read-only; see the module doc.
pub fn check(m: &ThreadManager, limits: &Limits, procs: &[Proc]) -> crate::Result<Report> {
    let mut r = Report::default();
    check_i1(m, limits, procs, &mut r)?;
    check_children(m, limits, &mut r)?;
    check_i4(m, limits, &mut r)?;
    check_i5(m, procs, &mut r)?;
    Ok(r)
}

fn check_i1(m: &ThreadManager, limits: &Limits, procs: &[Proc], r: &mut Report) -> crate::Result<()> {
    let live: BTreeSet<i64> = procs.iter().map(|p| p.pid).collect();
    let locks = m.store.list_thread_locks()?;
    let ids: Vec<String> = locks.iter().map(|l| l.thread_id.clone()).collect();
    let states = m.thread_states(&ids)?;
    let mut executing: BTreeSet<String> = BTreeSet::new();
    for l in &locks {
        if !states.get(&l.thread_id).is_some_and(|d| d.executing) {
            continue;
        }
        executing.insert(l.thread_id.clone());
        if age(&l.acquired_at).is_some_and(|a| a < limits.grace) {
            continue;
        }
        let pid = m.store.get_running_turn(&l.thread_id)?.map(|t| t.pid);
        if !pid.is_some_and(|p| p > 0 && live.contains(&p)) {
            r.add(Invariant::I1, format!("thread={} executing, no live pid (running pid {:?})", l.thread_id, pid));
        }
    }
    let attached: BTreeSet<i64> = m
        .store
        .list_all_attached_sessions()?
        .iter()
        .flat_map(|a| [Some(a.wrapper_pid), a.claude_pid])
        .flatten()
        .collect();
    for p in procs.iter().filter(|p| p.claude_like()) {
        let Some(s) = p.session() else { continue };
        if executing.contains(&s) || attached.contains(&p.pid) || p.age_s < limits.grace.as_secs() {
            continue;
        }
        if m.store.get_thread(&s)?.is_none() {
            continue;
        }
        // Not executing by the lock read above; ask the derivation itself for
        // a thread with no lock row, and let a turn that has just ended settle.
        if m.thread_state(&s)?.executing {
            continue;
        }
        if newest_turn_end(m, &s)?.is_some_and(|(_, ts)| age(&ts).is_some_and(|a| a < limits.grace)) {
            continue;
        }
        r.add(Invariant::I1, format!("thread={s} pid={} live claude, thread not executing", p.pid));
    }
    Ok(())
}

/// I2 and I3: one walk over the parents' ledgers.
fn check_children(m: &ThreadManager, limits: &Limits, r: &mut Report) -> crate::Result<()> {
    let mut skipped = 0;
    let all = ledgers(m.managed().dir(), &mut skipped);
    r.skipped_ledgers += skipped;
    let by_id: BTreeMap<String, &Ledger> = all.iter().map(|l| (l.thread_id().to_string(), l)).collect();
    for p in &all {
        let mut started_ts: BTreeMap<String, String> = BTreeMap::new();
        for e in p.events() {
            if e.kind == Kind::Child && e.within.is_none() && e.body["phase"] == "started" {
                if let Some(c) = e.body["child_id"].as_str() {
                    started_ts.insert(c.to_string(), e.ts.clone());
                }
            }
        }
        let owed = crate::ledger::spawn::outstanding(p);
        let open = crate::ledger::spawn::unfinished(p);
        for c in open.iter().filter(|c| started_ts.contains_key(*c)) {
            if m.store.get_thread(c)?.is_none() || !idle(m, c)? {
                continue;
            }
            let end = newest_turn_end(m, c)?.map(|(_, ts)| ts);
            let rec = by_id.get(c).and_then(|l| record_ts(l));
            let ended = newest(end, rec);
            if owed.contains(c) && m.thread_state(c)?.queued == 0 {
                if let Some(a) = ended.as_deref().and_then(age) {
                    if a > limits.stuck_wait {
                        r.add(Invariant::I2, format!("parent={} child={c} idle, queued 0, its run ended past the bound", p.thread_id()));
                    }
                }
            }
            let since = newest(ended, started_ts.get(c).cloned());
            if let Some(a) = since.as_deref().and_then(age) {
                if a > limits.idle_child {
                    r.add(Invariant::I3, format!("parent={} child={c} idle past the bound, no child{{finished}}", p.thread_id()));
                }
            }
        }
    }
    Ok(())
}

fn check_i4(m: &ThreadManager, limits: &Limits, r: &mut Report) -> crate::Result<()> {
    for id in m.store.thread_ids(usize::MAX >> 1)? {
        let Some(roster) = m.store.newest_of_kind(&id, "background_tasks_changed", false)? else { continue };
        let ThreadEvent::BackgroundTasksChanged { tasks, .. } = &roster.event else { continue };
        let agents: Vec<String> = tasks
            .iter()
            .filter(|t| t.task_type.as_deref() != Some("local_bash"))
            .filter_map(|t| t.task_id.clone())
            .collect();
        if agents.is_empty() {
            continue;
        }
        let Some((end_seq, end_ts)) = newest_turn_end(m, &id)? else { continue };
        if end_seq < roster.seq || age(&end_ts).is_none_or(|a| a <= limits.roster_after_end) {
            continue;
        }
        let after = m.store.events_from(&id, roster.seq + 1, 100_000)?;
        let notified: BTreeSet<&str> = after
            .iter()
            .filter_map(|e| match &e.event {
                ThreadEvent::TaskNotification { task_id, .. } => Some(task_id.as_str()),
                _ => None,
            })
            .collect();
        for t in agents.iter().filter(|t| !notified.contains(t.as_str())) {
            r.add(Invariant::I4, format!("thread={id} task={t} listed by the newest roster, no task_notification"));
        }
    }
    Ok(())
}

fn check_i5(m: &ThreadManager, procs: &[Proc], r: &mut Report) -> crate::Result<()> {
    for p in procs.iter().filter(|p| p.ppid == 1 && p.claude_like()) {
        let Some(s) = p.session() else { continue };
        if m.store.get_thread(&s)?.is_none() {
            r.add(Invariant::I5, format!("pid={} session={s} orphaned claude, no such thread", p.pid));
        }
    }
    Ok(())
}

/// **THE STANDING STATE AND THE LOOP.** One per daemon (tests make their own).
/// **HYSTERESIS (WARN-1 D-D, census §5 D-D).** An id that has been warned is
/// FORGOTTEN — so that its return warns again — only after it has been absent
/// for this many consecutive CLEAN passes ([`Report::degraded`] is false). A
/// degraded pass neither counts toward forgetting nor resets the count.
pub const FORGET_AFTER_CLEAN_PASSES: u32 = 2;

pub struct Health {
    limits: Mutex<Limits>,
    procs: ProcSource,
    standing: Mutex<BTreeMap<Invariant, BTreeSet<String>>>,
    /// Every id already warned, with how many consecutive CLEAN passes it has
    /// been absent for. Distinct from `standing` (what THIS pass found): an id
    /// stays remembered across a one-pass dropout, so its return is not news.
    warned: Mutex<BTreeMap<Invariant, BTreeMap<String, u32>>>,
    passes: Mutex<u64>,
    kick: tokio::sync::Notify,
}

impl Health {
    /// Production: [`Limits::default`] and [`ps_table`].
    pub fn new() -> Arc<Self> {
        Self::with(Limits::default(), Arc::new(ps_table))
    }

    pub fn with(limits: Limits, procs: ProcSource) -> Arc<Self> {
        Arc::new(Health {
            limits: Mutex::new(limits),
            procs,
            standing: Mutex::new(BTreeMap::new()),
            warned: Mutex::new(BTreeMap::new()),
            passes: Mutex::new(0),
            kick: tokio::sync::Notify::new(),
        })
    }

    /// Change the limits; a waiting loop restarts its wait (no early pass).
    pub fn set_limits(&self, l: Limits) {
        *self.limits.lock().unwrap() = l;
        self.kick.notify_waiters();
    }

    pub fn limits(&self) -> Limits {
        *self.limits.lock().unwrap()
    }

    /// Passes run so far.
    pub fn passes(&self) -> u64 {
        *self.passes.lock().unwrap()
    }

    /// What is violated now, by invariant code, with the ids named.
    pub fn standing(&self) -> BTreeMap<&'static str, Vec<String>> {
        self.standing
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.code(), v.iter().cloned().collect()))
            .collect()
    }

    /// Run one pass now and apply it.
    pub fn pass(&self, m: &ThreadManager) -> crate::Result<Report> {
        let limits = self.limits();
        let mut procs_failed = false;
        let procs = match (self.procs)() {
            Ok(p) => p,
            Err(e) => {
                procs_failed = true;
                tracing::warn!(error = %e, "health: reading the process table failed; I1's pid half and I5 are unchecked this pass");
                Vec::new()
            }
        };
        let mut rep = check(m, &limits, &procs)?;
        rep.procs_failed = procs_failed;
        self.apply(&rep);
        *self.passes.lock().unwrap() += 1;
        Ok(rep)
    }

    /// Fold a report into the standing state: WARN naming ONLY the ids not
    /// already warned (each invariant its own call site, so its own `kind` on
    /// the shape), INFO when it holds again, INFO when the standing count moves.
    ///
    /// **WARN-1 D-D (census §3, R10.H2 row).** The first version re-printed the
    /// WHOLE standing set whenever one id joined it, so every standing id was
    /// warned again each time; and it forgot an id the first pass it was
    /// absent, so an id that dropped out for one pass (a ledger caught
    /// mid-append, a failed `ps`) re-warned on its return. Now:
    ///
    /// * the warning names only the NEW ids (`count` is how many; `standing` is
    ///   the whole set's size, a number and not a list);
    /// * a warned id is forgotten only after [`FORGET_AFTER_CLEAN_PASSES`]
    ///   consecutive clean passes without it, and a [`Report::degraded`] pass
    ///   never counts toward that.
    ///
    /// `standing()` still reports what THIS pass found: the hysteresis governs
    /// only what is news, never what is true now.
    pub fn apply(&self, rep: &Report) {
        let degraded = rep.degraded();
        let mut st = self.standing.lock().unwrap();
        let mut warned = self.warned.lock().unwrap();
        for inv in Invariant::ALL {
            let now: BTreeSet<String> = rep.violations.get(&inv).cloned().unwrap_or_default();
            let was_n = st.get(&inv).map_or(0, BTreeSet::len);
            let mem = warned.entry(inv).or_default();
            let new: Vec<String> = now.iter().filter(|id| !mem.contains_key(*id)).cloned().collect();
            // Present ids reset their absence; absent ones age only on a clean pass.
            for id in &now {
                mem.insert(id.clone(), 0);
            }
            if !degraded {
                mem.retain(|id, absent| {
                    if now.contains(id) {
                        return true;
                    }
                    *absent += 1;
                    *absent < FORGET_AFTER_CLEAN_PASSES
                });
            }
            if !new.is_empty() {
                let ids = new.join("; ");
                let n = new.len();
                let standing = now.len();
                let what = inv.what();
                match inv {
                    Invariant::I1 => tracing::warn!(invariant = "I1", count = n, standing, ids = %ids, "health I1 violated: {what}: {ids}"),
                    Invariant::I2 => tracing::warn!(invariant = "I2", count = n, standing, ids = %ids, "health I2 violated: {what}: {ids}"),
                    Invariant::I3 => tracing::warn!(invariant = "I3", count = n, standing, ids = %ids, "health I3 violated: {what}: {ids}"),
                    Invariant::I4 => tracing::warn!(invariant = "I4", count = n, standing, ids = %ids, "health I4 violated: {what}: {ids}"),
                    Invariant::I5 => tracing::warn!(invariant = "I5", count = n, standing, ids = %ids, "health I5 violated: {what}: {ids}"),
                }
            }
            if now.len() != was_n && !now.is_empty() {
                tracing::info!(invariant = inv.code(), standing = now.len(), "health: {} standing count {}", inv.code(), now.len());
            }
            if now.is_empty() {
                if st.remove(&inv).is_some() {
                    tracing::info!(invariant = inv.code(), "health: {} holds again", inv.code());
                }
            } else {
                st.insert(inv, now);
            }
        }
        if rep.skipped_ledgers > 0 {
            tracing::info!(skipped = rep.skipped_ledgers, "health: ledgers unreadable this pass (mid-append?); next pass rereads them");
        }
    }
}

/// **START THE LOOP** on the current runtime: wait [`Limits::period`], run a
/// pass off the reactor, repeat. A changed limit restarts the wait.
pub fn spawn(m: ThreadManager, h: Arc<Health>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let notified = h.kick.notified();
            let wait = h.limits().period;
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = notified => continue,
            }
            let (m2, h2) = (m.clone(), Arc::clone(&h));
            match tokio::task::spawn_blocking(move || h2.pass(&m2)).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!(error = %e, "health: the invariant pass failed; the next period retries"),
                Err(e) => tracing::warn!(error = %e, "health: the invariant pass panicked; the next period retries"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etime_and_ps_lines_parse() {
        assert_eq!(parse_etime("00:05"), Some(5));
        assert_eq!(parse_etime("02:30"), Some(150));
        assert_eq!(parse_etime("01:00:00"), Some(3600));
        assert_eq!(parse_etime("11-14:30:15"), Some(11 * 86_400 + 14 * 3600 + 30 * 60 + 15));
        assert_eq!(parse_etime("x"), None);
        let p = parse_ps(" 67490 68671    02:30 claude -p --model m --session-id abc-1 --add-dir /x\n bad line\n");
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].pid, p[0].ppid, p[0].age_s), (67490, 68671, 150));
        assert!(p[0].claude_like());
        assert_eq!(p[0].session().as_deref(), Some("abc-1"));
    }

    /// **WARN-1 D-D: A FAILED `ps` IS A DEGRADED PASS.** Two passes whose
    /// process table failed do not forget an I5 id; the counterpart, two clean
    /// passes, does. Forced red by dropping `|| self.procs_failed` from
    /// [`Report::degraded`].
    #[test]
    fn a_failed_process_table_pass_never_counts_toward_forgetting() {
        let h = Health::with(Limits::default(), Arc::new(|| Ok(Vec::new())));
        let one = |ids: &[&str], procs_failed: bool| Report {
            violations: if ids.is_empty() {
                BTreeMap::new()
            } else {
                [(Invariant::I5, ids.iter().map(|s| s.to_string()).collect())].into_iter().collect()
            },
            skipped_ledgers: 0,
            procs_failed,
        };
        let remembered = |h: &Health| h.warned.lock().unwrap().get(&Invariant::I5).is_some_and(|m| m.contains_key("pid=1"));
        h.apply(&one(&["pid=1"], false));
        h.apply(&one(&[], true));
        h.apply(&one(&[], true));
        assert!(remembered(&h), "two passes with a failed `ps` forgot an I5 id");
        assert!(h.standing().is_empty(), "standing is what this pass saw: {:?}", h.standing());
        h.apply(&one(&[], false));
        h.apply(&one(&[], false));
        assert!(!remembered(&h), "two clean passes must forget it");
    }

    #[test]
    fn claude_like_is_the_program_or_its_script_never_a_shell_command_text() {
        let pr = |a: &str| Proc { pid: 2, ppid: 1, age_s: 0, args: a.into() };
        assert!(pr("/bin/sh /tmp/x/stall-claude --session-id s").claude_like());
        assert!(pr("/usr/local/bin/claude --resume s").claude_like());
        assert_eq!(pr("/usr/local/bin/claude --resume s").session().as_deref(), Some("s"));
        assert!(!pr("/bin/zsh -c source /Users/x/.claude/snap.sh && claude --session-id s").claude_like());
        assert!(!pr("/bin/sleep 60").claude_like());
        assert_eq!(pr("claude -p").session(), None);
    }
}
