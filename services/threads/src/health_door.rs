//! **`/health` ANSWERED OFF THE RUNTIME** (R9.C7i, implements C-7; DEPLOY-9j F1).
//!
//! ## Why this exists
//!
//! In prod (DEPLOY-9j, an internal report) cauldron's holder read
//! threads as DOWN 85 times with `reading the answer from
//! http://127.0.0.1:3653/health: Resource temporarily unavailable (os error 35)`.
//! That is a read timeout: the kernel completed the connect, the request was
//! written, and no status line came back within the probe's 1.5 s. `/health`
//! itself is cheap; it was not SERVED, because it shares one tokio runtime (and
//! the chassis accept loop) with code that makes synchronous SQLite calls on
//! worker threads. When every worker is parked in one, nothing polls the
//! accept, the `/health` task, or the SIGTERM handler. Three such answers in a
//! row (about 12 s) and the holder SIGTERMed a process that could not hear it,
//! then SIGKILLed it 75 s later.
//!
//! ## What this adds
//!
//! A **door**: a `std::net::TcpListener` on its own OS thread (no tokio),
//! bound right after the chassis port and before the store opens. It answers
//! `GET /health` from memory only:
//!
//! * a **lag stamp** — a task on the daemon's runtime writes the time every
//!   [`STAMP_PERIOD`]; `runtime_lag_ms` is how long ago it last did. A free
//!   runtime reads ~0; a starved one reads exactly how starved it is.
//! * the **last answer the runtime computed** — the same verdict the chassis
//!   `/health` gives (`ok`, `recovering`, `wedged`, `recovery_stuck`,
//!   `migrating`, plus the restart gate and the recovery object), cached by the
//!   stamp task each tick. The door never takes a lock the runtime can hold
//!   across a blocking call.
//!
//! So a slow-but-alive threads answers `200` with its lag, a dead one is
//! REFUSED (the thread dies with the process), and only a lag past the declared
//! bound ([`DEFAULT_STALL_SECS`], [`STALL_ENV`]) answers **503 `stalled`**.
//!
//! ## Where it listens, and why there
//!
//! On [`ADDR_ENV`] — `services/threads.toml` sets it to
//! `[::1]:${CAULDRON_HOSTED_PORT}`: the IPv6 loopback, on the chassis's own
//! port number. A cauldron declaration can expand only the hosted address and
//! port, with no arithmetic, so "the chassis port plus one" cannot be said per
//! environment; the IPv6 loopback can, and it is a different socket from the
//! chassis's `127.0.0.1` one. Unset ⇒ no door (every CLI verb and every test
//! that does not ask for it). A set address that cannot be bound fails the
//! boot: a holder pointed at a door that is not there would read the daemon as
//! dead.
//!
//! ## SIGTERM under starvation
//!
//! The stop handler runs on the same runtime, so a starved daemon does not hear
//! SIGTERM until a worker frees up (35089: 75 s, never logged). The door
//! registers its own SIGTERM flag (after tokio's handler, so tokio's still
//! hears it) and, when the runtime has not acknowledged the stop within
//! [`STOP_HEARD_GRACE`], logs that this stop cannot drain — so the holder's
//! grace before SIGKILL reads as what it is.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Where the door listens. Unset ⇒ no door.
pub const ADDR_ENV: &str = "THREADS_HEALTH_ADDR";

/// Override for [`DEFAULT_STALL_SECS`], in whole seconds (`0` / unparseable ⇒
/// the default). Exists so a test can shrink the bound (doctrine 4).
pub const STALL_ENV: &str = "THREADS_RUNTIME_STALL_SECS";

/// The runtime lag past which the door answers 503 `stalled`: the same 60 s
/// the store heartbeat calls wedged ([`crate::liveness::DEFAULT_STALE_AFTER_SECS`]).
pub const DEFAULT_STALL_SECS: u64 = crate::liveness::DEFAULT_STALE_AFTER_SECS;

/// How often the runtime stamps (and re-computes the cached answer).
pub const STAMP_PERIOD: Duration = Duration::from_millis(250);

/// How long after SIGTERM the runtime may take to acknowledge it before the
/// door logs the stop as unheard.
pub const STOP_HEARD_GRACE: Duration = Duration::from_secs(1);

/// `/health`'s `status` once the runtime lag is past the bound.
pub const STALLED: &str = "stalled";

/// `/health`'s `status` before the store is open and no migration is running.
pub const STARTING: &str = "starting";

/// The log line the door writes when SIGTERM goes unheard (asserted by
/// `tests/health_off_runtime.rs`).
pub const UNHEARD_STOP_LOG: &str = "health door: SIGTERM has not been heard by the runtime";

