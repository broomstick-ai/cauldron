//! **BIND FIRST, RECOVER BEHIND IT** (R9.C7g, implements C-7).
//!
//! ## Why this exists
//!
//! On 2026-10-20 (DEPLOY-9g's drain-cap handoff) a fresh threads replayed 252
//! "child resumed from its ledger" records BEFORE it served its port: the
//! listener was held from 19:41:32Z, but nothing accepted on it until
//! `recover()` returned at 19:42:35Z. For ~60 s a TCP connect succeeded (the
//! kernel answers from the backlog) and every request — `/health` included —
//! hung. The operator saw "threads is down". Nothing was lost; the daemon was
//! simply not answering while it did work nobody had to wait for.
//!
//! ## What changed
//!
//! `run_daemon` now serves the chassis the moment the store is open, and runs
//! `ThreadManager::recover` in a background task. While it runs:
//!
//! * `/health` answers **200** with `status: "recovering"` and a `recovery`
//!   object naming what is still resuming (`children_pending`, the phase).
//!   200 and not 503 ON PURPOSE: cauldron's holder reads ANY non-200 as
//!   `unhealthy` (`env::seams::probe`) and its launch path kills a child whose
//!   `/health` has not answered 200 within `start_timeout_ms`
//!   (`cli::jobs::wait_healthy`). A recovering daemon is alive and starting;
//!   it must never be relaunched for doing its recovery.
//! * a request that NEEDS recovery state (anything that dispatches a turn,
//!   queues, cancels, spawns or reads a thread/child roster recovery is still
//!   rebuilding) WAITS for recovery, bounded by [`DEFAULT_WAIT_MS`]
//!   ([`WAIT_ENV`]), and past the bound is refused **`busy`** — the code whose
//!   documented remedy is "call it again" (`shapewire::envelope::codes::BUSY`).
//!   It is never answered from half-recovered state.
//! * a request that does NOT need it (see [`needs_recovery`]) is served at once.
//!
//! When recovery returns, `/health` goes back to `status: "ok"` and its
//! `recovery.state` reads `done`.
//!
//! ## A recovery that never finishes is not healthy (R9.C7g r37, doctrine 10)
//!
//! Two ways recovery can fail to return, and what each does — so that nobody
//! sits in "200 recovering" forever while the holder, which reads only the
//! status code, believes the daemon is fine:
//!
//! * **it panics** → `run_daemon` sees the recovery task's panic and the
//!   PROCESS EXITS with [`PANIC_EXIT_CODE`]. The holder sees a dead service
//!   and relaunches it; recovery re-derives from durable state, so the next
//!   boot starts it again from the top.
//! * **it runs past [`DEFAULT_STUCK_MS`]** ([`STUCK_ENV`], 10 min by default)
//!   → `recovery.state` reads `stuck` and `/health` answers **503** with
//!   `status: "recovery_stuck"`, which the holder's probe reads as unhealthy
//!   (its existing relaunch path, drain first). The bound is computed at read
//!   time from when recovery began, so it needs no timer of its own.
//!
//! A manager that never calls [`Recovery::begin`] — every CLI verb and every
//! in-process test — is `done` from construction, so nothing outside
//! `run_daemon` changes behaviour.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// How long a request that needs recovery state waits for it before it is
/// refused `busy`.
pub const DEFAULT_WAIT_MS: u64 = 5_000;

/// Override for [`DEFAULT_WAIT_MS`] in milliseconds (`0` = refuse at once).
pub const WAIT_ENV: &str = "THREADS_RECOVERY_WAIT_MS";

/// How long boot recovery may run before `/health` calls it stuck (503).
pub const DEFAULT_STUCK_MS: u64 = 600_000;

/// Override for [`DEFAULT_STUCK_MS`] in milliseconds.
pub const STUCK_ENV: &str = "THREADS_RECOVERY_STUCK_MS";

/// The exit code of a daemon whose boot recovery panicked (`EX_SOFTWARE`).
pub const PANIC_EXIT_CODE: i32 = 70;

/// `recovery.state` once recovery has run past its bound.
pub const STUCK: &str = "stuck";

/// Boot recovery's progress, readable without the store.
#[derive(Debug)]
pub struct Recovery {
    recovering: AtomicBool,
    origin: Instant,
    started_ms: AtomicU64,
    /// R9.C7h: time the store spent migrating before recovery began; the
    /// stuck bound covers both ([`Recovery::begin_after`]).
    carried_ms: AtomicU64,
    finished_ms: AtomicU64,
    children_total: AtomicU64,
    children_resumed: AtomicU64,
    phase: Mutex<&'static str>,
    wait: Mutex<Duration>,
    stuck_after: Duration,
    done: tokio::sync::watch::Sender<bool>,
}

