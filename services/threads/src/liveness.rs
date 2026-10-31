//! **`/health` THAT CAN SAY NO** (R9.C6, gap C-6).
//!
//! ## Why this exists
//!
//! Cauldron decides whether threads is alive by asking it. Until R9.C6 the
//! question was `probe = "connect"`: a TCP handshake, which the KERNEL answers
//! from the listen backlog whether or not anything in this process is still
//! working. A wedged threads therefore read as live and the supervisor's
//! "unhealthy, restart it" branch never ran — and threads has wedged: on
//! 2026-09-11 a WAL grown to 87 MB left `/health` at 2.1 s and the daemon too
//! wedged to answer SIGTERM (see [`crate::spawn_wal_keeper`]).
//!
//! Switching the probe to `http` against `/health` is necessary and NOT
//! sufficient, and the reason is measured in the shapewire chassis:
//! `libs/shapewire/src/server.rs`'s `health` handler builds its body from the
//! producer's identity alone. It touches no store, takes no lock, and runs in
//! its own per-connection task on a multi-thread runtime — so as long as ONE
//! worker is free it answers `200 {"status":"ok"}`, while every task that needs
//! the store sits behind a write mutex that is never released. That `/health`
//! is a better check than `connect` only for a TOTAL runtime wedge (no worker
//! free: the request is accepted by the kernel and never answered, so the
//! probe times out). For the wedge that actually happened, it is no better.
//!
//! ## What this adds
//!
//! A **heartbeat**: a task on the daemon's own runtime that, every
//! [`HEARTBEAT_PERIOD`], runs a liveness check off the reactor
//! (`spawn_blocking`) and stamps the time when it succeeds. In the daemon the
//! check is [`crate::store::Store::ping`]: take the store's write mutex and
//! open-and-roll-back a write transaction — "could this daemon write its store
//! right now". A **layer** in front of the chassis router answers `/health`
//! with **503** once the last successful beat is older than the declared bound
//! ([`DEFAULT_STALE_AFTER_SECS`], overridable through [`STALE_ENV`]), and
//! otherwise lets shapewire's own handler answer exactly as before.
//!
//! So `/health` now fails in BOTH wedges: a runtime with no free worker cannot
//! answer at all (the beat cannot run either), and a runtime that can answer
//! but whose store is unreachable answers 503.
//!
//! ## The bound, and why it is long
//!
//! 60 s. A restart of threads interrupts every turn it is running, so a false
//! "wedged" is expensive and the bound has to outlast every LEGITIMATE hold of
//! the write lock: the five-minute WAL keeper's fold (milliseconds at steady
//! state, seconds on a grown WAL), a batch write, a writer waiting out its own
//! 5 s `busy_timeout`. A single successful beat inside the window resets it.
//! On top of this bound cauldron adds its own failure count (`down_after`
//! consecutive failed checks, `poll_secs` apart — 3 × 4 s by default), so the
//! wall time from "the store stopped answering" to "cauldron stops threads" is
//! about 60 s + 12 s.
//!
//! The shutdown drain is NOT inside this window and needs no allowance here:
//! on SIGTERM the chassis stops accepting before the drain begins, so during a
//! drain `/health` is unreachable whichever probe is declared, and what a
//! supervisor does with an unreachable port mid-stop is the stop path's
//! business (gap C-1), not the probe's.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};

/// How often the heartbeat runs its check.
pub const HEARTBEAT_PERIOD: Duration = Duration::from_secs(2);

/// How old the last successful beat may be before `/health` says 503.
pub const DEFAULT_STALE_AFTER_SECS: u64 = 60;

/// Override for [`DEFAULT_STALE_AFTER_SECS`], in whole seconds. Exists so a
/// test can shrink the bound instead of waiting a minute (doctrine 4); a value
/// that does not parse, or `0`, is ignored and the default stands.
pub const STALE_ENV: &str = "THREADS_HEALTH_STALE_SECS";

/// The heartbeat's shared state: when it last succeeded, and the bound.
#[derive(Clone, Debug)]
pub struct Heartbeat {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    origin: Instant,
    /// Milliseconds after `origin` of the last successful check.
    last_ok_ms: AtomicU64,
    stale_after: Duration,
}