/// One answer: the HTTP code and the JSON body.
pub type Answer = (u16, Value);

#[derive(Debug)]
struct Shared {
    origin: Instant,
    /// Milliseconds after `origin` of the runtime's last stamp.
    last_stamp_ms: AtomicU64,
    stall_after: Duration,
    /// The answer the runtime computed at its last stamp. Held only to clone
    /// or replace a value — never across anything that can block.
    cached: Mutex<Answer>,
    sigterm: Arc<AtomicBool>,
    stop_heard: AtomicBool,
}

/// The door's handle: the runtime side stamps it, the OS thread reads it.
#[derive(Clone, Debug)]
pub struct Door {
    inner: Arc<Shared>,
    addr: SocketAddr,
}

/// The stall bound from [`STALL_ENV`], else [`DEFAULT_STALL_SECS`].
pub fn stall_from_env() -> Duration {
    let secs = std::env::var(STALL_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_STALL_SECS);
    Duration::from_secs(secs)
}

/// The address [`ADDR_ENV`] names: `Ok(None)` when unset or empty, `Err` when
/// it is not a socket address.
pub fn addr_from_env() -> Result<Option<SocketAddr>, String> {
    match std::env::var(ADDR_ENV) {
        Ok(v) if !v.trim().is_empty() => v
            .trim()
            .parse::<SocketAddr>()
            .map(Some)
            .map_err(|e| format!("{ADDR_ENV}={v:?} is not a socket address ({e})")),
        _ => Ok(None),
    }
}