impl Default for Recovery {
    fn default() -> Self {
        Self::new()
    }
}

impl Recovery {
    /// Not recovering: `done` from the start.
    pub fn new() -> Self {
        let wait = std::env::var(WAIT_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_WAIT_MS);
        let stuck = std::env::var(STUCK_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(DEFAULT_STUCK_MS);
        Recovery {
            recovering: AtomicBool::new(false),
            origin: Instant::now(),
            started_ms: AtomicU64::new(0),
            carried_ms: AtomicU64::new(0),
            finished_ms: AtomicU64::new(0),
            children_total: AtomicU64::new(0),
            children_resumed: AtomicU64::new(0),
            phase: Mutex::new("done"),
            wait: Mutex::new(Duration::from_millis(wait)),
            stuck_after: Duration::from_millis(stuck),
            done: tokio::sync::watch::channel(true).0,
        }
    }

    fn now_ms(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }

    /// Recovery has started: from here until [`Recovery::finish`] the daemon
    /// is serving but `recovering`.
    pub fn begin(&self) {
        self.begin_after(Duration::ZERO)
    }

    /// [`Recovery::begin`], after a store migration that already took
    /// `migrated` (R9.C7h): the stuck bound covers migrate + recover together,
    /// so a migration that used most of it leaves recovery only the rest.
    pub fn begin_after(&self, migrated: Duration) {
        self.carried_ms.store(migrated.as_millis() as u64, Ordering::SeqCst);
        self.started_ms.store(self.now_ms(), Ordering::SeqCst);
        self.children_total.store(0, Ordering::SeqCst);
        self.children_resumed.store(0, Ordering::SeqCst);
        *self.phase.lock().unwrap() = "starting";
        self.recovering.store(true, Ordering::SeqCst);
        self.done.send_replace(false);
    }

    /// Which pass recovery is in — a word for the operator, not a contract.
    pub fn phase(&self, p: &'static str) {
        *self.phase.lock().unwrap() = p;
    }

    /// How many managed children the resume pass has to walk.
    pub fn children(&self, total: u64) {
        self.children_total.store(total, Ordering::SeqCst);
    }

    /// One more child walked.
    pub fn child_resumed(&self) {
        self.children_resumed.fetch_add(1, Ordering::SeqCst);
    }

    /// Recovery returned. Every waiter is released.
    pub fn finish(&self) {
        self.finished_ms.store(self.now_ms(), Ordering::SeqCst);
        *self.phase.lock().unwrap() = "done";
        self.recovering.store(false, Ordering::SeqCst);
        self.done.send_replace(true);
    }

    pub fn is_recovering(&self) -> bool {
        self.recovering.load(Ordering::SeqCst)
    }

    /// Still recovering, and for longer than the stuck bound ([`STUCK_ENV`]).
    pub fn is_stuck(&self) -> bool {
        self.is_recovering()
            && self.now_ms().saturating_sub(self.started_ms.load(Ordering::SeqCst))
                + self.carried_ms.load(Ordering::SeqCst)
                > self.stuck_after.as_millis() as u64
    }

    /// The bound past which a running recovery is `stuck`.
    pub fn stuck_after(&self) -> Duration {
        self.stuck_after
    }

    /// Children still to walk (0 once done).
    pub fn children_pending(&self) -> u64 {
        if !self.is_recovering() {
            return 0;
        }
        self.children_total
            .load(Ordering::SeqCst)
            .saturating_sub(self.children_resumed.load(Ordering::SeqCst))
    }

    /// **TEST SEAM.** Replace the wait bound.
    #[doc(hidden)]
    pub fn set_wait(&self, d: Duration) {
        *self.wait.lock().unwrap() = d;
    }

    pub fn wait_bound(&self) -> Duration {
        *self.wait.lock().unwrap()
    }

    /// Wait (at most the bound) for recovery to finish. `true` = done.
    pub async fn wait_done(&self) -> bool {
        let mut rx = self.done.subscribe();
        let bound = self.wait_bound();
        if *rx.borrow_and_update() {
            return true;
        }
        let done = matches!(tokio::time::timeout(bound, rx.wait_for(|d| *d)).await, Ok(Ok(_)));
        done
    }

    /// The `/health` `recovery` object.
    pub fn health_json(&self) -> Value {
        let recovering = self.is_recovering();
        let started = self.started_ms.load(Ordering::SeqCst);
        let finished = self.finished_ms.load(Ordering::SeqCst);
        json!({
            "state": if self.is_stuck() { STUCK } else if recovering { "recovering" } else { "done" },
            "phase": *self.phase.lock().unwrap(),
            "children_total": self.children_total.load(Ordering::SeqCst),
            "children_resumed": self.children_resumed.load(Ordering::SeqCst),
            "children_pending": self.children_pending(),
            "migrated_ms": self.carried_ms.load(Ordering::SeqCst),
            "elapsed_ms": if recovering { self.now_ms().saturating_sub(started) } else { finished.saturating_sub(started) },
        })
    }

    /// The refusal a request gets when the bound ran out.
    pub fn refusal(&self, method: &str) -> shapewire::envelope::Refusal {
        shapewire::envelope::Refusal::new(
            shapewire::envelope::codes::BUSY,
            format!(
                "threads is recovering after a restart ({} managed child(ren) still resuming, \
                 phase `{}`); `{method}` needs that state and waited {} ms for it. Nothing was \
                 done — call it again",
                self.children_pending(),
                *self.phase.lock().unwrap(),
                self.wait_bound().as_millis()
            ),
        )
    }
}

/// **DOES THIS METHOD NEED RECOVERY STATE?** `false` only for the reads whose
/// answer recovery cannot change or make wrong: the running image, the gauge,
/// a budget's row, and a thread's own event history
/// (append-only: recovery can only ADD to it, never make what is there
/// untrue). Everything else — every act, and every roster read
/// (`threads/list`, `agents/*`, which recovery is still materializing) —
/// waits. The default is `true`, so a method added later is safe until
/// someone decides otherwise.
pub fn needs_recovery(method: &str) -> bool {
    let segs: Vec<&str> = method.split('/').collect();
    let safe = match segs.as_slice() {
        ["daemon", "image"] => true,
        ["gauge", "status"] => true,
        ["budgets", _, "get"] => true,
        ["threads", _, "history"] => true,
        ["threads", _, "budget_get"] => true,
        _ => false,
    };
    !safe
}

/// `threads/{id}/history`? Served during recovery ONLY for a thread the store
/// already has: a child recovery has not materialized yet has no row, and
/// `list_events` would answer `[]` for it — an empty history that is not
/// true. The producer gates that case like any other ([`needs_recovery`]).
/// (`shapes/list` is not here or above: it is a chassis method and never
/// reaches the producer's `handle`.)
pub fn is_thread_history(method: &str) -> bool {
    matches!(method.split('/').collect::<Vec<_>>().as_slice(), ["threads", _, "history"])
}

/// **TEST SEAM** (`test-seams` builds only): `THREADS_RECOVERY_FAULT=panic`
/// panics boot recovery, `=hang` never lets it return. Production builds
/// compile the inert twin below.
///
/// R9.C7i: `=block` STARVES THE RUNTIME. `hang` is `pending()` and yields, so
/// it cannot reproduce a starved runtime; `block` puts one task per worker
/// thread into `std::thread::sleep` for [`BLOCK_MS_ENV`] ms (default
/// [`DEFAULT_BLOCK_MS`]) — every worker parked in a synchronous call, as the
/// prod stall was — and then lets recovery run on.
#[cfg(any(test, feature = "test-seams"))]
pub async fn fault_seam() {
    match std::env::var("THREADS_RECOVERY_FAULT").as_deref() {
        Ok("panic") => panic!("THREADS_RECOVERY_FAULT=panic: boot recovery panicked (test seam)"),
        Ok("hang") => std::future::pending::<()>().await,
        Ok("block") => {
            let ms = std::env::var(BLOCK_MS_ENV)
                .ok()
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(DEFAULT_BLOCK_MS);
            let workers = tokio::runtime::Handle::current().metrics().num_workers();
            tracing::warn!(workers, block_ms = ms, "THREADS_RECOVERY_FAULT=block: starving every runtime worker (test seam)");
            // Spawned, then awaited: the newest spawn sits in this worker's
            // (unstealable) LIFO slot and runs here once this task yields; the
            // rest are stolen by the other workers. One per worker, no more —
            // a spare would run after and stretch the stall past `ms`.
            let blockers: Vec<_> = (0..workers)
                .map(|_| tokio::spawn(async move { std::thread::sleep(std::time::Duration::from_millis(ms)) }))
                .collect();
            for b in blockers {
                let _ = b.await;
            }
        }
        _ => {}
    }
}

/// `THREADS_RECOVERY_FAULT=block`'s length, in ms (R9.C7i test seam).
pub const BLOCK_MS_ENV: &str = "THREADS_RECOVERY_FAULT_BLOCK_MS";

/// `THREADS_RECOVERY_FAULT=block`'s default length.
pub const DEFAULT_BLOCK_MS: u64 = 5_000;

/// The inert twin of the test seam.
#[cfg(not(any(test, feature = "test-seams")))]
pub async fn fault_seam() {}