impl Heartbeat {
    /// A heartbeat that has just beaten: a daemon is fresh when it starts
    /// serving, which is after recovery, which is when this is built.
    pub fn new(stale_after: Duration) -> Self {
        Heartbeat {
            inner: Arc::new(Inner {
                origin: Instant::now(),
                last_ok_ms: AtomicU64::new(0),
                stale_after,
            }),
        }
    }

    /// The bound from [`STALE_ENV`], else [`DEFAULT_STALE_AFTER_SECS`].
    pub fn from_env() -> Self {
        let secs = std::env::var(STALE_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|s| *s > 0)
            .unwrap_or(DEFAULT_STALE_AFTER_SECS);
        Self::new(Duration::from_secs(secs))
    }

    /// Record a successful check, now.
    pub fn beat(&self) {
        let ms = self.inner.origin.elapsed().as_millis() as u64;
        self.inner.last_ok_ms.store(ms, Ordering::Relaxed);
    }

    /// Time since the last successful check.
    pub fn age(&self) -> Duration {
        let now = self.inner.origin.elapsed().as_millis() as u64;
        Duration::from_millis(now.saturating_sub(self.inner.last_ok_ms.load(Ordering::Relaxed)))
    }

    pub fn stale_after(&self) -> Duration {
        self.inner.stale_after
    }

    pub fn is_stale(&self) -> bool {
        self.age() > self.inner.stale_after
    }
}

/// Run `check` every `period`, off the reactor, and beat on every success.
///
/// The loop itself is an ordinary task on the daemon's runtime ON PURPOSE: a
/// runtime that cannot schedule it is a runtime that is not serving either,
/// and that too should read as stale.
pub fn spawn<F>(hb: Heartbeat, period: Duration, check: F) -> tokio::task::JoinHandle<()>
where
    F: Fn() -> Result<(), String> + Send + Sync + 'static,
{
    let check = Arc::new(check);
    tokio::spawn(async move {
        let mut was_stale = false;
        loop {
            tokio::time::sleep(period).await;
            let c = Arc::clone(&check);
            match tokio::task::spawn_blocking(move || c()).await {
                Ok(Ok(())) => {
                    if was_stale {
                        tracing::warn!(
                            "liveness: the heartbeat check succeeded again; /health answers 200"
                        );
                    }
                    hb.beat();
                    was_stale = false;
                }
                Ok(Err(e)) => {
                    let stale = hb.is_stale();
                    if stale && !was_stale {
                        tracing::error!(
                            error = %e,
                            age_ms = hb.age().as_millis() as u64,
                            bound_ms = hb.stale_after().as_millis() as u64,
                            "liveness: the heartbeat check has not succeeded within its bound; \
                             /health now answers 503 so the supervisor can see it"
                        );
                    } else {
                        tracing::warn!(error = %e, "liveness: heartbeat check failed");
                    }
                    was_stale = stale;
                }
                Err(e) => tracing::error!(error = %e, "liveness: heartbeat check panicked"),
            }
        }
    })
}

/// What else a stale `/health` carries: the fields the healthy answer adds
/// (`restart_gate`), read from memory, never from the store.
pub type Extras = Arc<dyn Fn() -> std::collections::BTreeMap<String, serde_json::Value> + Send + Sync>;

#[derive(Clone)]
struct Guard {
    hb: Heartbeat,
    extras: Extras,
}

/// Put the heartbeat in front of `/health`: stale ⇒ 503 with the age and the
/// bound in the body; fresh ⇒ the router's own `/health` answers unchanged.
/// Every other route passes straight through.
///
/// R9.C7d: the 503 body also carries `extras` — the restart gate
/// (`restart_gate.{idle, turns, draining_since}`) — so the supervisor that is
/// about to relaunch a wedged daemon can see whether turns are still running
/// and ask it to DRAIN first, rather than killing them.
pub fn layer(router: Router, hb: Heartbeat, extras: Extras) -> Router {
    router.layer(axum::middleware::from_fn_with_state(Guard { hb, extras }, guard))
}

