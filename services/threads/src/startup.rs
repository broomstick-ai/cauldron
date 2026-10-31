//! **THE STORE MIGRATES BEHIND THE SERVED PORT** (R9.C7h, implements C-7).
//!
//! ## Why this exists
//!
//! R9.C7g made threads serve before boot recovery, so `/health` answered
//! within 2 s of the bind. It did not hold in prod (DEPLOY-9h, 2026-10-21):
//! 75.4 s with no `/health` answer. The socket was bound and LISTENing, but
//! nothing accepted on it while `build_manager` → `Store::open` ran R10.M1's
//! one-time queue migration (~72 s on the 4.3 GB store), which comes BEFORE
//! the chassis is built. Bind-first bound the port; it did not serve it.
//!
//! ## What changed
//!
//! `run_daemon` now serves the listener the moment it holds it, with the
//! router this module builds, and opens the store (`build_manager`) off the
//! reactor behind it. Until the chassis is ready the router answers for it:
//!
//! * **a migration is running** (`Store::open_noticing` announced one) →
//!   every request, `/health` included, is answered **503** with
//!   `status: "migrating"` and a `migration` object (the step, how long).
//!   503 and not 200 ON PURPOSE (the brief: non-200, distinct from
//!   `recovering` and `recovery_stuck`): the daemon cannot serve a single
//!   request yet. Cauldron's holder reads this answer as ALIVE-NOT-READY
//!   (`env::hosting::health_status`), so it neither kills the launch nor
//!   relaunches it.
//! * **the migration has run past the stuck bound** (the SAME bound as boot
//!   recovery, [`crate::recovery::STUCK_ENV`], 10 min by default) → 503
//!   `status: "recovery_stuck"`, which the holder reads as unhealthy and
//!   relaunches through its existing path. The time spent migrating is carried
//!   into boot recovery's clock ([`crate::recovery::Recovery::begin_after`]),
//!   so migrate + recover together are what the bound covers.
//! * **no migration is running** (the store is opening, or on-boot work is
//!   under way) → `GET /health` is answered AT ONCE with 200
//!   `status: "starting"` — the same answer the off-runtime door
//!   ([`Startup::health_answer`]) gives in this phase (R9.C7l: DEPLOY-9k's
//!   1.46 s unannounced store open held every /health probe, so the chassis
//!   looked down for 2.21 s across a planned handoff). `starting` is alive,
//!   not ready: a client that needs ready keys on `status == "ok"`, never on
//!   the code. Every OTHER request WAITS for the chassis (or for a migration
//!   to be announced) and is then served by it. Nothing is ever reported as
//!   `migrating` for a store that needed no migration.
//!
//! Once the chassis router is handed over ([`Startup::ready`]) every request
//! is forwarded to it unchanged — the C7g `recovering` → `ok` path, the MCP
//! door and the WS upgrade are exactly as they were.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::json;
use tower::ServiceExt;

/// `/health`'s `status` while a store migration runs behind the served port.
pub const MIGRATING: &str = "migrating";

/// The pre-chassis state of one daemon boot.
#[derive(Debug)]
pub struct Startup {
    /// `Some((since, step))` while a migration is running.
    migrating: Mutex<Option<(Instant, &'static str)>>,
    /// Total time spent migrating, once finished.
    migrated_ms: AtomicU64,
    stuck_after: Duration,
    chassis: OnceLock<Router>,
    changed: tokio::sync::watch::Sender<u64>,
}

impl Default for Startup {
    fn default() -> Self {
        Self::new()
    }
}

impl Startup {
    pub fn new() -> Self {
        let stuck = std::env::var(crate::recovery::STUCK_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(crate::recovery::DEFAULT_STUCK_MS);
        Startup {
            migrating: Mutex::new(None),
            migrated_ms: AtomicU64::new(0),
            stuck_after: Duration::from_millis(stuck),
            chassis: OnceLock::new(),
            changed: tokio::sync::watch::channel(0).0,
        }
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n += 1);
    }

