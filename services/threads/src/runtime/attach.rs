//! **SH-A2 — `threads/{thread_id}/attach`: lease a thread to an interactive
//! claude TUI** (`docs/SHIMS.md` §A.1, §A.4 row SH-A2).
//!
//! The daemon does everything a turn does BEFORE its spawn, in the turn's
//! order, and then — instead of spawning — hands the caller (`cauldron-claude`,
//! SH-A8) the launch recipe and keeps the thread's lease:
//!
//! 1. **`TurnLock`** — the one door every execution enters through. A thread a
//!    turn (or another attach) holds is REFUSED, never waited on: "it is
//!    impossible to run the same thread two times".
//! 2. **resolve/place** — the thread's STICKY account and its pool login
//!    directory as of now (`current_config_dir`); a request naming a different
//!    account is refused (a move is `move_thread`'s, with its transcript port,
//!    never an attach side effect); an unavailable account is refused.
//! 3. **`spawn_login`** — the spawn guard, verbatim: no login directory, no
//!    attach (except the machine default, by name).
//! 4. **scrub** — the API-error scrub, never fatal, exactly as `run_once`.
//! 5. **`ProjectedTurn::begin_interactive`** — a thread that is not
//!    `projection: projected` is REFUSED (SHIMS.md §A.3 allows "projected
//!    only, or flip"; this takes the first — see the round-38 ledger entry).
//! 6. **cookie + run token** — minted for the attach's cookie.
//! 7. **the `--mcp-config` file** (`0600`) and the `--settings` hooks file
//!    (`0600`), beside each other under `<data_dir>/turns/<thread>/`.
//! 8. **the `attached_sessions` row** (SH-A1) — LAST, so every failure before
//!    it has nothing durable to undo but the lock and the files.
//!
//! Every refusal releases what was taken: the lock (dropped), the files
//! (removed), and an opened projected turn (closed with `turn_ended`). A
//! failure AFTER the row insert deletes the row too — no orphan row can
//! outlive a failed attach.
//!
//! The lease (lock, projected turn, files) is parked in the manager's
//! `attached` map under the cookie. SH-A4's detach takes it back out.
//!
//! **SH-A4 — `threads/{thread_id}/detach` and the pid reaper.** Detach is the
//! lease's one exit: it DELETES the `attached_sessions` row FIRST (never a
//! soft end — `verify()` treats only a gone row as an ended session, so the
//! run token is void from that instant), then takes the lease back out of the
//! `attached` map, closes its projected turn (`ProjectedTurn::end`: the final
//! reconcile and a `turn_ended` if the wire never closed it), releases the
//! `TurnLock`, removes both files, re-asks the thread's queue
//! (`maybe_dispatch`, then the pool's `placement_wake`) and reports the run's
//! end (`managed::turn_finished`) — `finish_turn`'s epilogue, in its order.
//! An unknown cookie (or one attached to another thread) answers NOT FOUND
//! and changes nothing. Two detaches of one cookie (an explicit detach racing
//! the reaper) have exactly one winner: the row delete and the lease removal
//! are one step under the `attached` map's lock, and the loser answers NOT
//! FOUND (SH-A2c). The reaper ([`ThreadManager::reap_attached_sessions`],
//! driven by `crate::liveness::spawn_attach_reaper`) runs the SAME detach for
//! a row whose recorded pids are all dead. It never signals any process: the
//! operator is typing in that claude.
//!
//! **What this does NOT do:** accept the token in `verify()` (SH-A3), launch
//! anything, or re-adopt a lease across a daemon restart (SH-A5).

use std::sync::atomic::Ordering;

use super::{LockRefusal, ThreadManager, TurnLock, TurnMcpConfig};
use crate::spawn::{SpawnConfig, ATTACH_COOKIE_ENV, ATTACH_THREAD_ENV};

/// What a caller asks for.
#[derive(Debug, Clone, Default)]
pub struct AttachRequest {
    /// The `cauldron-claude` wrapper process (the reaper's fallback pid).
    pub wrapper_pid: i64,
    /// `--account <name>`: must be the thread's own account. `None` takes it.
    pub account: Option<String>,
}