impl Door {
    /// Bind `addr` and start the door's OS thread.
    pub fn start(addr: SocketAddr, stall_after: Duration) -> std::io::Result<Door> {
        let listener = TcpListener::bind(addr)?;
        let bound = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let door = Door {
            inner: Arc::new(Shared {
                origin: Instant::now(),
                last_stamp_ms: AtomicU64::new(0),
                stall_after,
                cached: Mutex::new((200, json!({ "status": STARTING }))),
                sigterm: Arc::new(AtomicBool::new(false)),
                stop_heard: AtomicBool::new(false),
            }),
            addr: bound,
        };
        let d = door.clone();
        std::thread::Builder::new()
            .name("threads-health-door".into())
            .spawn(move || d.run(listener))?;
        Ok(door)
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stall_after(&self) -> Duration {
        self.inner.stall_after
    }

    fn now_ms(&self) -> u64 {
        self.inner.origin.elapsed().as_millis() as u64
    }

    /// How long ago the runtime last stamped.
    pub fn lag(&self) -> Duration {
        Duration::from_millis(self.now_ms().saturating_sub(self.inner.last_stamp_ms.load(Ordering::SeqCst)))
    }

    /// The runtime side: stamp now, with the answer it computed.
    pub fn stamp(&self, answer: Answer) {
        *self.inner.cached.lock().unwrap_or_else(|e| e.into_inner()) = answer;
        self.inner.last_stamp_ms.store(self.now_ms(), Ordering::SeqCst);
    }

    /// Spawn the stamp task on the CURRENT runtime: every [`STAMP_PERIOD`],
    /// compute the answer and stamp. It is an ordinary task ON PURPOSE — a
    /// runtime that cannot schedule it is the thing being measured.
    pub fn spawn_stamp<F>(&self, compute: F) -> tokio::task::JoinHandle<()>
    where
        F: Fn() -> Answer + Send + Sync + 'static,
    {
        let d = self.clone();
        tokio::spawn(async move {
            loop {
                d.stamp(compute());
                tokio::time::sleep(STAMP_PERIOD).await;
            }
        })
    }

    /// Register the door's own SIGTERM flag. Call AFTER tokio's SIGTERM handler
    /// is installed: from then on a SIGTERM no longer terminates the process by
    /// default, so the door must never be the only listener.
    pub fn watch_sigterm(&self) {
        let flag = Arc::clone(&self.inner.sigterm);
        // SAFETY: the action only stores to an atomic, which is async-signal-safe.
        let r = unsafe {
            signal_hook_registry::register(libc::SIGTERM, move || flag.store(true, Ordering::SeqCst))
        };
        if let Err(e) = r {
            tracing::warn!(error = %e, "health door: could not watch SIGTERM; an unheard stop will not be logged");
        }
    }

    /// The runtime side: the stop signal reached `run_daemon`.
    pub fn stop_heard(&self) {
        self.inner.stop_heard.store(true, Ordering::SeqCst);
    }

    /// What `/health` on the door says right now.
    pub fn answer(&self) -> Answer {
        let lag = self.lag();
        let (mut code, mut body) = self.inner.cached.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !body.is_object() {
            body = json!({});
        }
        let o = body.as_object_mut().expect("made an object above");
        if lag > self.inner.stall_after {
            let last = o.get("status").cloned().unwrap_or(Value::Null);
            o.insert("last_status".into(), last);
            o.insert("status".into(), json!(STALLED));
            o.insert(
                "why".into(),
                json!("the daemon's tokio runtime has not run its stamp task within the declared stall bound"),
            );
            code = 503;
        }
        o.insert("runtime_lag_ms".into(), json!(lag.as_millis() as u64));
        o.insert("stall_after_ms".into(), json!(self.inner.stall_after.as_millis() as u64));
        o.insert("stopping".into(), json!(self.inner.sigterm.load(Ordering::SeqCst)));
        o.insert("served_by".into(), json!("health-door"));
        (code, body)
    }

    fn run(self, listener: TcpListener) {
        let mut sigterm_at: Option<Instant> = None;
        let mut unheard_logged = false;
        loop {
            match listener.accept() {
                Ok((s, _)) => self.serve(s),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "health door: accept failed");
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            if self.inner.sigterm.load(Ordering::SeqCst) && sigterm_at.is_none() {
                sigterm_at = Some(Instant::now());
                tracing::info!(
                    runtime_lag_ms = self.lag().as_millis() as u64,
                    "health door: SIGTERM received"
                );
            }
            if let Some(at) = sigterm_at {
                if !unheard_logged
                    && !self.inner.stop_heard.load(Ordering::SeqCst)
                    && at.elapsed() > STOP_HEARD_GRACE
                {
                    unheard_logged = true;
                    tracing::error!(
                        since_sigterm_ms = at.elapsed().as_millis() as u64,
                        runtime_lag_ms = self.lag().as_millis() as u64,
                        "{UNHEARD_STOP_LOG}: its stop handler runs on the starved runtime, so this stop \
                         will not drain until a worker frees up; a supervisor's grace before SIGKILL is \
                         waiting on a stalled runtime, not on a drain"
                    );
                }
            }
        }
    }

    fn serve(&self, mut s: TcpStream) {
        // An accepted socket inherits O_NONBLOCK from the listener on BSDs.
        let _ = s.set_nonblocking(false);
        let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = s.set_write_timeout(Some(Duration::from_millis(500)));
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && !buf.windows(2).any(|w| w == b"\n\n") && buf.len() < 8192 {
            match s.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let text = String::from_utf8_lossy(&buf);
        let mut first = text.lines().next().unwrap_or("").split_whitespace();
        let (method, target) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
        let path = target.split('?').next().unwrap_or("");
        let (code, body) = if method == "GET" && path == "/health" {
            self.answer()
        } else {
            (404, json!({ "status": "not_found", "why": "the health door answers GET /health only" }))
        };
        let body = body.to_string();
        let reason = match code {
            200 => "OK",
            404 => "Not Found",
            503 => "Service Unavailable",
            _ => "",
        };
        let resp = format!(
            "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = s.write_all(resp.as_bytes());
        let _ = s.flush();
    }
}

/// The verdict the chassis `/health` gives once the chassis is built — the
/// same order as `liveness::guard`: a stale store heartbeat (503 `wedged`),
/// recovery past its bound (503 `recovery_stuck`), recovery running (200
/// `recovering`), else 200 `ok`. Every body carries `extras`.
pub fn ready_answer(
    hb: &crate::liveness::Heartbeat,
    extras: std::collections::BTreeMap<String, Value>,
) -> Answer {
    let state = extras
        .get("recovery")
        .and_then(|r| r.get("state"))
        .and_then(|s| s.as_str())
        .map(str::to_string);
    let (code, mut body) = if hb.is_stale() {
        (
            503,
            json!({
                "status": "wedged",
                "heartbeat_age_ms": hb.age().as_millis() as u64,
                "stale_after_ms": hb.stale_after().as_millis() as u64,
                "why": "the daemon's store-liveness heartbeat has not succeeded within its bound",
            }),
        )
    } else if state.as_deref() == Some(crate::recovery::STUCK) {
        (
            503,
            json!({
                "status": crate::liveness::RECOVERY_STUCK,
                "why": "boot recovery has run past its bound without finishing",
            }),
        )
    } else if state.as_deref() == Some("recovering") {
        (200, json!({ "status": crate::liveness::RECOVERING }))
    } else {
        (200, json!({ "status": "ok" }))
    };
    if let Some(o) = body.as_object_mut() {
        for (k, v) in extras {
            o.entry(k).or_insert(v);
        }
    }
    (code, body)
}
