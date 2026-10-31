//! # The host — wake on demand, at most one awake, no tool stalls another
//!
//! [`ToolHost`] holds the declarations and ONE slot, `slot: Option<Arc<Gen>>`.
//! The at-most-one invariant is that slot's type: there is nowhere to keep a
//! second process. Waking a server empties the slot — killing and reaping
//! what was in it — before it spawns into it, so waking a second server puts
//! the first to sleep.
//!
//! Nothing is spawned at load. A process server is woken by the first
//! [`ToolHost::call`] that names one of its tools (or by an explicit
//! [`ToolHost::wake`]), and stays awake until another server is woken, it is
//! put to sleep, or the host is dropped.
//!
//! ## One stuck tool cannot stall the rest (R5.S2b)
//!
//! R5.S2 held the whole host behind ONE mutex, taken by every seam word for
//! the whole of a call, so a wedged tool (up to [`CALL_TIMEOUT`] plus a
//! handshake) stalled every turn's `tools/list` and every other tool's call.
//! The host now takes `&self` and has no host-wide lock held across I/O:
//!
//! * **Listing takes no lock at all.** The declarations are immutable after
//!   load ([`ToolHost::decls`]), so `MCP_TOOLS` is answered while any tool is
//!   wedged.
//! * **Per-server state.** The awake server's pipe lives in its own `Gen`
//!   (one per spawn), behind its own `io` lock; only callers of THAT server
//!   queue on it. The slot lock and the child lock are held only for a
//!   bookkeeping step, a spawn, or a kill — never across a read.
//! * **A bounded wait, then preemption.** A call for another server waits at
//!   most [`Bounds::preempt`] for the awake one to go idle, then puts it to
//!   sleep regardless (the at-most-one invariant holds; nothing is spawned
//!   until the old process is reaped). The preempted server's in-flight call
//!   notices within [`POLL`] and answers [`CallError::Failed`] — an `isError`
//!   for THAT tool only. A server that hangs on `initialize` is "busy" for
//!   this purpose, so a hung wake is preempted the same way.
//!
//! A healthy tool that is mid-call is NOT put to sleep before the bound: two
//! threads using two different tools take turns, each waiting at most the
//! bound, rather than killing each other.
//!
//! ## The stdio transport
//!
//! A process server speaks MCP's stdio transport: newline-delimited JSON-RPC
//! on stdin/stdout, stderr inherited (so it lands in the supervisor's own
//! log). On wake the host performs `initialize` then
//! `notifications/initialized`; a server that does not answer `initialize`
//! inside [`Bounds::handshake`] is killed and the wake fails. A reader thread
//! owns stdout and forwards lines over a channel, so every read has a timeout.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::decl::{self, Residency, ToolDecl};

/// The MCP revision the host proposes on `initialize`.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// How long a freshly spawned server has to answer `initialize`.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one `tools/call` may take before the server is presumed wedged,
/// put to sleep, and the call refused.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a call for one server waits for ANOTHER, awake and busy, server
/// to go idle before putting it to sleep anyway (R5.S2b).
pub const PREEMPT_AFTER: Duration = Duration::from_secs(10);
/// How often a read in flight checks whether its server was put to sleep
/// under it, and how often a waiting waker re-checks the slot.
pub const POLL: Duration = Duration::from_millis(25);

/// The host's time bounds. [`Bounds::default`] is the production set; tests
/// shrink them (doctrine 4: a bound a test outlasts must be injectable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub handshake: Duration,
    pub call: Duration,
    pub preempt: Duration,
}

impl Default for Bounds {
    fn default() -> Self {
        Self { handshake: HANDSHAKE_TIMEOUT, call: CALL_TIMEOUT, preempt: PREEMPT_AFTER }
    }
}

impl Bounds {
    /// The longest a single `MCP_CALL` can take end to end: wait out a busy
    /// server, wake ours, serve the call. A remote caller's timeout must
    /// exceed this so the host's typed refusal arrives first.
    pub fn worst_call(&self) -> Duration {
        self.preempt + self.handshake + self.call
    }
}