/// **THE LAUNCH RECIPE** — what `cauldron-claude` spawns, inheriting its TTY.
#[derive(Debug, Clone)]
pub struct LaunchRecipe {
    pub thread_id: String,
    pub cookie: String,
    pub account: String,
    pub program: String,
    /// `SpawnConfig::build_interactive_args`: `build_args` minus the print
    /// flags, plus `--settings <hooks file>`.
    pub args: Vec<String>,
    /// `SpawnConfig::env` plus the attach cookie and thread. `None` = REMOVE.
    pub env: Vec<(String, Option<String>)>,
    pub cwd: String,
    /// `--resume` (true) or `--session-id` (false).
    pub resume: bool,
}

impl LaunchRecipe {
    pub fn to_json(&self) -> serde_json::Value {
        let env: serde_json::Map<String, serde_json::Value> = self
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone().map_or(serde_json::Value::Null, Into::into)))
            .collect();
        serde_json::json!({
            "thread_id": self.thread_id,
            "cookie": self.cookie,
            "account": self.account,
            "program": self.program,
            "args": self.args,
            "env": env,
            "cwd": self.cwd,
            "resume": self.resume,
        })
    }
}

/// Why an attach was refused. Every variant left nothing behind.
#[derive(Debug)]
pub enum AttachRefused {
    /// There is no such thread. Never reported as `Locked`: a caller told
    /// "the thread is held" about a thread that does not exist would wait for
    /// a turn that is not running (DEPLOY-9i finding 2).
    NotFound { thread: String },
    /// A turn or another attach holds the thread (`TurnLock`).
    Locked(LockRefusal),
    /// The request named an account that is not the thread's.
    AccountMismatch { thread: String, asked: String },
    /// The spawn guard refused the login, or the account is not available.
    Login(String),
    /// The thread is not `projection: projected`.
    NotProjected,
    /// `ProjectedTurn::begin_interactive` refused, or the transcript store is unknown.
    Projection(String),
    /// Writing a file or the row failed.
    Io(String),
}

impl std::fmt::Display for AttachRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttachRefused::NotFound { thread } => write!(f, "attach refused: no such thread: {thread}"),
            AttachRefused::Locked(r) => write!(
                f,
                "attach refused: the thread is held ({}) — a turn or another attach is running it",
                r.as_str()
            ),
            AttachRefused::AccountMismatch { thread, asked } => write!(
                f,
                "attach refused: the thread runs on {thread}, not {asked}; move it first (move_thread ports its transcript)"
            ),
            AttachRefused::Login(why) => write!(f, "attach refused: {why}"),
            AttachRefused::NotProjected => write!(
                f,
                "attach refused: the thread is not `projection: projected`; an interactive session on a \
                 native_owned thread would reach the ledger through no importer (docs/SHIMS.md §A.3)"
            ),
            AttachRefused::Projection(why) => write!(f, "attach refused: projection: {why}"),
            AttachRefused::Io(why) => write!(f, "attach refused: {why}"),
        }
    }
}

impl AttachRefused {
    pub fn code(&self) -> &'static str {
        match self {
            AttachRefused::NotFound { .. } => "not_found",
            AttachRefused::Locked(_) => "locked",
            AttachRefused::AccountMismatch { .. } => "account_mismatch",
            AttachRefused::Login(_) => "login",
            AttachRefused::NotProjected => "not_projected",
            AttachRefused::Projection(_) => "projection",
            AttachRefused::Io(_) => "io",
        }
    }
}

/// **THE LEASE** an attach holds until detach (SH-A4). Dropping it releases
/// the thread and removes both files.
pub struct AttachedHold {
    lock: TurnLock,
    projected: Option<crate::ledger::turn::ProjectedTurn>,
    _mcp: Option<TurnMcpConfig>,
    _settings: TurnMcpConfig,
}

impl AttachedHold {
    /// **SH-A5** — the lease `recover()` takes back for a session that outlived
    /// the daemon that attached it: the thread's lock re-taken under THIS
    /// daemon and the session's files. No projected turn — it died with the
    /// old daemon; the thread stays dirty and the next turn's begin reconciles it.
    pub(super) fn recovered(lock: TurnLock, mcp: Option<TurnMcpConfig>, settings: TurnMcpConfig) -> Self {
        AttachedHold { lock, projected: None, _mcp: mcp, _settings: settings }
    }
}

/// Write `body` to `path`, `0600`, refusing to overwrite.
fn write_private(path: &std::path::Path, body: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    use std::io::Write as _;
    o.open(path)?.write_all(body.as_bytes())
}