    /// `Store::open_noticing`'s notice: a migration step is about to run.
    pub fn notice(&self, step: &'static str) {
        let mut m = self.migrating.lock().unwrap_or_else(|e| e.into_inner());
        match m.as_mut() {
            Some((_, s)) => *s = step,
            None => {
                tracing::info!(
                    step,
                    "store migration running behind the served port; /health answers 503 `migrating`"
                );
                *m = Some((Instant::now(), step));
            }
        }
        drop(m);
        self.bump();
    }

    /// The store is open: any migration is over.
    pub fn opened(&self) {
        let mut m = self.migrating.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((since, _)) = m.take() {
            let ms = since.elapsed().as_millis() as u64;
            self.migrated_ms.store(ms, Ordering::SeqCst);
            tracing::info!(elapsed_ms = ms, "store migration done");
        }
        drop(m);
        self.bump();
    }

    /// How long the finished migration took (zero when there was none).
    pub fn migrated_for(&self) -> Duration {
        Duration::from_millis(self.migrated_ms.load(Ordering::SeqCst))
    }

    /// Hand every request from here on to the chassis.
    pub fn ready(&self, chassis: Router) {
        let _ = self.chassis.set(chassis);
        self.bump();
    }

    /// The router `run_daemon` serves from the moment it holds the port.
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new().fallback(answer).with_state(Arc::clone(self))
    }

    /// The answer while migrating, or `None` when no migration is running.
    fn migrating_answer(&self) -> Option<Response> {
        let body = self.migrating_body()?;
        Some((StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response())
    }

    /// R9.C7i: what the off-runtime health door says before the chassis is
    /// built — the migrating answer (503) while a migration runs, else 200
    /// `starting`: the process is alive and opening its store, which is the
    /// same class as `recovering`, never a death.
    pub fn health_answer(&self) -> crate::health_door::Answer {
        match self.migrating_body() {
            Some(b) => (503, b),
            None => (200, json!({ "status": crate::health_door::STARTING })),
        }
    }

    fn migrating_body(&self) -> Option<serde_json::Value> {
        let m = self.migrating.lock().unwrap_or_else(|e| e.into_inner());
        let (since, step) = (*m)?;
        drop(m);
        let elapsed = since.elapsed();
        let stuck = elapsed > self.stuck_after;
        let body = json!({
            "status": if stuck { crate::liveness::RECOVERY_STUCK } else { MIGRATING },
            "why": if stuck {
                "a store migration has run past its bound without finishing"
            } else {
                "threads is migrating its store after a restart; it serves nothing until that is done"
            },
            "migration": {
                "state": if stuck { crate::recovery::STUCK } else { MIGRATING },
                "step": step,
                "elapsed_ms": elapsed.as_millis() as u64,
                "stuck_after_ms": self.stuck_after.as_millis() as u64,
            },
        });
        Some(body)
    }
}

async fn answer(State(s): State<Arc<Startup>>, req: Request) -> Response {
    // Subscribe BEFORE looking, so a change between the look and the wait is
    // not missed.
    let mut rx = s.changed.subscribe();
    loop {
        if let Some(chassis) = s.chassis.get() {
            return match chassis.clone().oneshot(req).await {
                Ok(r) => r,
                Err(never) => match never {},
            };
        }
        if let Some(r) = s.migrating_answer() {
            return r;
        }
        // R9.C7l: /health does not wait for the chassis — the process is
        // alive and opening its store; say so, as the door does.
        if req.method() == axum::http::Method::GET && req.uri().path() == "/health" {
            let (code, body) = s.health_answer();
            let code = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
            return (code, Json(body)).into_response();
        }
        if rx.changed().await.is_err() {
            return (StatusCode::SERVICE_UNAVAILABLE, "threads is shutting down").into_response();
        }
    }
}