/// One spawn of one server: its process and its pipe, each behind its own
/// lock, so killing it never waits on a read and reading it never blocks the
/// slot.
struct Gen {
    name: String,
    pid: u32,
    woken_at: u64,
    /// Held only for `try_wait` / `kill` + `wait`.
    child: Mutex<Child>,
    /// Held across ONE request/response with this server. Only callers of
    /// this server ever wait on it.
    io: Mutex<Io>,
    /// Callers holding or queued on `io`, the waker's handshake included. A
    /// waker for another server waits for this to reach 0, up to the bound.
    busy: AtomicUsize,
    /// `false` once this spawn is put to sleep; a read in flight sees it
    /// within [`POLL`].
    alive: AtomicBool,
}

struct Io {
    stdin: ChildStdin,
    rx: Receiver<String>,
    next_id: u64,
}

/// A caller's claim on a `Gen`: counted in `busy` for as long as it lives.
struct Claim(Arc<Gen>);

impl Drop for Claim {
    fn drop(&mut self) {
        self.0.busy.fetch_sub(1, Ordering::SeqCst);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic mid-call poisons a lock. The state behind it is still the one
    // slot / one pipe; recover it rather than refusing every future call.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Gen {
    fn exited(&self) -> bool {
        matches!(lock(&self.child).try_wait(), Ok(Some(_)) | Err(_))
    }
    /// Kill and reap. After this returns the process is gone, not signalled.
    fn kill(&self) {
        self.alive.store(false, Ordering::SeqCst);
        let mut c = lock(&self.child);
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// What a wake did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wake {
    /// The server now awake.
    pub name: String,
    pub pid: u32,
    /// `false` when it was already awake and nothing was spawned.
    pub spawned: bool,
    /// The server that was put to sleep to make room, if any.
    pub slept: Option<String>,
}

/// Why a call was not served, as a closed set the wire renders with a code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// No declaration names this tool.
    UnknownTool(String),
    /// The tool is ours and is served in-process by `provider`, never by a
    /// process the supervisor wakes.
    InProcess { tool: String, server: String, provider: String },
    /// The server could not be woken, or died, wedged or was preempted
    /// mid-call.
    Failed(String),
    /// The server answered with a JSON-RPC error.
    Rpc { code: i64, message: String },
}

impl CallError {
    pub fn code(&self) -> &'static str {
        match self {
            CallError::UnknownTool(_) => "unknown_tool",
            CallError::InProcess { .. } => "in_process",
            CallError::Failed(_) => "tool_failed",
            CallError::Rpc { .. } => "tool_error",
        }
    }
    pub fn message(&self) -> String {
        match self {
            CallError::UnknownTool(t) => format!("no MCP tool declaration names `{t}`"),
            CallError::InProcess { tool, server, provider } => format!(
                "`{tool}` is declared by `{server}` as in-process: it is served by `{provider}`'s own \
                 code, never by a process the supervisor wakes"
            ),
            CallError::Failed(m) => m.clone(),
            CallError::Rpc { code, message } => format!("the tool server answered error {code}: {message}"),
        }
    }
}