impl ThreadManager {
    /// **ATTACH** — see the module doc for the order and the guarantees.
    pub fn attach_thread(
        &self,
        thread_id: &str,
        req: &AttachRequest,
    ) -> std::result::Result<LaunchRecipe, AttachRefused> {
        // 1. THE LOCK. Refused, never waited on. `try_lock_thread` reports a
        // thread that does not exist as `StoreError` (its `get_state` failed),
        // which is not "held": ask the store whether the thread exists, and
        // answer NOT FOUND when it says no. Any other store failure stays a
        // fail-closed `Locked(StoreError)`.
        let lock = match self.try_lock_thread(thread_id, "attach") {
            Ok(l) => l,
            Err(LockRefusal::StoreError(why)) => {
                return Err(match self.store.get_thread(thread_id) {
                    Ok(None) => AttachRefused::NotFound { thread: thread_id.to_string() },
                    _ => AttachRefused::Locked(LockRefusal::StoreError(why)),
                });
            }
            Err(r) => return Err(AttachRefused::Locked(r)),
        };
        let state = lock.state.clone();
        // Every refusal below releases the lock EXPLICITLY (Drop is the
        // backstop, and it logs a defect when it is the one that fires).
        macro_rules! refuse {
            ($e:expr) => {{
                lock.release();
                return Err($e);
            }};
        }

        // 2. RESOLVE: the sticky account, its login directory as of now.
        let account = state.account.lock().unwrap().clone();
        if let Some(asked) = &req.account {
            if asked != &account {
                refuse!(AttachRefused::AccountMismatch { thread: account, asked: asked.clone() });
            }
        }
        let config_dir = self.current_config_dir(&state, &account);
        // 3. THE SPAWN GUARD, and the one availability predicate.
        let login = match self.spawn_login(&account, config_dir.as_deref()) {
            Ok(l) => l,
            Err(why) => refuse!(AttachRefused::Login(why)),
        };
        if let Some(why) = self.account_unavailable_reasons(&account) {
            refuse!(AttachRefused::Login(format!("{account} is not available ({})", why.join(", "))));
        }

        let (cfg, projected, allow_loss, scrub) = {
            let s = state.settings.lock().unwrap();
            let dirs = crate::plugins::collect_plugin_dirs(&s);
            let dir_strs: Vec<String> = dirs.iter().map(|p| p.display().to_string()).collect();
            (
                SpawnConfig::from_settings(&state.id, &s, dir_strs),
                s.projection.is_projected(),
                s.projection_allow_loss.clone(),
                s.scrub_api_error_details,
            )
        };
        // Refused BEFORE any write (the scrub rewrites the transcript).
        if !projected {
            refuse!(AttachRefused::NotProjected);
        }
        let Some(home) = crate::accounts::transcript_home() else {
            refuse!(AttachRefused::Projection(
                "HOME is not set: no transcript store to render the thread into".into()
            ));
        };

        // 4. THE API-ERROR SCRUB — never fatal, as in `run_once`.
        if scrub {
            if let Err(e) = crate::scrub::scrub_session(login.config_dir(), &home, &state.id) {
                tracing::error!(
                    thread = %state.id, error = %e,
                    "THE API-ERROR SCRUB FAILED — this attach resumes from a transcript that may \
                     still carry vendor error prose"
                );
            }
        }

        // 5. THE PROJECTED TURN, without a prompt (the operator types it).
        let cookie = uuid::Uuid::new_v4().to_string();
        let now = crate::store::now_rfc3339();
        let begun = crate::ledger::turn::ProjectedTurn::begin_interactive(crate::ledger::turn::TurnInputs {
            data_dir: &self.data_dir,
            thread_id: &state.id,
            config_dir: login.config_dir(),
            home: &home,
            cwd: &cfg.cwd,
            prompt: "",
            message_ids: &[],
            attempt: 1,
            cookie: &cookie,
            allow_loss: &allow_loss,
            now: &now,
            write_lock: self.managed.write_lock(),
        });
        let proj = match begun {
            Ok(p) => p,
            Err(e) => refuse!(AttachRefused::Projection(e.to_string())),
        };
        let resume = proj.resumable();
        state.resumable.store(resume, Ordering::SeqCst);
        let mut hold_proj = Some(proj);
        // From here every refusal closes the projected turn it opened.
        let close = |p: &mut Option<crate::ledger::turn::ProjectedTurn>| {
            if let Some(mut p) = p.take() {
                if let Err(e) = p.end(None, &crate::store::now_rfc3339()) {
                    tracing::error!(thread = %thread_id, error = %e, "closing a refused attach's projected turn failed");
                }
            }
        };

        // 6–7. THE TOKEN, THE DOOR, THE HOOKS FILE.
        let (run_token, mcp) = self.turn_mcp(&state.id, &cookie);
        let settings_path = self
            .data_dir
            .join("turns")
            .join(&state.id)
            .join(format!("{cookie}.settings.json"));
        let settings_body = crate::spawn::attach_settings_json(&crate::spawn::hook_bin()).to_string();
        if let Err(e) = write_private(&settings_path, &settings_body) {
            close(&mut hold_proj);
            refuse!(AttachRefused::Io(format!("writing {}: {e}", settings_path.display())));
        }
        let settings = TurnMcpConfig(settings_path);

        let mut cfg = cfg;
        cfg.run_token = Some(run_token);
        cfg.mcp_config = mcp.as_ref().map(TurnMcpConfig::path);
        let args = cfg.build_interactive_args(!resume, &settings.path());
        let mut env = cfg.env(&login);
        env.push((ATTACH_COOKIE_ENV.into(), Some(cookie.clone())));
        env.push((ATTACH_THREAD_ENV.into(), Some(state.id.clone())));
        let recipe = LaunchRecipe {
            thread_id: state.id.clone(),
            cookie: cookie.clone(),
            account: account.clone(),
            program: crate::spawn::claude_bin(),
            args,
            env,
            cwd: cfg.cwd.clone(),
            resume,
        };

        // 8. THE ROW — last.
        let row = crate::store::AttachedSessionRow {
            cookie: cookie.clone(),
            thread_id: state.id.clone(),
            account,
            wrapper_pid: req.wrapper_pid,
            claude_pid: None,
            transcript_path: None,
            byte_offset: 0,
            started_at: now,
        };
        if let Err(e) = self.store.insert_attached_session(&row) {
            close(&mut hold_proj);
            refuse!(AttachRefused::Io(format!("recording the attached session: {e}")));
        }

        // AFTER THE ROW: park the lease. A failure here deletes the row.
        let hold = AttachedHold { lock, projected: hold_proj, _mcp: mcp, _settings: settings };
        if let Err(why) = self.park_attached(&cookie, hold) {
            match self.store.delete_attached_session(&cookie) {
                Ok(_) => {}
                Err(e) => tracing::error!(
                    thread = %thread_id, %cookie, error = %e,
                    "COULD NOT DELETE A FAILED ATTACH'S ROW — it is an orphan until detach/recover"
                ),
            }
            return Err(AttachRefused::Io(why));
        }
        tracing::info!(thread = %thread_id, %cookie, resume, "attached an interactive session");
        Ok(recipe)
    }