async fn guard(State(g): State<Guard>, req: Request, next: Next) -> Response {
    let hb = &g.hb;
    if req.uri().path() == "/health" && hb.is_stale() {
        let mut body = serde_json::json!({
            "status": "wedged",
            "heartbeat_age_ms": hb.age().as_millis() as u64,
            "stale_after_ms": hb.stale_after().as_millis() as u64,
            "why": "the daemon's store-liveness heartbeat has not succeeded within its bound",
        });
        if let Some(o) = body.as_object_mut() {
            for (k, v) in (g.extras)() {
                o.entry(k).or_insert(v);
            }
        }
        return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
    }
    // R9.C7g (r37): RECOVERY RAN PAST ITS BOUND. A recovery that never
    // returns must not read healthy forever — the holder reads only the code,
    // so this one is 503 (`status: "recovery_stuck"`), which its probe calls
    // unhealthy and its existing relaunch path acts on. See `crate::recovery`.
    if req.uri().path() == "/health" {
        let extras = (g.extras)();
        let stuck = extras
            .get("recovery")
            .and_then(|r| r.get("state"))
            .and_then(|s| s.as_str())
            == Some(crate::recovery::STUCK);
        if stuck {
            let mut body = serde_json::json!({
                "status": RECOVERY_STUCK,
                "why": "boot recovery has run past its bound without finishing",
            });
            if let Some(o) = body.as_object_mut() {
                for (k, v) in extras {
                    o.entry(k).or_insert(v);
                }
            }
            return (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response();
        }
    }
    // R9.C7g: SERVING BUT STILL RECOVERING. The answer stays 200 — cauldron's
    // holder reads any other code as unhealthy, and a recovering daemon is
    // alive and starting, not sick — but `status` says `recovering` instead of
    // `ok`, so nobody reading the body mistakes it for done. The `recovery`
    // object (from `extras`) names what is still resuming.
    if req.uri().path() == "/health" {
        let recovering = (g.extras)()
            .get("recovery")
            .and_then(|r| r.get("state"))
            .and_then(|s| s.as_str())
            == Some("recovering");
        if recovering {
            let resp = next.run(req).await;
            if resp.status() != StatusCode::OK {
                return resp;
            }
            let bytes = match axum::body::to_bytes(resp.into_body(), 1 << 20).await {
                Ok(b) => b,
                Err(e) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, format!("reading /health: {e}")).into_response()
                }
            };
            let mut body: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(o) = body.as_object_mut() {
                o.insert("status".into(), serde_json::json!(RECOVERING));
            }
            return (StatusCode::OK, Json(body)).into_response();
        }
    }
    next.run(req).await
}

/// `/health`'s `status` while boot recovery runs behind a served port (R9.C7g).
pub const RECOVERING: &str = "recovering";

/// `/health`'s `status` (with a 503) once boot recovery has run past its
/// bound (R9.C7g r37, `crate::recovery::STUCK_ENV`).
pub const RECOVERY_STUCK: &str = "recovery_stuck";

// ── SH-A4: THE ATTACHED-SESSION PID REAPER (`docs/SHIMS.md` §A.3) ────────────
//
// A terminal closed under `cauldron-claude` sends no detach. Every
// [`ATTACH_REAP_PERIOD`] (overridable through [`ATTACH_REAP_ENV`], `0` = off)
// this task runs [`crate::ThreadManager::reap_attached_sessions`] off the
// reactor: a row whose recorded pids are all dead is detached exactly as an
// explicit detach would be. It READS liveness (`kill(pid, 0)` and the zombie
// check) and NEVER sends a process a signal — the operator may be typing in it.

/// How often the attach reaper sweeps `attached_sessions`.
pub const ATTACH_REAP_PERIOD: Duration = Duration::from_secs(5);

/// Override for [`ATTACH_REAP_PERIOD`] in milliseconds; `0` switches the
/// reaper off, a value that does not parse keeps the default.
pub const ATTACH_REAP_ENV: &str = "THREADS_ATTACH_REAP_MS";

/// The reaper period from the environment: `None` = off.
pub fn attach_reap_period_from_env() -> Option<Duration> {
    match std::env::var(ATTACH_REAP_ENV).ok().and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(0) => None,
        Some(ms) => Some(Duration::from_millis(ms)),
        None => Some(ATTACH_REAP_PERIOD),
    }
}

/// Spawn the attach reaper on the current runtime.
pub fn spawn_attach_reaper(
    manager: crate::ThreadManager,
    period: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(period).await;
            let m = manager.clone();
            match tokio::task::spawn_blocking(move || m.reap_attached_sessions()).await {
                Ok(reaped) if !reaped.is_empty() => tracing::warn!(
                    ?reaped,
                    "attach reaper: detached sessions whose every recorded pid is dead"
                ),
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "attach reaper: the sweep panicked"),
            }
        }
    })
}