/// The supervisor's MCP tool host. Shared as `&ToolHost` (or `Arc`) by every
/// seam connection; there is no outer lock.
pub struct ToolHost {
    /// Immutable after `new`: listing reads it with no lock.
    decls: BTreeMap<String, ToolDecl>,
    bounds: Bounds,
    /// THE ONE AWAKE SLOT. Never held across I/O.
    slot: Mutex<Option<Arc<Gen>>>,
    /// Every spawn this host has made, for `MCP_STATUS`.
    wakes: AtomicU64,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl ToolHost {
    /// Hold `decls` under the production [`Bounds`]. Spawns NOTHING.
    pub fn new(decls: Vec<ToolDecl>) -> Result<Self> {
        Self::with_bounds(decls, Bounds::default())
    }

    /// Hold `decls` under `bounds`. Spawns NOTHING — waking is on demand.
    pub fn with_bounds(decls: Vec<ToolDecl>, bounds: Bounds) -> Result<Self> {
        decl::check_unique_tools(&decls)?;
        Ok(Self {
            decls: decls.into_iter().map(|d| (d.name.clone(), d)).collect(),
            bounds,
            slot: Mutex::new(None),
            wakes: AtomicU64::new(0),
        })
    }

    pub fn bounds(&self) -> Bounds {
        self.bounds
    }

    pub fn decls(&self) -> impl Iterator<Item = &ToolDecl> {
        self.decls.values()
    }

    /// The server that declares `tool`.
    pub fn owner_of(&self, tool: &str) -> Option<&ToolDecl> {
        self.decls.values().find(|d| d.tools.iter().any(|t| t.name == tool))
    }

    /// Empty the slot if its occupant exited by itself (reaping it).
    fn reap_dead(slot: &mut Option<Arc<Gen>>) {
        if slot.as_ref().is_some_and(|g| g.exited()) {
            if let Some(g) = slot.take() {
                g.alive.store(false, Ordering::SeqCst);
            }
        }
    }

    /// The awake server and its pid, if one is awake AND still running. A
    /// server that exited by itself is reaped here and reported asleep.
    pub fn awake(&self) -> Option<(String, u32)> {
        let mut slot = lock(&self.slot);
        Self::reap_dead(&mut slot);
        slot.as_ref().map(|g| (g.name.clone(), g.pid))
    }

    pub fn wakes(&self) -> u64 {
        self.wakes.load(Ordering::SeqCst)
    }

    /// **PUT THE AWAKE SERVER TO SLEEP**: kill it and reap it. Returns its
    /// name. After this returns the process is gone, not merely signalled. A
    /// call in flight to it answers `Failed` within [`POLL`].
    pub fn sleep(&self) -> Option<String> {
        let mut slot = lock(&self.slot);
        let g = slot.take()?;
        g.kill();
        Some(g.name.clone())
    }

    /// Put `g` to sleep if it is still the occupant; kill it either way.
    fn sleep_gen(&self, g: &Arc<Gen>) {
        let mut slot = lock(&self.slot);
        if slot.as_ref().is_some_and(|s| Arc::ptr_eq(s, g)) {
            *slot = None;
        }
        // Killed while the slot is held, so no waker can spawn before it is
        // reaped.
        g.kill();
    }

    /// **WAKE `server`**, putting whatever else is awake to sleep first.
    pub fn wake(&self, server: &str) -> Result<Wake> {
        self.claim(server).map(|(_, w)| w)
    }

    /// Wake `server` (or find it awake) and return a claim on it, counted
    /// busy, so no other waker preempts it before the bound.
    fn claim(&self, server: &str) -> Result<(Claim, Wake)> {
        let d = self.decls.get(server).ok_or_else(|| anyhow!("no MCP tool declaration is named `{server}`"))?;
        if d.residency != Residency::Process {
            bail!(
                "`{server}` is in-process (provider `{}`); there is no process to wake",
                d.provider.as_deref().unwrap_or("?")
            );
        }
        // WHICH occupant was first seen busy, and when; it is preempted once
        // this is `bounds.preempt` old. Keyed by the occupant (its pid: one
        // per spawn) so the clock RESTARTS when the occupant changes: a waiter
        // that outlasted a stuck server must still give the server woken in
        // its place its full grace (R5.S2b inspect finding 1 — with two
        // waiters, the second killed the first's fresh spawn at once).
        let mut busy_since: Option<(u32, Instant)> = None;
        loop {
            let mut slot = lock(&self.slot);
            Self::reap_dead(&mut slot);
            let mut slept = None;
            if let Some(g) = slot.as_ref() {
                if g.name == server {
                    g.busy.fetch_add(1, Ordering::SeqCst);
                    let w = Wake { name: g.name.clone(), pid: g.pid, spawned: false, slept: None };
                    return Ok((Claim(g.clone()), w));
                }
                if g.busy.load(Ordering::SeqCst) > 0 {
                    let since = match busy_since {
                        Some((who, at)) if who == g.pid => at,
                        _ => busy_since.insert((g.pid, Instant::now())).1,
                    };
                    if since.elapsed() < self.bounds.preempt {
                        // Wait for it to go idle — WITHOUT holding the slot.
                        drop(slot);
                        std::thread::sleep(POLL);
                        continue;
                    }
                    // Outlasted the bound: preempted below; its in-flight
                    // call answers Failed for that tool only.
                }
                // THE AT-MOST-ONE LINE. The slot is emptied — the previous
                // server killed and reaped — BEFORE anything is spawned.
                let old = slot.take().expect("checked Some");
                old.kill();
                slept = Some(old.name.clone());
            }
            return self.spawn_into(slot, d, slept);
        }
    }

    /// Spawn `d` into the (empty, held) slot and complete its handshake —
    /// the handshake AFTER the slot is released, under the new spawn's own
    /// `io` lock, so a server that hangs on `initialize` holds nobody else.
    fn spawn_into(
        &self,
        mut slot: MutexGuard<'_, Option<Arc<Gen>>>,
        d: &ToolDecl,
        slept: Option<String>,
    ) -> Result<(Claim, Wake)> {
        let server = d.name.as_str();
        let p = d.process.as_ref().expect("validated: a process server has [process]");
        let mut child = Command::new(&p.command)
            .args(&p.args)
            .envs(&p.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning MCP tool server `{server}` ({})", p.command))?;
        self.wakes.fetch_add(1, Ordering::SeqCst);
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let pid = child.id();
        let g = Arc::new(Gen {
            name: server.to_string(),
            pid,
            woken_at: now(),
            child: Mutex::new(child),
            io: Mutex::new(Io { stdin, rx, next_id: 1 }),
            busy: AtomicUsize::new(1),
            alive: AtomicBool::new(true),
        });
        let claim = Claim(g.clone());
        // Take our own pipe BEFORE publishing the spawn, so a second caller
        // of this server queues behind the handshake instead of racing it.
        let mut io = lock(&g.io);
        *slot = Some(g.clone());
        // Release the slot: from here on nothing host-wide is held.
        drop(slot);
        let hello = self.rpc(
            &g,
            &mut io,
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "cauldron-supervisor", "version": env!("CARGO_PKG_VERSION")},
            }),
            self.bounds.handshake,
        );
        if let Err(e) = hello {
            return Err(anyhow!("MCP tool server `{server}` did not complete initialize: {}", e.message()));
        }
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let _ = writeln!(io.stdin, "{note}").and_then(|_| io.stdin.flush());
        drop(io);
        Ok((claim, Wake { name: server.to_string(), pid, spawned: true, slept }))
    }

    /// **SERVE ONE `tools/call`**: find the server that declares `tool`, wake
    /// it if it is not the one awake, and forward the call. Returns the
    /// server's `CallToolResult` as it sent it.
    pub fn call(&self, tool: &str, arguments: Value) -> std::result::Result<Value, CallError> {
        let d = self.owner_of(tool).ok_or_else(|| CallError::UnknownTool(tool.to_string()))?;
        if d.residency == Residency::InProcess {
            return Err(CallError::InProcess {
                tool: tool.to_string(),
                server: d.name.clone(),
                provider: d.provider.clone().unwrap_or_default(),
            });
        }
        let server = d.name.clone();
        let (claim, _) = self.claim(&server).map_err(|e| CallError::Failed(format!("{e:#}")))?;
        let g = &claim.0;
        let mut io = lock(&g.io);
        if !g.alive.load(Ordering::SeqCst) {
            return Err(CallError::Failed(format!(
                "MCP tool server `{server}` was put to sleep before this call reached it"
            )));
        }
        self.rpc(g, &mut io, "tools/call", json!({"name": tool, "arguments": arguments}), self.bounds.call)
    }

    /// One request/response with `g`. A transport failure (the server died,
    /// wedged, wrote garbage, or was preempted) puts it to sleep: it is in an
    /// unknown state and the next call should start it clean.
    fn rpc(
        &self,
        g: &Arc<Gen>,
        io: &mut Io,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> std::result::Result<Value, CallError> {
        let r = Self::rpc_inner(g, io, method, params, timeout);
        if matches!(r, Err(CallError::Failed(_))) {
            self.sleep_gen(g);
        }
        r
    }

    fn rpc_inner(
        g: &Gen,
        io: &mut Io,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> std::result::Result<Value, CallError> {
        let id = io.next_id;
        io.next_id += 1;
        // `id` first: a stdio server reading lines sees the id before params.
        let req = json!({"id": id, "jsonrpc": "2.0", "method": method, "params": params});
        writeln!(io.stdin, "{req}")
            .and_then(|_| io.stdin.flush())
            .map_err(|e| CallError::Failed(format!("writing to MCP tool server `{}`: {e}", g.name)))?;
        let deadline = Instant::now() + timeout;
        let preempted = || {
            CallError::Failed(format!(
                "MCP tool server `{}` was put to sleep during `{method}` (another tool needed the \
                 one awake slot and this one did not finish within the preempt bound)",
                g.name
            ))
        };
        loop {
            if !g.alive.load(Ordering::SeqCst) {
                return Err(preempted());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(CallError::Failed(format!(
                    "MCP tool server `{}` did not answer `{method}` within {}s",
                    g.name,
                    timeout.as_secs_f64()
                )));
            }
            let line = match io.rx.recv_timeout(left.min(POLL)) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => continue,
                // Killed by a preemptor: its stdout closes, often before
                // this read sees `alive` drop. Say which it was.
                Err(RecvTimeoutError::Disconnected) if !g.alive.load(Ordering::SeqCst) => return Err(preempted()),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(CallError::Failed(format!(
                        "MCP tool server `{}` closed its stdout during `{method}`",
                        g.name
                    )))
                }
            };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                // Not JSON: a server logging to stdout. Skipped, not fatal.
                continue;
            };
            if msg.get("id") != Some(&json!(id)) {
                // A notification, or a request from the server: this host
                // offers no client capabilities, so it is not answered.
                continue;
            }
            if let Some(e) = msg.get("error") {
                return Err(CallError::Rpc {
                    code: e.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: e.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
                });
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// The host's state as one JSON object. Never waits on a tool.
    pub fn status(&self) -> Value {
        let awake = {
            let mut slot = lock(&self.slot);
            Self::reap_dead(&mut slot);
            slot.as_ref().map(|g| {
                json!({"server": g.name, "pid": g.pid, "woken_at": g.woken_at, "busy": g.busy.load(Ordering::SeqCst)})
            })
        };
        json!({
            "awake": awake,
            "wakes": self.wakes(),
            "bounds_ms": {
                "handshake": self.bounds.handshake.as_millis() as u64,
                "call": self.bounds.call.as_millis() as u64,
                "preempt": self.bounds.preempt.as_millis() as u64,
            },
            "declared": self.decls.values().map(|d| json!({
                "server": d.name,
                "residency": match d.residency { Residency::InProcess => "in-process", Residency::Process => "process" },
                "provider": d.provider,
                "tools": d.tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>(),
                "source": d.source.display().to_string(),
            })).collect::<Vec<_>>(),
        })
    }
}

impl Drop for ToolHost {
    /// The supervisor exiting takes its awake tool with it: a tool process is
    /// never left running unmanaged.
    fn drop(&mut self) {
        self.sleep();
    }
}