    /// Park the lease under its cookie. On `Err` the hold has been closed
    /// (projected turn ended, lock released, files removed).
    fn park_attached(&self, cookie: &str, mut hold: AttachedHold) -> std::result::Result<(), String> {
        if self.attach_fault_after_insert.swap(false, Ordering::SeqCst) {
            if let Some(mut p) = hold.projected.take() {
                let _ = p.end(None, &crate::store::now_rfc3339());
            }
            hold.lock.release();
            return Err("injected fault after the attached row was written (test)".into());
        }
        self.attached.lock().unwrap_or_else(|e| e.into_inner()).insert(cookie.to_string(), hold);
        Ok(())
    }

    /// The cookies of every lease this daemon holds for `thread_id`.
    pub fn attached_cookies(&self, thread_id: &str) -> Vec<String> {
        self.attached
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, h)| h.lock.thread_id() == thread_id)
            .map(|(c, _)| c.clone())
            .collect()
    }

    /// TEST ONLY: make the NEXT attach fail right after its row is written.
    #[doc(hidden)]
    pub fn fail_next_attach_after_insert_for_test(&self) {
        self.attach_fault_after_insert.store(true, Ordering::SeqCst);
    }
}

/// Why a detach changed nothing.
#[derive(Debug)]
pub enum DetachRefused {
    /// No attached session with this cookie on this thread — never attached,
    /// already detached (or reaped), or attached to ANOTHER thread.
    NotFound { thread: String, cookie: String },
    /// Reading or deleting the row failed. Nothing was released.
    Io(String),
}

impl std::fmt::Display for DetachRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DetachRefused::NotFound { thread, cookie } => write!(
                f,
                "detach refused: no attached session {cookie} on thread {thread} (never attached, \
                 already detached, or attached to another thread)"
            ),
            DetachRefused::Io(why) => write!(f, "detach refused: {why}"),
        }
    }
}

impl DetachRefused {
    pub fn code(&self) -> &'static str {
        match self {
            DetachRefused::NotFound { .. } => "not_found",
            DetachRefused::Io(_) => "io",
        }
    }
}

/// What a detach did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detached {
    pub thread_id: String,
    pub cookie: String,
    /// Whether THIS daemon held the lease (lock, projected turn, files) and
    /// released it. `false` for a row left by another daemon process: the row
    /// is deleted, and the lock it may still name is `recover()`'s (SH-A5).
    pub released: bool,
}

impl Detached {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "thread_id": self.thread_id,
            "cookie": self.cookie,
            "released": self.released,
        })
    }
}

/// TEST ONLY: detaches of a cookie registered here wait, after their reads
/// and before their delete, until `parties` of them have arrived (or 5 s
/// pass) — the interleaving in which two detaches both see the row. Keyed by
/// cookie, so tests running in parallel in one binary cannot meet each other.
/// Empty in production: the cost is one uncontended lock per detach.
static DETACH_RENDEZVOUS: std::sync::Mutex<
    Option<std::collections::HashMap<String, std::sync::Arc<(std::sync::Mutex<usize>, std::sync::Condvar)>>>,
> = std::sync::Mutex::new(None);

/// TEST ONLY: make the next `parties` detaches of `cookie` meet after their
/// reads (see [`DETACH_RENDEZVOUS`]).
#[doc(hidden)]
pub fn rendezvous_detaches_after_read_for_test(cookie: &str, parties: usize) {
    let mut g = DETACH_RENDEZVOUS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(Default::default)
        .insert(cookie.to_string(), std::sync::Arc::new((std::sync::Mutex::new(parties), std::sync::Condvar::new())));
}

fn rendezvous_after_read(cookie: &str) {
    let r = {
        let g = DETACH_RENDEZVOUS.lock().unwrap_or_else(|e| e.into_inner());
        match g.as_ref().and_then(|m| m.get(cookie)) {
            Some(r) => r.clone(),
            None => return,
        }
    };
    let (left, cv) = &*r;
    let mut n = left.lock().unwrap_or_else(|e| e.into_inner());
    *n = n.saturating_sub(1);
    if *n == 0 {
        cv.notify_all();
        DETACH_RENDEZVOUS.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(|m| m.remove(cookie));
        return;
    }
    let _ = cv.wait_timeout_while(n, std::time::Duration::from_secs(5), |n| *n > 0);
}

/// Is the session a row describes still alive? `None` = no pid recorded at
/// all (`wrapper_pid <= 0` and no claude pid): nothing to judge by, so the
/// reaper leaves it to an explicit detach. Otherwise alive while ANY recorded
/// pid is alive — the claude pid from SessionStart, the wrapper pid as the
/// fallback (`docs/SHIMS.md` §A.3) — so a session is reaped only when every
/// process that could still call detach is gone.
pub(crate) fn session_alive(row: &crate::store::AttachedSessionRow) -> Option<bool> {
    let pids: Vec<i64> = row
        .claude_pid
        .into_iter()
        .chain(std::iter::once(row.wrapper_pid))
        .filter(|p| *p > 0)
        .collect();
    if pids.is_empty() {
        return None;
    }
    Some(pids.into_iter().any(super::pid_alive))
}

impl ThreadManager {
    /// **DETACH** — see the module doc. `exit` is the claude exit code the
    /// wrapper saw, recorded on the `turn_ended` the detach writes.
    pub fn detach_thread(
        &self,
        thread_id: &str,
        cookie: &str,
        exit: Option<i32>,
    ) -> std::result::Result<Detached, DetachRefused> {
        self.detach_inner(Some(thread_id), cookie, exit, "detach")
    }

    fn detach_inner(
        &self,
        thread_id: Option<&str>,
        cookie: &str,
        exit: Option<i32>,
        why: &str,
    ) -> std::result::Result<Detached, DetachRefused> {
        let not_found = || DetachRefused::NotFound {
            thread: thread_id.unwrap_or("?").to_string(),
            cookie: cookie.to_string(),
        };
        let row = self
            .store
            .get_attached_session(cookie)
            .map_err(|e| DetachRefused::Io(format!("reading attached_sessions: {e}")))?;
        let held_thread = self
            .attached
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(cookie)
            .map(|h| h.lock.thread_id().to_string());
        let owner = match (&row, &held_thread) {
            (Some(r), _) => r.thread_id.clone(),
            (None, Some(t)) => t.clone(),
            (None, None) => return Err(not_found()),
        };
        if thread_id.is_some_and(|t| t != owner) {
            return Err(not_found());
        }
        rendezvous_after_read(cookie);

        // 1–2. THE ROW, DELETED FIRST (from here `verify()` refuses the
        // token), then THE LEASE, back out of the map — as ONE step under the
        // `attached` map's lock. Two detaches of one cookie (an explicit
        // detach racing the reaper) can both pass the reads above; this is
        // where exactly one of them wins. The winner deletes the row and takes
        // the hold; the loser finds neither and answers NOT FOUND, never a
        // `released: false` success (SH-A4 inspect, finding 2). Lock order is
        // `attached` then the store; nothing takes them the other way.
        let (deleted, hold) = {
            let mut map = self.attached.lock().unwrap_or_else(|e| e.into_inner());
            let deleted = if row.is_some() {
                self.store
                    .delete_attached_session(cookie)
                    .map_err(|e| DetachRefused::Io(format!("deleting the attached session: {e}")))?
            } else {
                false
            };
            (deleted, map.remove(cookie))
        };
        let Some(mut hold) = hold else {
            if !deleted {
                // Another detach (or the reaper) got here first.
                return Err(not_found());
            }
            tracing::info!(
                thread = %owner, %cookie, why,
                "detached an attached session this daemon held no lease for; its row is deleted"
            );
            return Ok(Detached { thread_id: owner, cookie: cookie.to_string(), released: false });
        };
        // 3. THE FINAL RECONCILE and `turn_ended`.
        if let Some(mut p) = hold.projected.take() {
            if let Err(e) = p.end(exit, &crate::store::now_rfc3339()) {
                tracing::error!(
                    thread = %owner, %cookie, error = %e,
                    "THE DETACH'S FINAL RECONCILE FAILED — the lease is released anyway; the \
                     ledger stays dirty and the next turn's begin reconciles it"
                );
            }
        }
        // 4. THE LOCK, then the files (dropping the hold).
        hold.lock.release();
        drop(hold);
        tracing::info!(thread = %owner, %cookie, why, ?exit, "detached an interactive session");
        // 5. finish_turn's epilogue: the queue, the pool, the managed report.
        if let Ok(state) = self.get_state(&owner) {
            let terminal = std::mem::take(&mut *state.turn_terminal.lock().unwrap());
            self.maybe_dispatch(&state);
            self.placement_wake("attach_detached");
            crate::managed::turn_finished(self, &owner, terminal);
        }
        Ok(Detached { thread_id: owner, cookie: cookie.to_string(), released: true })
    }

    /// **THE PID REAPER'S SWEEP.** Every `attached_sessions` row whose recorded
    /// pids are ALL dead ([`session_alive`]) is detached exactly as an explicit
    /// detach would. A live pid keeps its row; a row with no pid at all is
    /// left for an explicit detach. Never signals a process. Returns the
    /// cookies it detached.
    pub fn reap_attached_sessions(&self) -> Vec<String> {
        let rows = match self.store.list_all_attached_sessions() {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "the attach reaper could not read attached_sessions");
                return vec![];
            }
        };
        let mut reaped = vec![];
        for row in rows {
            match session_alive(&row) {
                Some(false) => {}
                Some(true) => continue,
                None => {
                    tracing::debug!(
                        thread = %row.thread_id, cookie = %row.cookie,
                        "attached session records no pid; the reaper cannot judge it"
                    );
                    continue;
                }
            }
            match self.detach_inner(None, &row.cookie, None, "reaped: every recorded pid is dead") {
                Ok(_) => reaped.push(row.cookie),
                Err(DetachRefused::NotFound { .. }) => {} // detached under us
                Err(e) => tracing::error!(thread = %row.thread_id, cookie = %row.cookie, error = %e, "reaping failed"),
            }
        }
        reaped
    }
}
